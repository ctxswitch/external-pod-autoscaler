use anyhow::{anyhow, Result};
use futures::StreamExt;
use k8s_openapi::api::coordination::v1::Lease;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{MicroTime, ObjectMeta, OwnerReference};
use kube::{
    api::{Api, DeleteParams, Patch, PatchParams, PostParams},
    runtime::{watcher, WatchStreamExt},
    Client,
};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

const FIELD_MANAGER: &str = "external-pod-autoscaler";

/// Get current time as a jiff Timestamp for k8s-openapi v0.27+
fn jiff_now() -> k8s_openapi::jiff::Timestamp {
    k8s_openapi::jiff::Timestamp::now()
}

// Both sets and the local draining flag are always updated together under
// the replica_sets RwLock to give readers a consistent snapshot.
struct ReplicaSets {
    active: HashSet<String>,
    draining: HashSet<String>,
    local_draining: bool,
}

/// Manages replica membership using Kubernetes Lease API
///
/// Each replica creates a Lease with its pod IP in holderIdentity.
/// Watches all leases to maintain view of active replicas.
pub struct MembershipManager {
    client: Client,
    my_replica_id: String,
    my_pod_ip: String,
    my_pod_name: String,
    namespace: String,
    webhook_port: u16,
    replica_sets: Arc<RwLock<ReplicaSets>>,
}

impl MembershipManager {
    /// Create new membership manager
    ///
    /// # Arguments
    /// * `client` - Kubernetes client
    /// * `my_replica_id` - Unique ID for this replica (POD_UID). Also used as the
    ///   Pod's UID in the lease `ownerReference` for garbage collection.
    /// * `my_pod_ip` - IP address of this pod (POD_IP)
    /// * `my_pod_name` - Name of this pod (POD_NAME)
    /// * `namespace` - Namespace this replica runs in (POD_NAMESPACE)
    /// * `webhook_port` - Port the webhook server listens on
    pub fn new(
        client: Client,
        my_replica_id: String,
        my_pod_ip: String,
        my_pod_name: String,
        namespace: String,
        webhook_port: u16,
    ) -> Self {
        Self {
            client,
            my_replica_id,
            my_pod_ip,
            my_pod_name,
            namespace,
            webhook_port,
            replica_sets: Arc::new(RwLock::new(ReplicaSets {
                active: HashSet::new(),
                draining: HashSet::new(),
                local_draining: false,
            })),
        }
    }

    /// Run membership manager (lease renewal + discovery)
    pub async fn run(self: Arc<Self>) -> Result<()> {
        info!(
            replica_id = %self.my_replica_id,
            pod_name = %self.my_pod_name,
            pod_ip = %self.my_pod_ip,
            "Starting membership manager"
        );

        // Run renewal and discovery concurrently
        tokio::select! {
            result = self.clone().renewal_loop() => {
                warn!("Lease renewal loop stopped: {:?}", result);
                result
            },
            result = self.discovery_loop() => {
                warn!("Lease discovery loop stopped: {:?}", result);
                result
            },
        }
    }

