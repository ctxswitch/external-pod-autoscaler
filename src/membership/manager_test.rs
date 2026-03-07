use std::collections::BTreeMap;

use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{MicroTime, ObjectMeta};
use kube::api::Api;
use kube_fake_client::ClientBuilder;

use crate::membership::manager::MembershipManager;

// Build a Lease pre-seeded in the fake client with the given name, namespace,
// holderIdentity, renew_time, and lease_duration_seconds.
// Leases need dynamic timestamps so they remain inline rather than YAML fixtures.
fn make_lease(
    name: &str,
    namespace: &str,
    holder_identity: &str,
    renew_time: k8s_openapi::jiff::Timestamp,
    lease_duration_seconds: i32,
) -> Lease {
    Lease {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(namespace.to_string()),
            labels: Some([("app".to_string(), "external-pod-autoscaler".to_string())].into()),
            ..Default::default()
        },
        spec: Some(LeaseSpec {
            holder_identity: Some(holder_identity.to_string()),
            lease_duration_seconds: Some(lease_duration_seconds),
            renew_time: Some(MicroTime(renew_time)),
            acquire_time: Some(MicroTime(renew_time)),
            lease_transitions: None,
            preferred_holder: None,
            strategy: None,
        }),
    }
}

// register_and_renew on a fresh client creates a Lease with the correct labels
// and holderIdentity (pod_ip:webhook_port), and lease_duration_seconds of 30.
#[tokio::test]
async fn register_creates_lease() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_id = "replica-abc";
    let pod_ip = "10.0.0.1";
    let pod_name = "epa-pod-abc";
    let webhook_port: u16 = 8443;

    let client = ClientBuilder::new().build().await?;

    let manager = MembershipManager::new(
        client.clone(),
        replica_id.to_string(),
        pod_ip.to_string(),
        pod_name.to_string(),
        namespace.to_string(),
        webhook_port,
    );

    manager.register_and_renew().await?;

    let lease_api: Api<Lease> = Api::namespaced(client, namespace);
    let expected_lease_name = format!("epa-replica-{}", replica_id);
    let lease = lease_api.get(&expected_lease_name).await?;

    let labels = lease
        .metadata
        .labels
        .as_ref()
        .ok_or("lease should have labels")?;
    assert_eq!(
        labels.get("app").map(String::as_str),
        Some("external-pod-autoscaler"),
        "lease should have app=external-pod-autoscaler label"
    );
    assert_eq!(
        labels.get("replica-id").map(String::as_str),
        Some(replica_id),
        "lease should carry the replica-id label"
    );

    let spec = lease.spec.as_ref().ok_or("lease should have spec")?;

    let expected_identity = format!("{}:{}", pod_ip, webhook_port);
    assert_eq!(
        spec.holder_identity.as_deref(),
        Some(expected_identity.as_str()),
        "holderIdentity should be pod_ip:webhook_port"
    );

    assert_eq!(
        spec.lease_duration_seconds,
        Some(30),
        "lease_duration_seconds should be 30"
    );

    assert!(
        spec.renew_time.is_some(),
        "renew_time should be set after registration"
    );

    Ok(())
}

// Calling register_and_renew when a lease already exists should not error and
// should leave the lease present with updated metadata (patch path / 409 branch).
#[tokio::test]
async fn register_renews_existing_lease() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_id = "replica-xyz";
    let pod_ip = "10.0.0.2";
    let pod_name = "epa-pod-xyz";
    let webhook_port: u16 = 8443;

    let lease_name = format!("epa-replica-{}", replica_id);
    let old_time = k8s_openapi::jiff::Timestamp::now() - std::time::Duration::from_secs(60);

    // Pre-seed an existing lease so the create path hits 409.
    let existing_lease = make_lease(
        &lease_name,
        namespace,
        &format!("{}:{}", pod_ip, webhook_port),
        old_time,
        30,
    );

    let client = ClientBuilder::new()
        .with_object(existing_lease)
        .build()
        .await?;

    let manager = MembershipManager::new(
        client.clone(),
        replica_id.to_string(),
        pod_ip.to_string(),
        pod_name.to_string(),
        namespace.to_string(),
        webhook_port,
    );

    // Must not error — should fall through to the patch (409) branch.
    manager.register_and_renew().await?;

    // The lease should still exist after the renewal.
    let lease_api: Api<Lease> = Api::namespaced(client, namespace);
    let lease = lease_api.get(&lease_name).await?;

    let spec = lease
        .spec
        .as_ref()
        .ok_or("lease should have spec after renewal")?;

    let expected_identity = format!("{}:{}", pod_ip, webhook_port);
    assert_eq!(
        spec.holder_identity.as_deref(),
        Some(expected_identity.as_str()),
        "holderIdentity should be preserved after renewal"
    );

    // The renew_time should have been updated to a more recent timestamp.
    let renewed_at = spec
        .renew_time
        .as_ref()
        .ok_or("renew_time should be set after renewal")?;
    assert!(
        renewed_at.0 > old_time,
        "renew_time should be newer than the original timestamp"
    );

    Ok(())
}

