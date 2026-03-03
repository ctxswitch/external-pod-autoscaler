use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{MicroTime, ObjectMeta};
use kube::api::Api;
use kube_fake_client::ClientBuilder;

use crate::controller::membership::MembershipManager;

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
