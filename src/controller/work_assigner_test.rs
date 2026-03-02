use std::sync::Arc;

use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{MicroTime, ObjectMeta};
use kube_fake_client::ClientBuilder;

use super::membership::MembershipManager;
use super::work_assigner::WorkAssigner;

#[test]
fn test_compute_weight_deterministic() {
    let weight1 = WorkAssigner::compute_weight("replica-1", "default/test-epa");
    let weight2 = WorkAssigner::compute_weight("replica-1", "default/test-epa");
    assert_eq!(weight1, weight2, "Hash should be deterministic");
}

#[test]
fn test_compute_weight_different_inputs() {
    let weight1 = WorkAssigner::compute_weight("replica-1", "default/epa-1");
    let weight2 = WorkAssigner::compute_weight("replica-2", "default/epa-1");
    let weight3 = WorkAssigner::compute_weight("replica-1", "default/epa-2");

    assert_ne!(
        weight1, weight2,
        "Different replicas should produce different hashes"
    );
    assert_ne!(
        weight1, weight3,
        "Different EPAs should produce different hashes"
    );
}

/// Build a valid, non-expired Lease for use with the fake client.
fn make_active_lease(replica_id: &str, namespace: &str) -> Lease {
    let lease_name = format!("epa-replica-{}", replica_id);
    let now = chrono::Utc::now();

    Lease {
        metadata: ObjectMeta {
            name: Some(lease_name),
            namespace: Some(namespace.to_string()),
            labels: Some([("app".to_string(), "external-pod-autoscaler".to_string())].into()),
            ..Default::default()
        },
        spec: Some(LeaseSpec {
            holder_identity: Some("10.0.0.1:8443".to_string()),
            lease_duration_seconds: Some(30),
            renew_time: Some(MicroTime(now)),
            acquire_time: Some(MicroTime(now)),
            lease_transitions: None,
        }),
    }
}

// Single replica — should_handle_epa always returns true for the sole owner.
#[tokio::test]
async fn should_handle_epa_returns_true_for_owner() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_id = "replica-1";

    let lease = make_active_lease(replica_id, namespace);

    let client = ClientBuilder::new().with_object(lease).build().await?;

    let manager = Arc::new(MembershipManager::new(
        client,
        replica_id.to_string(),
        "10.0.0.1".to_string(),
        "epa-pod-1".to_string(),
        namespace.to_string(),
        8443,
    ));

    manager.update_active_replicas().await?;

    let assigner = WorkAssigner::new(manager);

    assert!(
        assigner.should_handle_epa("default", "test-epa").await,
        "Single replica should always be the owner"
    );

    Ok(())
}

// Two replicas — only the owner replica returns true for should_handle_epa.
#[tokio::test]
async fn should_handle_epa_returns_false_for_non_owner() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_a = "replica-a";
    let replica_b = "replica-b";

    let lease_a = make_active_lease(replica_a, namespace);
    let lease_b = make_active_lease(replica_b, namespace);

    let client = ClientBuilder::new()
        .with_object(lease_a)
        .with_object(lease_b)
        .build()
        .await?;

    // Create manager for replica-a
    let manager_a = Arc::new(MembershipManager::new(
        client.clone(),
        replica_a.to_string(),
        "10.0.0.1".to_string(),
        "epa-pod-a".to_string(),
        namespace.to_string(),
        8443,
    ));
    manager_a.update_active_replicas().await?;
    let assigner_a = WorkAssigner::new(manager_a);

    // Create manager for replica-b
    let manager_b = Arc::new(MembershipManager::new(
        client,
        replica_b.to_string(),
        "10.0.0.2".to_string(),
        "epa-pod-b".to_string(),
        namespace.to_string(),
        8443,
    ));
    manager_b.update_active_replicas().await?;
    let assigner_b = WorkAssigner::new(manager_b);

    let a_handles = assigner_a.should_handle_epa("default", "test-epa").await;
    let b_handles = assigner_b.should_handle_epa("default", "test-epa").await;

    // Exactly one should be true
    assert_ne!(
        a_handles, b_handles,
        "Exactly one replica should own the EPA, got a={}, b={}",
        a_handles, b_handles
    );

    Ok(())
}

// No active replicas — get_epa_owner returns None.
#[tokio::test]
async fn get_epa_owner_no_replicas() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";

    let client = ClientBuilder::new().build().await?;

    let manager = Arc::new(MembershipManager::new(
        client,
        "replica-lonely".to_string(),
        "10.0.0.1".to_string(),
        "epa-pod-lonely".to_string(),
        namespace.to_string(),
        8443,
    ));

    // Don't call update_active_replicas — active set stays empty
    let assigner = WorkAssigner::new(manager);

    let owner = assigner.get_epa_owner("default/test-epa").await;
    assert!(
        owner.is_none(),
        "No active replicas should produce None owner"
    );

    Ok(())
}

// get_epa_owner is deterministic — same inputs produce same owner.
#[tokio::test]
async fn get_epa_owner_consistent() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_id = "replica-1";

    let lease = make_active_lease(replica_id, namespace);

    let client = ClientBuilder::new().with_object(lease).build().await?;

    let manager = Arc::new(MembershipManager::new(
        client,
        replica_id.to_string(),
        "10.0.0.1".to_string(),
        "epa-pod-1".to_string(),
        namespace.to_string(),
        8443,
    ));

    manager.update_active_replicas().await?;

    let assigner = WorkAssigner::new(manager);

    let owner1 = assigner.get_epa_owner("default/test-epa").await;
    let owner2 = assigner.get_epa_owner("default/test-epa").await;

    assert_eq!(
        owner1, owner2,
        "get_epa_owner should return the same owner for the same input"
    );
    assert!(
        owner1.is_some(),
        "Owner should be Some when replicas are active"
    );

    Ok(())
}