// get_replica_address returns the holderIdentity from the lease for a known replica.
#[tokio::test]
async fn get_replica_address_returns_holder_identity() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_id = "replica-999";
    let expected_address = "192.168.1.5:8443";

    let lease_name = format!("epa-replica-{}", replica_id);
    let renew_time = k8s_openapi::jiff::Timestamp::now();

    let lease = make_lease(&lease_name, namespace, expected_address, renew_time, 30);

    let client = ClientBuilder::new().with_object(lease).build().await?;

    let manager = MembershipManager::new(
        client,
        replica_id.to_string(),
        "10.0.0.99".to_string(),
        "epa-pod-999".to_string(),
        namespace.to_string(),
        8443,
    );

    let address = manager.get_replica_address(replica_id).await?;

    assert_eq!(
        address, expected_address,
        "get_replica_address should return the holderIdentity from the lease"
    );

    Ok(())
}

// get_replica_address returns an error when no lease exists for the given replica ID.
#[tokio::test]
async fn get_replica_address_missing_returns_error() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";

    let client = ClientBuilder::new().build().await?;

    let manager = MembershipManager::new(
        client,
        "self-replica".to_string(),
        "10.0.0.1".to_string(),
        "epa-pod-self".to_string(),
        namespace.to_string(),
        8443,
    );

    let result = manager.get_replica_address("nonexistent-replica").await;

    assert!(
        result.is_err(),
        "get_replica_address should return an error when no lease exists for the replica"
    );

    Ok(())
}

// Build a Lease with draining=true label for testing draining classification.
fn make_draining_lease(
    name: &str,
    namespace: &str,
    holder_identity: &str,
    renew_time: k8s_openapi::jiff::Timestamp,
    lease_duration_seconds: i32,
) -> Lease {
    let labels = BTreeMap::from([
        ("app".to_string(), "external-pod-autoscaler".to_string()),
        ("draining".to_string(), "true".to_string()),
    ]);

    Lease {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(namespace.to_string()),
            labels: Some(labels),
            ..Default::default()
        },
        spec: Some(LeaseSpec {
            holder_identity: Some(holder_identity.to_string()),
            lease_duration_seconds: Some(lease_duration_seconds),
            renew_time: Some(MicroTime(renew_time)),
            acquire_time: Some(MicroTime(renew_time)),
            lease_transitions: None,
            preferred_holder: None,
            strategy: None,
        }),
    }
}

// update_active_replicas includes only non-expired leases and excludes expired ones.
#[tokio::test]
async fn update_active_replicas_filters_expired() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";

    // A lease renewed recently — still valid (renew_time + 30s is in the future).
    let valid_replica_id = "replica-valid";
    let valid_lease_name = format!("epa-replica-{}", valid_replica_id);
    let valid_renew_time = k8s_openapi::jiff::Timestamp::now() - std::time::Duration::from_secs(5);
    let valid_lease = make_lease(
        &valid_lease_name,
        namespace,
        "10.0.0.10:8443",
        valid_renew_time,
        30, // expires at now+25s
    );

    // A lease whose renew_time + duration is in the past — expired.
    let expired_replica_id = "replica-expired";
    let expired_lease_name = format!("epa-replica-{}", expired_replica_id);
    let expired_renew_time =
        k8s_openapi::jiff::Timestamp::now() - std::time::Duration::from_secs(120);
    let expired_lease = make_lease(
        &expired_lease_name,
        namespace,
        "10.0.0.20:8443",
        expired_renew_time,
        30, // expired 90s ago
    );

    let client = ClientBuilder::new()
        .with_object(valid_lease)
        .with_object(expired_lease)
        .build()
        .await?;

    let manager = MembershipManager::new(
        client,
        "self-replica".to_string(),
        "10.0.0.1".to_string(),
        "epa-pod-self".to_string(),
        namespace.to_string(),
        8443,
    );

    manager.update_active_replicas().await?;

    let active = manager.get_active_replicas().await;

    assert!(
        active.contains(&valid_replica_id.to_string()),
        "non-expired replica should be in the active set; got: {:?}",
        active
    );
    assert!(
        !active.contains(&expired_replica_id.to_string()),
        "expired replica should not be in the active set; got: {:?}",
        active
    );
    assert_eq!(
        active.len(),
        1,
        "exactly one replica should be active; got: {:?}",
        active
    );

    Ok(())
}