    /// Renewal loop: create/update our lease every 10s
    async fn renewal_loop(self: Arc<Self>) -> Result<()> {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            interval.tick().await;

            if let Err(e) = self.register_and_renew().await {
                warn!(
                    error = %e,
                    replica_id = %self.my_replica_id,
                    "Failed to renew lease"
                );
            }
        }
    }

    /// Create or update our lease with current IP
    pub(crate) async fn register_and_renew(&self) -> Result<()> {
        let lease_api: Api<Lease> = Api::namespaced(self.client.clone(), &self.namespace);
        let lease_name = format!("epa-replica-{}", self.my_replica_id);

        // Store IP:port in holderIdentity for request forwarding
        let holder_identity = format!("{}:{}", self.my_pod_ip, self.webhook_port);

        let now = MicroTime(jiff_now());

        // Read the draining flag under the lock, then drop the lock before
        // making the API call.
        let is_draining = {
            let sets = self.replica_sets.read().await;
            sets.local_draining
        };

        let mut labels = BTreeMap::from([
            ("app".to_string(), "external-pod-autoscaler".to_string()),
            ("replica-id".to_string(), self.my_replica_id.clone()),
        ]);

        if is_draining {
            labels.insert("draining".to_string(), "true".to_string());
        }

        let lease = Lease {
            metadata: ObjectMeta {
                name: Some(lease_name.clone()),
                namespace: Some(self.namespace.clone()),
                labels: Some(labels),
                owner_references: Some(vec![OwnerReference {
                    api_version: "v1".to_string(),
                    kind: "Pod".to_string(),
                    name: self.my_pod_name.clone(),
                    uid: self.my_replica_id.clone(),
                    controller: None,
                    block_owner_deletion: None,
                }]),
                ..Default::default()
            },
            spec: Some(k8s_openapi::api::coordination::v1::LeaseSpec {
                holder_identity: Some(holder_identity),
                lease_duration_seconds: Some(30), // 3x renewal interval
                renew_time: Some(now.clone()),
                acquire_time: Some(now),
                lease_transitions: None,
                preferred_holder: None,
                strategy: None,
            }),
        };

        // Try to create, if exists then update
        let create_params = PostParams {
            field_manager: Some(FIELD_MANAGER.to_string()),
            ..Default::default()
        };
        match lease_api.create(&create_params, &lease).await {
            Ok(_) => {
                info!(
                    replica_id = %self.my_replica_id,
                    lease_name = %lease_name,
                    "Created lease"
                );
            }
            Err(kube::Error::Api(ae)) if ae.code == 409 => {
                // Lease exists, update it
                lease_api
                    .patch(
                        &lease_name,
                        &PatchParams::apply(FIELD_MANAGER).force(),
                        &Patch::Apply(&lease),
                    )
                    .await?;

                debug!(
                    replica_id = %self.my_replica_id,
                    lease_name = %lease_name,
                    "Renewed lease"
                );
            }
            Err(e) => return Err(e.into()),
        }

        Ok(())
    }

    /// Discovery loop: watch leases for real-time updates
    async fn discovery_loop(&self) -> Result<()> {
        let lease_api: Api<Lease> = Api::namespaced(self.client.clone(), &self.namespace);

        // Watch leases with our prefix
        let watcher_config = watcher::Config::default().labels("app=external-pod-autoscaler");

        let mut lease_stream = watcher(lease_api, watcher_config)
            .default_backoff()
            .applied_objects()
            .boxed();

        info!("Starting lease watcher for replica discovery");

        while let Some(event) = lease_stream.next().await {
            match event {
                Ok(_lease) => {
                    // Refresh active replicas on each lease event
                    if let Err(e) = self.update_active_replicas().await {
                        warn!(error = %e, "Failed to update active replicas");
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Lease watch error");
                }
            }
        }

        warn!("Lease watcher stopped");
        Err(anyhow!("Lease watcher unexpectedly stopped"))
    }

    /// Update active replicas list from current leases
    pub(crate) async fn update_active_replicas(&self) -> Result<()> {
        let lease_api: Api<Lease> = Api::namespaced(self.client.clone(), &self.namespace);

        // List all leases with our label
        let leases = lease_api
            .list(&kube::api::ListParams::default().labels("app=external-pod-autoscaler"))
            .await?;

        let now = jiff_now();
        let mut active = HashSet::new();
        let mut draining = HashSet::new();

        for lease in leases.items {
            if let Some(spec) = &lease.spec {
                // Check if lease is still valid (within lease duration)
                if let (Some(renew_time), Some(duration)) =
                    (&spec.renew_time, spec.lease_duration_seconds)
                {
                    let Ok(duration_u64) = u64::try_from(duration) else {
                        warn!(
                            lease_name = ?lease.metadata.name,
                            duration,
                            "Skipping lease with negative lease_duration_seconds"
                        );
                        continue;
                    };
                    // Clamp to a sane maximum to avoid overflow in timestamp arithmetic.
                    let clamped = duration_u64.min(86400);
                    let expires_at = renew_time.0 + std::time::Duration::from_secs(clamped);

                    if now < expires_at {
                        // Extract replica ID from lease name (epa-replica-{id})
                        if let Some(name) = &lease.metadata.name {
                            if let Some(replica_id) = name.strip_prefix("epa-replica-") {
                                let is_draining = lease
                                    .metadata
                                    .labels
                                    .as_ref()
                                    .and_then(|l| l.get("draining"))
                                    .is_some_and(|v| v == "true");

                                if is_draining {
                                    draining.insert(replica_id.to_string());
                                } else {
                                    active.insert(replica_id.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        debug!(
            active_count = active.len(),
            draining_count = draining.len(),
            replicas = ?active,
            draining_replicas = ?draining,
            "Updated replica sets"
        );

        // Apply local override and write all state atomically under the lock.
        let mut sets = self.replica_sets.write().await;

        if sets.local_draining {
            active.remove(&self.my_replica_id);
            draining.insert(self.my_replica_id.clone());
        }

        sets.active = active;
        sets.draining = draining;

        Ok(())
    }

    /// Get the network address (IP:port) of a replica by ID (for request forwarding)
    pub async fn get_replica_address(&self, replica_id: &str) -> Result<String> {
        let lease_api: Api<Lease> = Api::namespaced(self.client.clone(), &self.namespace);
        let lease_name = format!("epa-replica-{}", replica_id);

        let lease = lease_api
            .get(&lease_name)
            .await
            .map_err(|e| anyhow!("Failed to get lease for replica {}: {}", replica_id, e))?;

        let address = lease
            .spec
            .and_then(|s| s.holder_identity)
            .ok_or_else(|| anyhow!("Lease {} missing holder identity", lease_name))?;

        Ok(address)
    }

    /// Get list of all active replica IDs
    pub async fn get_active_replicas(&self) -> Vec<String> {
        let sets = self.replica_sets.read().await;
        sets.active.iter().cloned().collect()
    }

    /// Returns the union of active and draining replica IDs as a single atomic snapshot.
    ///
    /// The order of elements in the returned `Vec` is not defined; callers must
    /// not rely on position. The active and draining sets are disjoint, so no
    /// replica ID appears more than once.
    #[allow(dead_code)]
    pub(crate) async fn get_all_replicas(&self) -> Vec<String> {
        let sets = self.replica_sets.read().await;
        debug_assert!(
            sets.active.is_disjoint(&sets.draining),
            "active and draining sets must be disjoint"
        );
        sets.active
            .iter()
            .chain(sets.draining.iter())
            .cloned()
            .collect()
    }

    /// Get list of all draining replica IDs
    #[allow(dead_code)]
    pub(crate) async fn get_draining_replicas(&self) -> Vec<String> {
        let sets = self.replica_sets.read().await;
        sets.draining.iter().cloned().collect()
    }

    /// Marks this replica as draining by setting the local flag and immediately
    /// renewing the lease with a `draining=true` label.
    ///
    /// After this call, other replicas observing the lease will classify this
    /// replica as draining rather than active.
    ///
    /// The flag is set under the lock before the API call. If the immediate
    /// lease renewal fails, the renewal loop will propagate the draining
    /// label on its next tick.
    #[allow(dead_code)]
    pub(crate) async fn mark_draining(&self) {
        {
            let mut sets = self.replica_sets.write().await;
            sets.local_draining = true;
        }
        if let Err(e) = self.register_and_renew().await {
            warn!(
                error = %e,
                replica_id = %self.my_replica_id,
                "Draining flag set locally; failed to propagate label immediately — will retry on next renewal tick"
            );
            return;
        }
        info!(
            replica_id = %self.my_replica_id,
            "Marked replica as draining and propagated label"
        );
    }

    /// Deletes this replica's lease from the Kubernetes namespace.
    ///
    /// This is a hard removal; unlike `mark_draining`, the lease object is
    /// destroyed. Callers should call `mark_draining` first to allow graceful
    /// handoff.
    ///
    /// # Errors
    /// Returns an error if the Kubernetes API delete request fails with a
    /// non-404 status. A missing lease (404) is treated as success (idempotent).
    #[allow(dead_code)]
    pub(crate) async fn delete_lease(&self) -> Result<()> {
        // Set draining unconditionally so the renewal loop cannot re-create
        // the lease as active if it fires between the delete and process exit.
        {
            let mut sets = self.replica_sets.write().await;
            sets.local_draining = true;
        }

        let lease_api: Api<Lease> = Api::namespaced(self.client.clone(), &self.namespace);
        let lease_name = format!("epa-replica-{}", self.my_replica_id);

        match lease_api
            .delete(&lease_name, &DeleteParams::default())
            .await
        {
            Ok(_) => {
                info!(
                    replica_id = %self.my_replica_id,
                    lease_name = %lease_name,
                    "Deleted lease"
                );
            }
            Err(kube::Error::Api(ae)) if ae.code == 404 => {
                debug!(
                    replica_id = %self.my_replica_id,
                    lease_name = %lease_name,
                    "Lease already absent; delete is a no-op"
                );
                // Do not reset local_draining — a concurrent mark_draining
                // may have set it and expects it to persist.
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        }

        // Do not reset local_draining. The process is shutting down and the
        // flag must remain set so any renewal_loop tick that fires before
        // process exit re-creates the lease with draining=true.
        Ok(())
    }

    /// Get this replica's ID
    pub fn my_replica_id(&self) -> &str {
        &self.my_replica_id
    }

    /// Sets the local draining flag without making an API call.
    /// Test-only method for simulating watch lag.
    #[cfg(test)]
    pub(crate) async fn set_draining_for_test(&self) {
        let mut sets = self.replica_sets.write().await;
        sets.local_draining = true;
    }
}
