use anyhow::{anyhow, Result};
use futures::StreamExt;
use k8s_openapi::api::coordination::v1::Lease;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{MicroTime, ObjectMeta};
use kube::{
    api::{Api, Patch, PatchParams, PostParams},
    runtime::{watcher, WatchStreamExt},
    Client,
};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

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
    active_replicas: Arc<RwLock<HashSet<String>>>,
}

impl MembershipManager {
    /// Create new membership manager
    ///
    /// # Arguments
    /// * `client` - Kubernetes client
    /// * `my_replica_id` - Unique ID for this replica (POD_UID)
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
            active_replicas: Arc::new(RwLock::new(HashSet::new())),
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

        let now = MicroTime(chrono::Utc::now());

        let lease = Lease {
            metadata: ObjectMeta {
                name: Some(lease_name.clone()),
                namespace: Some(self.namespace.clone()),
                labels: Some(
                    [
                        ("app".to_string(), "external-pod-autoscaler".to_string()),
                        ("replica-id".to_string(), self.my_replica_id.clone()),
                    ]
                    .into(),
                ),
                ..Default::default()
            },
            spec: Some(k8s_openapi::api::coordination::v1::LeaseSpec {
                holder_identity: Some(holder_identity),
                lease_duration_seconds: Some(30), // 3x renewal interval
                renew_time: Some(now.clone()),
                acquire_time: Some(now),
                lease_transitions: None,
            }),
        };

        // Try to create, if exists then update
        match lease_api.create(&PostParams::default(), &lease).await {
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
                        &PatchParams::apply("external-pod-autoscaler"),
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

        let now = chrono::Utc::now();
        let mut active = HashSet::new();

        for lease in leases.items {
            if let Some(spec) = lease.spec {
                // Check if lease is still valid (within lease duration)
                if let (Some(renew_time), Some(duration)) =
                    (spec.renew_time, spec.lease_duration_seconds)
                {
                    let expires_at = renew_time.0 + chrono::Duration::seconds(duration as i64);

                    if now < expires_at {
                        // Extract replica ID from lease name (epa-replica-{id})
                        if let Some(name) = lease.metadata.name {
                            if let Some(replica_id) = name.strip_prefix("epa-replica-") {
                                active.insert(replica_id.to_string());
                            }
                        }
                    }
                }
            }
        }

        debug!(
            active_count = active.len(),
            replicas = ?active,
            "Updated active replicas"
        );

        let mut replicas = self.active_replicas.write().await;
        *replicas = active;

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
        let replicas = self.active_replicas.read().await;
        replicas.iter().cloned().collect()
    }

    /// Get this replica's ID
    pub fn my_replica_id(&self) -> &str {
        &self.my_replica_id
    }
}