// update_active_replicas classifies draining-labelled leases into the draining
// set and excludes them from the active set.
#[tokio::test]
async fn update_active_replicas_separates_draining() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";

    let active_id = "replica-active";
    let draining_id = "replica-draining";

    let now = k8s_openapi::jiff::Timestamp::now();

    let active_lease = make_lease(
        &format!("epa-replica-{}", active_id),
        namespace,
        "10.0.0.1:8443",
        now,
        30,
    );

    let draining_lease = make_draining_lease(
        &format!("epa-replica-{}", draining_id),
        namespace,
        "10.0.0.2:8443",
        now,
        30,
    );

    let client = ClientBuilder::new()
        .with_object(active_lease)
        .with_object(draining_lease)
        .build()
        .await?;

    let manager = MembershipManager::new(
        client,
        "self-replica".to_string(),
        "10.0.0.99".to_string(),
        "epa-pod-self".to_string(),
        namespace.to_string(),
        8443,
    );

    manager.update_active_replicas().await?;

    let active = manager.get_active_replicas().await;
    let draining = manager.get_draining_replicas().await;

    assert!(
        active.contains(&active_id.to_string()),
        "active replica should be in the active set; got: {:?}",
        active
    );
    assert!(
        !active.contains(&draining_id.to_string()),
        "draining replica should NOT be in the active set; got: {:?}",
        active
    );
    assert!(
        draining.contains(&draining_id.to_string()),
        "draining replica should be in the draining set; got: {:?}",
        draining
    );
    assert!(
        !draining.contains(&active_id.to_string()),
        "active replica should NOT be in the draining set; got: {:?}",
        draining
    );

    Ok(())
}

// mark_draining sets the draining label on the lease.
#[tokio::test]
async fn mark_draining_applies_label_to_lease() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_id = "replica-drain";
    let pod_ip = "10.0.0.1";
    let pod_name = "epa-pod-drain";

    let client = ClientBuilder::new().build().await?;

    let manager = MembershipManager::new(
        client.clone(),
        replica_id.to_string(),
        pod_ip.to_string(),
        pod_name.to_string(),
        namespace.to_string(),
        8443,
    );

    // Create the lease first
    manager.register_and_renew().await?;

    // Mark as draining
    manager.mark_draining().await;

    // Verify the lease has the draining label
    let lease_api: Api<Lease> = Api::namespaced(client, namespace);
    let lease_name = format!("epa-replica-{}", replica_id);
    let lease = lease_api.get(&lease_name).await?;

    let labels = lease
        .metadata
        .labels
        .as_ref()
        .ok_or("lease should have labels")?;

    assert_eq!(
        labels.get("draining").map(String::as_str),
        Some("true"),
        "lease should have draining=true label after mark_draining"
    );

    Ok(())
}

// delete_lease removes the lease from Kubernetes.
#[tokio::test]
async fn delete_lease_removes_lease() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_id = "replica-del";
    let pod_ip = "10.0.0.1";
    let pod_name = "epa-pod-del";

    let lease_name = format!("epa-replica-{}", replica_id);
    let now = k8s_openapi::jiff::Timestamp::now();
    let lease = make_lease(&lease_name, namespace, "10.0.0.1:8443", now, 30);

    let client = ClientBuilder::new().with_object(lease).build().await?;

    let manager = MembershipManager::new(
        client.clone(),
        replica_id.to_string(),
        pod_ip.to_string(),
        pod_name.to_string(),
        namespace.to_string(),
        8443,
    );

    manager.delete_lease().await?;

    // Verify the lease is gone
    let lease_api: Api<Lease> = Api::namespaced(client, namespace);
    let err = lease_api
        .get(&lease_name)
        .await
        .expect_err("lease should not exist after delete_lease");
    assert!(
        matches!(err, kube::Error::Api(ref ae) if ae.code == 404),
        "expected 404 not-found, got: {:?}",
        err
    );

    Ok(())
}

// Local is_draining override moves this replica from active to draining even
// when the lease label hasn't propagated yet (watch lag).
#[tokio::test]
async fn update_active_replicas_local_draining_override() -> Result<(), Box<dyn std::error::Error>>
{
    let namespace = "test-ns";
    let my_id = "replica-self";

    let now = k8s_openapi::jiff::Timestamp::now();

    // Create a lease for this replica WITHOUT the draining label (simulating watch lag)
    let my_lease = make_lease(
        &format!("epa-replica-{}", my_id),
        namespace,
        "10.0.0.1:8443",
        now,
        30,
    );

    let client = ClientBuilder::new().with_object(my_lease).build().await?;

    let manager = MembershipManager::new(
        client,
        my_id.to_string(),
        "10.0.0.1".to_string(),
        "epa-pod-self".to_string(),
        namespace.to_string(),
        8443,
    );

    // Set the flag directly without patching the lease, simulating the race
    // where the flag is set but the label hasn't propagated to the API server.
    manager.set_draining_for_test().await;

    // update_active_replicas should use the local override even though the
    // listed lease does not have the draining label
    manager.update_active_replicas().await?;

    let active = manager.get_active_replicas().await;
    let draining = manager.get_draining_replicas().await;

    assert!(
        !active.contains(&my_id.to_string()),
        "locally draining replica should NOT be in the active set; got: {:?}",
        active
    );
    assert!(
        draining.contains(&my_id.to_string()),
        "locally draining replica should be in the draining set; got: {:?}",
        draining
    );

    Ok(())
}

// delete_lease is idempotent — calling it when the lease is already gone succeeds.
#[tokio::test]
async fn delete_lease_idempotent() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";

    let client = ClientBuilder::new().build().await?;

    let manager = MembershipManager::new(
        client,
        "replica-gone".to_string(),
        "10.0.0.1".to_string(),
        "epa-pod-gone".to_string(),
        namespace.to_string(),
        8443,
    );

    // Should not error even though no lease exists
    let result = manager.delete_lease().await;
    assert!(
        result.is_ok(),
        "delete_lease should succeed even when lease does not exist"
    );

    Ok(())
}

// get_all_replicas returns the union of the active and draining sets.
#[tokio::test]
async fn get_all_replicas_returns_union_of_active_and_draining()
-> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let now = k8s_openapi::jiff::Timestamp::now();

    let active_lease = make_lease("epa-replica-active", namespace, "10.0.0.1:8443", now, 30);
    let draining_lease =
        make_draining_lease("epa-replica-draining", namespace, "10.0.0.2:8443", now, 30);

    let client = ClientBuilder::new()
        .with_object(active_lease)
        .with_object(draining_lease)
        .build()
        .await?;

    let manager = MembershipManager::new(
        client,
        "self".to_string(),
        "10.0.0.99".to_string(),
        "pod-self".to_string(),
        namespace.to_string(),
        8443,
    );
    manager.update_active_replicas().await?;

    let all = manager.get_all_replicas().await;
    assert_eq!(
        all.len(),
        2,
        "should contain both active and draining replicas"
    );
    assert!(
        all.contains(&"active".to_string()),
        "active replica must be present; got: {:?}",
        all
    );
    assert!(
        all.contains(&"draining".to_string()),
        "draining replica must be present; got: {:?}",
        all
    );

    // Verify the disjointness invariant: each ID lands in the correct set.
    let active = manager.get_active_replicas().await;
    let draining = manager.get_draining_replicas().await;
    assert!(
        active.contains(&"active".to_string()),
        "non-draining lease must be in active set; got: {:?}",
        active
    );
    assert!(
        draining.contains(&"draining".to_string()),
        "draining-labelled lease must be in draining set; got: {:?}",
        draining
    );
    Ok(())
}
