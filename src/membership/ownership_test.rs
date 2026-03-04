use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{MicroTime, ObjectMeta};
use kube::api::{Api, DeleteParams, PostParams};
use kube_fake_client::ClientBuilder;

use super::manager::MembershipManager;
use super::ownership::{EpaOwnership, GRACE_BUFFER};

// NOTE: Grace-window logic in EpaOwnership uses tokio::time::Instant, while
// lease expiry in MembershipManager uses wall-clock time (jiff::Timestamp).
// Tests using start_paused = true can control grace-window timing via
// tokio::time::advance(), but cannot simulate lease expiry through time
// advancement alone — delete leases explicitly to simulate replica removal.

/// Build a valid, non-expired Lease with `draining=true` label for use with the fake client.
fn make_draining_active_lease(replica_id: &str, namespace: &str, pod_ip: &str) -> Lease {
    let lease_name = format!("epa-replica-{}", replica_id);
    let now = k8s_openapi::jiff::Timestamp::now();

    let labels = BTreeMap::from([
        ("app".to_string(), "external-pod-autoscaler".to_string()),
        ("draining".to_string(), "true".to_string()),
    ]);

    Lease {
        metadata: ObjectMeta {
            name: Some(lease_name),
            namespace: Some(namespace.to_string()),
            labels: Some(labels),
            ..Default::default()
        },
        spec: Some(LeaseSpec {
            holder_identity: Some(format!("{}:8443", pod_ip)),
            lease_duration_seconds: Some(30),
            renew_time: Some(MicroTime(now)),
            acquire_time: Some(MicroTime(now)),
            lease_transitions: None,
            preferred_holder: None,
            strategy: None,
        }),
    }
}

/// Build a valid, non-expired Lease for use with the fake client.
fn make_active_lease(replica_id: &str, namespace: &str, pod_ip: &str) -> Lease {
    let lease_name = format!("epa-replica-{}", replica_id);
    let now = k8s_openapi::jiff::Timestamp::now();

    Lease {
        metadata: ObjectMeta {
            name: Some(lease_name),
            namespace: Some(namespace.to_string()),
            labels: Some([("app".to_string(), "external-pod-autoscaler".to_string())].into()),
            ..Default::default()
        },
        spec: Some(LeaseSpec {
            holder_identity: Some(format!("{}:8443", pod_ip)),
            lease_duration_seconds: Some(30),
            renew_time: Some(MicroTime(now)),
            acquire_time: Some(MicroTime(now)),
            lease_transitions: None,
            preferred_holder: None,
            strategy: None,
        }),
    }
}

async fn make_ownership(
    client: kube::Client,
    replica_id: &str,
    pod_ip: &str,
    namespace: &str,
) -> Result<(Arc<MembershipManager>, EpaOwnership), Box<dyn std::error::Error>> {
    let manager = Arc::new(MembershipManager::new(
        client,
        replica_id.to_string(),
        pod_ip.to_string(),
        format!("epa-pod-{}", replica_id),
        namespace.to_string(),
        8443,
    ));
    manager.update_active_replicas().await?;
    let ownership = EpaOwnership::new(Arc::clone(&manager));
    Ok((manager, ownership))
}

#[tokio::test]
async fn single_replica_owns_all_epas() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_id = "replica-1";

    let lease = make_active_lease(replica_id, namespace, "10.0.0.1");

    let client = ClientBuilder::new().with_object(lease).build().await?;

    let (_manager, ownership) = make_ownership(client, replica_id, "10.0.0.1", namespace).await?;

    let owner = ownership.get_epa_owner("default", "test-epa").await;
    assert_eq!(
        owner.as_deref(),
        Some(replica_id),
        "Single replica should always be the owner"
    );

    Ok(())
}

#[tokio::test]
async fn exactly_one_replica_owns_epa() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_a = "replica-a";
    let replica_b = "replica-b";

    let lease_a = make_active_lease(replica_a, namespace, "10.0.0.1");
    let lease_b = make_active_lease(replica_b, namespace, "10.0.0.2");

    let client = ClientBuilder::new()
        .with_object(lease_a)
        .with_object(lease_b)
        .build()
        .await?;

    let (_manager_a, ownership_a) =
        make_ownership(client.clone(), replica_a, "10.0.0.1", namespace).await?;
    let (_manager_b, ownership_b) =
        make_ownership(client, replica_b, "10.0.0.2", namespace).await?;

    let owner_a = ownership_a
        .get_epa_owner("default", "test-epa")
        .await
        .ok_or("expected an owner when two replicas are active")?;
    let owner_b = ownership_b
        .get_epa_owner("default", "test-epa")
        .await
        .ok_or("expected an owner when two replicas are active")?;
    assert_eq!(owner_a, owner_b, "Both replicas should agree on the owner");
    assert!(
        owner_a == replica_a || owner_a == replica_b,
        "Owner should be one of the active replicas, got {}",
        owner_a
    );

    let owner_epa2_a = ownership_a
        .get_epa_owner("default", "test-epa-2")
        .await
        .ok_or("expected an owner for second EPA")?;
    let owner_epa2_b = ownership_b
        .get_epa_owner("default", "test-epa-2")
        .await
        .ok_or("expected an owner for second EPA")?;
    assert_eq!(
        owner_epa2_a, owner_epa2_b,
        "Both replicas must agree for any EPA key"
    );

    Ok(())
}

#[tokio::test]
async fn get_epa_owner_no_replicas() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";

    let client = ClientBuilder::new().build().await?;

    // update_active_replicas intentionally not called; active set stays empty
    let manager = Arc::new(MembershipManager::new(
        client,
        "replica-lonely".to_string(),
        "10.0.0.1".to_string(),
        "epa-pod-lonely".to_string(),
        namespace.to_string(),
        8443,
    ));

    let ownership = EpaOwnership::new(manager);

    let owner = ownership.get_epa_owner("default", "test-epa").await;
    assert!(
        owner.is_none(),
        "No active replicas should produce None owner"
    );

    Ok(())
}

#[tokio::test]
async fn get_epa_owner_consistent() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_id = "replica-1";

    let lease = make_active_lease(replica_id, namespace, "10.0.0.1");

    let client = ClientBuilder::new().with_object(lease).build().await?;

    let (_manager, ownership) = make_ownership(client, replica_id, "10.0.0.1", namespace).await?;

    let owner1 = ownership.get_epa_owner("default", "test-epa").await;
    let owner2 = ownership.get_epa_owner("default", "test-epa").await;

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

#[tokio::test(start_paused = true)]
async fn refresh_ownership_tracks_new_owner() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_id = "replica-1";

    let lease = make_active_lease(replica_id, namespace, "10.0.0.1");
    let client = ClientBuilder::new().with_object(lease).build().await?;

    let (_manager, ownership) = make_ownership(client, replica_id, "10.0.0.1", namespace).await?;

    assert!(
        !ownership.should_scrape_epa("default", "test-epa").await,
        "Should not scrape before refresh_ownership is called"
    );
    assert!(
        !ownership.should_serve_epa("default", "test-epa").await,
        "Should not serve before refresh_ownership is called"
    );

    ownership
        .refresh_ownership("default", "test-epa", Duration::from_millis(100))
        .await;

    assert!(
        ownership.should_scrape_epa("default", "test-epa").await,
        "New owner should scrape immediately"
    );
    assert!(
        ownership.should_serve_epa("default", "test-epa").await,
        "New owner should serve immediately when no previous owner exists"
    );

    Ok(())
}

#[tokio::test(start_paused = true)]
async fn should_serve_after_evaluation_period_elapses() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_id = "replica-1";

    let lease = make_active_lease(replica_id, namespace, "10.0.0.1");
    let client = ClientBuilder::new().with_object(lease).build().await?;

    let (_manager, ownership) = make_ownership(client, replica_id, "10.0.0.1", namespace).await?;
    let eval_period = Duration::from_millis(50);

    ownership
        .refresh_ownership("default", "test-epa", eval_period)
        .await;

    tokio::time::advance(Duration::from_millis(200)).await;

    ownership
        .refresh_ownership("default", "test-epa", eval_period)
        .await;

    assert!(
        ownership.should_scrape_epa("default", "test-epa").await,
        "Owner should still scrape after evaluation period"
    );
    assert!(
        ownership.should_serve_epa("default", "test-epa").await,
        "Owner should serve after evaluation period elapses"
    );

    Ok(())
}

#[tokio::test(start_paused = true)]
async fn lost_owner_still_scrapes_and_serves() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_a = "replica-a";
    let replica_b = "replica-b";

    let lease_a = make_active_lease(replica_a, namespace, "10.0.0.1");
    let lease_b = make_active_lease(replica_b, namespace, "10.0.0.2");

    let client = ClientBuilder::new()
        .with_object(lease_a)
        .with_object(lease_b)
        .build()
        .await?;

    let owner = {
        let (_, probe) = make_ownership(client.clone(), replica_a, "10.0.0.1", namespace).await?;
        probe
            .get_epa_owner("default", "test-epa")
            .await
            .ok_or("should have an owner with two active replicas")?
    };

    let (winner_id, winner_manager, winner_ownership) = if owner == replica_a {
        let (m, a) = make_ownership(client.clone(), replica_a, "10.0.0.1", namespace).await?;
        (replica_a, m, a)
    } else {
        let (m, a) = make_ownership(client.clone(), replica_b, "10.0.0.2", namespace).await?;
        (replica_b, m, a)
    };

    let eval_period = Duration::from_millis(50);

    winner_ownership
        .refresh_ownership("default", "test-epa", eval_period)
        .await;

    tokio::time::advance(Duration::from_millis(200)).await;

    winner_ownership
        .refresh_ownership("default", "test-epa", eval_period)
        .await;

    assert!(
        winner_ownership
            .should_serve_epa("default", "test-epa")
            .await,
        "Winner should be serving before we remove its lease"
    );

    let lease_api: Api<Lease> = Api::namespaced(client.clone(), namespace);
    let winner_lease_name = format!("epa-replica-{}", winner_id);
    lease_api
        .delete(&winner_lease_name, &DeleteParams::default())
        .await?;

    winner_manager.update_active_replicas().await?;

    winner_ownership
        .refresh_ownership("default", "test-epa", eval_period)
        .await;

    assert!(
        winner_ownership
            .should_scrape_epa("default", "test-epa")
            .await,
        "Old owner should still scrape during the grace window"
    );
    assert!(
        winner_ownership
            .should_serve_epa("default", "test-epa")
            .await,
        "Old owner should still serve during the grace window"
    );

    tokio::time::advance(eval_period + GRACE_BUFFER + Duration::from_millis(100)).await;
    winner_ownership
        .refresh_ownership("default", "test-epa", eval_period)
        .await;
    assert!(
        !winner_ownership
            .should_scrape_epa("default", "test-epa")
            .await,
        "Old owner must stop scraping after grace window expires"
    );
    assert!(
        !winner_ownership
            .should_serve_epa("default", "test-epa")
            .await,
        "Old owner must stop serving after grace window expires"
    );

    Ok(())
}

#[tokio::test]
async fn non_owner_does_not_scrape_or_serve() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_a = "replica-a";
    let replica_b = "replica-b";

    let lease_a = make_active_lease(replica_a, namespace, "10.0.0.1");
    let lease_b = make_active_lease(replica_b, namespace, "10.0.0.2");

    let client = ClientBuilder::new()
        .with_object(lease_a)
        .with_object(lease_b)
        .build()
        .await?;

    let (_manager_a, ownership_a) =
        make_ownership(client.clone(), replica_a, "10.0.0.1", namespace).await?;
    let (_manager_b, ownership_b) =
        make_ownership(client, replica_b, "10.0.0.2", namespace).await?;

    let owner = ownership_a
        .get_epa_owner("default", "test-epa")
        .await
        .ok_or("should have an owner")?;

    let non_owner_ownership = if owner == replica_a {
        &ownership_b
    } else {
        &ownership_a
    };

    non_owner_ownership
        .refresh_ownership("default", "test-epa", Duration::from_millis(100))
        .await;

    assert!(
        !non_owner_ownership
            .should_scrape_epa("default", "test-epa")
            .await,
        "Non-owner should not scrape"
    );
    assert!(
        !non_owner_ownership
            .should_serve_epa("default", "test-epa")
            .await,
        "Non-owner should not serve"
    );

    Ok(())
}

#[tokio::test(start_paused = true)]
async fn scale_down_old_owner_continues_serving() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_a = "replica-a";
    let replica_b = "replica-b";
    let replica_c = "replica-c";

    let lease_a = make_active_lease(replica_a, namespace, "10.0.0.1");
    let lease_b = make_active_lease(replica_b, namespace, "10.0.0.2");
    let lease_c = make_active_lease(replica_c, namespace, "10.0.0.3");

    let client = ClientBuilder::new()
        .with_object(lease_a)
        .with_object(lease_b)
        .with_object(lease_c)
        .build()
        .await?;

    let owner = {
        let (_, probe) = make_ownership(client.clone(), replica_a, "10.0.0.1", namespace).await?;
        probe
            .get_epa_owner("default", "test-epa")
            .await
            .ok_or("should have an owner with three active replicas")?
    };

    let (winner_id, winner_manager, winner_ownership) = if owner == replica_a {
        let (m, a) = make_ownership(client.clone(), replica_a, "10.0.0.1", namespace).await?;
        (replica_a, m, a)
    } else if owner == replica_b {
        let (m, a) = make_ownership(client.clone(), replica_b, "10.0.0.2", namespace).await?;
        (replica_b, m, a)
    } else {
        let (m, a) = make_ownership(client.clone(), replica_c, "10.0.0.3", namespace).await?;
        (replica_c, m, a)
    };

    let eval_period = Duration::from_millis(50);

    winner_ownership
        .refresh_ownership("default", "test-epa", eval_period)
        .await;

    tokio::time::advance(Duration::from_millis(200)).await;

    winner_ownership
        .refresh_ownership("default", "test-epa", eval_period)
        .await;

    assert!(
        winner_ownership
            .should_serve_epa("default", "test-epa")
            .await,
        "Owner should be serving after evaluation period"
    );

    let lease_api: Api<Lease> = Api::namespaced(client.clone(), namespace);
    let winner_lease_name = format!("epa-replica-{}", winner_id);
    lease_api
        .delete(&winner_lease_name, &DeleteParams::default())
        .await?;

    winner_manager.update_active_replicas().await?;

    winner_ownership
        .refresh_ownership("default", "test-epa", eval_period)
        .await;

    assert!(
        winner_ownership
            .should_scrape_epa("default", "test-epa")
            .await,
        "Old owner should still scrape during grace window after scale-down"
    );
    assert!(
        winner_ownership
            .should_serve_epa("default", "test-epa")
            .await,
        "Old owner should still serve during grace window after scale-down"
    );

    let new_owner = winner_ownership
        .get_epa_owner("default", "test-epa")
        .await
        .ok_or("should have a new owner from remaining replicas")?;
    assert_ne!(
        new_owner, winner_id,
        "New owner should be one of the remaining replicas, not the removed one"
    );

    Ok(())
}

#[tokio::test(start_paused = true)]
async fn scale_up_old_owner_continues_serving() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_a = "replica-a";
    let replica_b = "replica-b";

    let lease_a = make_active_lease(replica_a, namespace, "10.0.0.1");

    let client = ClientBuilder::new().with_object(lease_a).build().await?;

    let (manager_a, ownership_a) =
        make_ownership(client.clone(), replica_a, "10.0.0.1", namespace).await?;

    let eval_period = Duration::from_millis(50);

    ownership_a
        .refresh_ownership("default", "test-epa", eval_period)
        .await;

    tokio::time::advance(Duration::from_millis(200)).await;

    ownership_a
        .refresh_ownership("default", "test-epa", eval_period)
        .await;

    assert!(
        ownership_a.should_serve_epa("default", "test-epa").await,
        "Replica a should be serving as the sole owner"
    );

    let lease_b = make_active_lease(replica_b, namespace, "10.0.0.2");
    let lease_api: Api<Lease> = Api::namespaced(client.clone(), namespace);
    lease_api.create(&PostParams::default(), &lease_b).await?;

    manager_a.update_active_replicas().await?;

    ownership_a
        .refresh_ownership("default", "test-epa", eval_period)
        .await;

    let new_owner = ownership_a
        .get_epa_owner("default", "test-epa")
        .await
        .ok_or("should have an owner after scale-up")?;

    assert!(
        ownership_a.should_scrape_epa("default", "test-epa").await,
        "ownership_a must scrape after scale-up (as owner or during grace window)"
    );
    assert!(
        ownership_a.should_serve_epa("default", "test-epa").await,
        "ownership_a must serve after scale-up (as owner or during grace window)"
    );

    if new_owner != replica_a {
        assert_eq!(
            new_owner, replica_b,
            "when replica_a loses hash ownership, replica_b must be the new owner"
        );
    }

    Ok(())
}

#[tokio::test]
async fn get_previous_epa_owner_returns_draining_replica() -> Result<(), Box<dyn std::error::Error>>
{
    let namespace = "test-ns";
    let replica_a = "replica-a";
    let replica_b = "replica-b";
    let replica_c = "replica-c";

    let lease_a = make_active_lease(replica_a, namespace, "10.0.0.1");
    let lease_b = make_active_lease(replica_b, namespace, "10.0.0.2");
    let lease_c = make_active_lease(replica_c, namespace, "10.0.0.3");

    let client = ClientBuilder::new()
        .with_object(lease_a)
        .with_object(lease_b)
        .with_object(lease_c)
        .build()
        .await?;

    // Determine the hash winner with all three active.
    let original_owner = {
        let (_, probe) = make_ownership(client.clone(), replica_a, "10.0.0.1", namespace).await?;
        probe
            .get_epa_owner("default", "test-epa")
            .await
            .ok_or("should have an owner with three active replicas")?
    };

    let (winner_id, winner_ip) = if original_owner == replica_a {
        (replica_a, "10.0.0.1")
    } else if original_owner == replica_b {
        (replica_b, "10.0.0.2")
    } else if original_owner == replica_c {
        (replica_c, "10.0.0.3")
    } else {
        panic!("unexpected owner: {}", original_owner);
    };

    // Replace the winner's lease with a draining version.
    let lease_api: Api<Lease> = Api::namespaced(client.clone(), namespace);
    let winner_lease_name = format!("epa-replica-{}", winner_id);
    lease_api
        .delete(&winner_lease_name, &DeleteParams::default())
        .await?;

    let draining_lease = make_draining_active_lease(winner_id, namespace, winner_ip);
    lease_api
        .create(&PostParams::default(), &draining_lease)
        .await?;

    // Build an ownership from one of the survivors and refresh membership.
    // Pick a survivor that is not the draining winner. When the winner is
    // replica_b or replica_c we fall through to replica_a.
    let (survivor_id, survivor_ip) = match winner_id {
        id if id == replica_a => (replica_b, "10.0.0.2"),
        id if id == replica_b => (replica_a, "10.0.0.1"),
        id if id == replica_c => (replica_a, "10.0.0.1"),
        _ => panic!("unexpected winner_id: {}", winner_id),
    };

    // make_ownership calls update_active_replicas internally; the draining
    // lease is already present in the client at this point.
    let (_manager, ownership) =
        make_ownership(client.clone(), survivor_id, survivor_ip, namespace).await?;

    // get_epa_owner should return a surviving (active) replica, not the draining one.
    let current_owner = ownership
        .get_epa_owner("default", "test-epa")
        .await
        .ok_or("should have a current owner from active replicas")?;
    assert_ne!(
        current_owner, winner_id,
        "get_epa_owner should not return the draining replica"
    );

    // get_previous_epa_owner should return the original winner (the draining replica).
    let previous_owner = ownership
        .get_previous_epa_owner("default", "test-epa")
        .await
        .ok_or("should have a previous owner from active + draining replicas")?;
    assert_eq!(
        previous_owner, winner_id,
        "get_previous_epa_owner should return the draining replica that was the original winner"
    );

    assert_ne!(
        current_owner, previous_owner,
        "get_epa_owner and get_previous_epa_owner must disagree when the winner is draining"
    );

    Ok(())
}

#[tokio::test]
async fn get_previous_epa_owner_returns_none_when_no_replicas(
) -> Result<(), Box<dyn std::error::Error>> {
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
    // Exercise the scan path that finds no leases, confirming both
    // active and draining sets remain empty.
    manager.update_active_replicas().await?;
    let ownership = EpaOwnership::new(Arc::clone(&manager));
    let previous = ownership
        .get_previous_epa_owner("default", "test-epa")
        .await;
    assert!(
        previous.is_none(),
        "No active or draining replicas should produce None"
    );
    Ok(())
}

#[tokio::test]
async fn get_previous_epa_owner_matches_current_when_no_draining(
) -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_a = "replica-a";
    let replica_b = "replica-b";

    let lease_a = make_active_lease(replica_a, namespace, "10.0.0.1");
    let lease_b = make_active_lease(replica_b, namespace, "10.0.0.2");

    let client = ClientBuilder::new()
        .with_object(lease_a)
        .with_object(lease_b)
        .build()
        .await?;

    let (_manager, ownership) = make_ownership(client, replica_a, "10.0.0.1", namespace).await?;

    let current = ownership.get_epa_owner("default", "test-epa").await;
    let previous = ownership
        .get_previous_epa_owner("default", "test-epa")
        .await;

    assert!(current.is_some(), "should have a current owner");
    assert_eq!(
        current, previous,
        "with no draining replicas, get_previous_epa_owner should match get_epa_owner"
    );

    Ok(())
}

#[tokio::test]
async fn get_epa_owner_returns_new_owner_during_transition(
) -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_a = "replica-a";
    let replica_b = "replica-b";

    let lease_a = make_active_lease(replica_a, namespace, "10.0.0.1");
    let lease_b = make_active_lease(replica_b, namespace, "10.0.0.2");

    let client = ClientBuilder::new()
        .with_object(lease_a)
        .with_object(lease_b)
        .build()
        .await?;

    let owner = {
        let (_, probe) = make_ownership(client.clone(), replica_a, "10.0.0.1", namespace).await?;
        probe
            .get_epa_owner("default", "test-epa")
            .await
            .ok_or("should have an owner")?
    };

    let (winner_id, other_id, winner_manager, winner_ownership) = if owner == replica_a {
        let (m, a) = make_ownership(client.clone(), replica_a, "10.0.0.1", namespace).await?;
        (replica_a, replica_b, m, a)
    } else {
        let (m, a) = make_ownership(client.clone(), replica_b, "10.0.0.2", namespace).await?;
        (replica_b, replica_a, m, a)
    };

    winner_ownership
        .refresh_ownership("default", "test-epa", Duration::from_millis(100))
        .await;

    let lease_api: Api<Lease> = Api::namespaced(client.clone(), namespace);
    let winner_lease_name = format!("epa-replica-{}", winner_id);
    lease_api
        .delete(&winner_lease_name, &DeleteParams::default())
        .await?;

    winner_manager.update_active_replicas().await?;

    winner_ownership
        .refresh_ownership("default", "test-epa", Duration::from_millis(100))
        .await;

    // During the transition the old owner is in the grace window, but
    // get_epa_owner should return the new hash winner for forwarding.
    let current_owner = winner_ownership
        .get_epa_owner("default", "test-epa")
        .await
        .ok_or("should have a new owner after lease removal")?;

    assert_eq!(
        current_owner, other_id,
        "get_epa_owner should return the new owner during transition for correct forwarding"
    );

    Ok(())
}

#[tokio::test(start_paused = true)]
async fn lost_owner_stops_after_grace_window_expires() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_a = "replica-a";
    let replica_b = "replica-b";

    let lease_a = make_active_lease(replica_a, namespace, "10.0.0.1");
    let lease_b = make_active_lease(replica_b, namespace, "10.0.0.2");

    let client = ClientBuilder::new()
        .with_object(lease_a)
        .with_object(lease_b)
        .build()
        .await?;

    // Determine the winner
    let owner = {
        let (_, probe) = make_ownership(client.clone(), replica_a, "10.0.0.1", namespace).await?;
        probe
            .get_epa_owner("default", "test-epa")
            .await
            .ok_or("should have an owner with two active replicas")?
    };

    let (winner_id, winner_manager, winner_ownership) = if owner == replica_a {
        let (m, a) = make_ownership(client.clone(), replica_a, "10.0.0.1", namespace).await?;
        (replica_a, m, a)
    } else {
        let (m, a) = make_ownership(client.clone(), replica_b, "10.0.0.2", namespace).await?;
        (replica_b, m, a)
    };

    let eval_period = Duration::from_secs(10);

    // Establish ownership and wait for eval period to elapse
    winner_ownership
        .refresh_ownership("default", "test-epa", eval_period)
        .await;

    tokio::time::advance(eval_period + Duration::from_secs(1)).await;

    winner_ownership
        .refresh_ownership("default", "test-epa", eval_period)
        .await;

    assert!(
        winner_ownership
            .should_serve_epa("default", "test-epa")
            .await,
        "Winner should be serving after eval period"
    );

    // Delete the winner's lease to trigger ownership loss
    let lease_api: Api<Lease> = Api::namespaced(client.clone(), namespace);
    let winner_lease_name = format!("epa-replica-{}", winner_id);
    lease_api
        .delete(&winner_lease_name, &DeleteParams::default())
        .await?;

    winner_manager.update_active_replicas().await?;

    winner_ownership
        .refresh_ownership("default", "test-epa", eval_period)
        .await;

    // Should still be scraping/serving during grace window
    assert!(
        winner_ownership
            .should_scrape_epa("default", "test-epa")
            .await,
        "Old owner should still scrape during grace window"
    );
    assert!(
        winner_ownership
            .should_serve_epa("default", "test-epa")
            .await,
        "Old owner should still serve during grace window"
    );

    tokio::time::advance(eval_period + GRACE_BUFFER + Duration::from_secs(1)).await;

    // Refresh again to trigger eviction of expired lost entries
    winner_ownership
        .refresh_ownership("default", "test-epa", eval_period)
        .await;

    assert!(
        !winner_ownership
            .should_scrape_epa("default", "test-epa")
            .await,
        "Old owner should stop scraping after grace window expires"
    );
    assert!(
        !winner_ownership
            .should_serve_epa("default", "test-epa")
            .await,
        "Old owner should stop serving after grace window expires"
    );

    Ok(())
}

#[tokio::test]
async fn get_previous_epa_owner_unchanged_when_non_winner_drains(
) -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_a = "replica-a";
    let replica_b = "replica-b";

    let lease_a = make_active_lease(replica_a, namespace, "10.0.0.1");
    let lease_b = make_active_lease(replica_b, namespace, "10.0.0.2");

    let client = ClientBuilder::new()
        .with_object(lease_a)
        .with_object(lease_b)
        .build()
        .await?;

    // Determine the hash winner with both replicas active.
    let original_owner = {
        let (_, probe) = make_ownership(client.clone(), replica_a, "10.0.0.1", namespace).await?;
        probe
            .get_epa_owner("default", "test-epa")
            .await
            .ok_or("should have an owner with two active replicas")?
    };

    // Identify the non-winner and move it to draining.
    let (non_winner_id, non_winner_ip) = if original_owner == replica_a {
        (replica_b, "10.0.0.2")
    } else if original_owner == replica_b {
        (replica_a, "10.0.0.1")
    } else {
        panic!("unexpected owner: {}", original_owner);
    };

    // Replace the non-winner's lease with a draining version.
    let lease_api: Api<Lease> = Api::namespaced(client.clone(), namespace);
    let non_winner_lease_name = format!("epa-replica-{}", non_winner_id);
    lease_api
        .delete(&non_winner_lease_name, &DeleteParams::default())
        .await?;

    let draining_lease = make_draining_active_lease(non_winner_id, namespace, non_winner_ip);
    lease_api
        .create(&PostParams::default(), &draining_lease)
        .await?;

    // Build an ownership from the winner (still active) and refresh membership.
    let (winner_id, winner_ip) = if original_owner == replica_a {
        (replica_a, "10.0.0.1")
    } else if original_owner == replica_b {
        (replica_b, "10.0.0.2")
    } else {
        panic!("unexpected owner: {}", original_owner);
    };

    // make_ownership calls update_active_replicas internally; the draining
    // lease is already present in the client at this point.
    let (_manager, ownership) =
        make_ownership(client.clone(), winner_id, winner_ip, namespace).await?;

    // get_epa_owner should still return the original winner (it is still active).
    let current_owner = ownership
        .get_epa_owner("default", "test-epa")
        .await
        .ok_or("should have a current owner")?;
    assert_eq!(
        current_owner, original_owner,
        "get_epa_owner should still return the original winner when only a non-winner drains"
    );

    // get_previous_epa_owner should also return the original winner because
    // the highest-weight replica across active+draining is unchanged.
    let previous_owner = ownership
        .get_previous_epa_owner("default", "test-epa")
        .await
        .ok_or("should have a previous owner")?;
    assert_eq!(
        previous_owner, original_owner,
        "get_previous_epa_owner should match get_epa_owner when the draining replica is not the winner"
    );

    Ok(())
}

#[tokio::test(start_paused = true)]
async fn should_serve_immediately_when_previous_owner_gone(
) -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_id = "replica-1";

    let lease = make_active_lease(replica_id, namespace, "10.0.0.1");
    let client = ClientBuilder::new().with_object(lease).build().await?;

    let (_manager, ownership) = make_ownership(client, replica_id, "10.0.0.1", namespace).await?;

    // Refresh with a 60-second eval period — do NOT advance time.
    ownership
        .refresh_ownership("default", "test-epa", Duration::from_secs(60))
        .await;

    // Eval period has not elapsed, but there is no previous owner (only us),
    // so should_serve_epa returns true immediately.
    assert!(
        ownership.should_serve_epa("default", "test-epa").await,
        "Should serve immediately when no previous owner exists"
    );

    Ok(())
}

#[tokio::test(start_paused = true)]
async fn should_wait_when_previous_owner_alive() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "test-ns";
    let replica_a = "replica-a";
    let replica_b = "replica-b";

    let lease_a = make_active_lease(replica_a, namespace, "10.0.0.1");
    let lease_b = make_active_lease(replica_b, namespace, "10.0.0.2");

    let client = ClientBuilder::new()
        .with_object(lease_a)
        .with_object(lease_b)
        .build()
        .await?;

    // Determine the hash winner with both replicas active.
    let winner = {
        let (_, probe) = make_ownership(client.clone(), replica_a, "10.0.0.1", namespace).await?;
        probe
            .get_epa_owner("default", "test-epa")
            .await
            .ok_or("should have an owner with two active replicas")?
    };

    // Identify winner and non-winner.
    let (winner_id, winner_ip, non_winner_id, non_winner_ip) = if winner == replica_a {
        (replica_a, "10.0.0.1", replica_b, "10.0.0.2")
    } else {
        (replica_b, "10.0.0.2", replica_a, "10.0.0.1")
    };

    // Delete the winner's active lease and create a draining lease for it.
    let lease_api: Api<Lease> = Api::namespaced(client.clone(), namespace);
    let winner_lease_name = format!("epa-replica-{}", winner_id);
    lease_api
        .delete(&winner_lease_name, &DeleteParams::default())
        .await?;

    let draining_lease = make_draining_active_lease(winner_id, namespace, winner_ip);
    lease_api
        .create(&PostParams::default(), &draining_lease)
        .await?;

    // Set up the non-winner's ownership and refresh membership.
    let (_non_winner_manager, non_winner_ownership) =
        make_ownership(client.clone(), non_winner_id, non_winner_ip, namespace).await?;

    // The non-winner is now the only active replica, so it becomes the hash owner.
    non_winner_ownership
        .refresh_ownership("default", "test-epa", Duration::from_secs(60))
        .await;

    // Do NOT advance time — eval period has not elapsed.
    // The previous owner (winner) is still in the draining set, so we should wait.
    assert!(
        !non_winner_ownership
            .should_serve_epa("default", "test-epa")
            .await,
        "Should not serve when previous owner is still draining"
    );

    // Verify the non-winner does own the EPA now.
    let current_owner = non_winner_ownership
        .get_epa_owner("default", "test-epa")
        .await
        .ok_or("should have a current owner")?;
    assert_eq!(
        current_owner, non_winner_id,
        "Non-winner should now be the hash owner"
    );

    // Verify the previous owner is the draining winner.
    let previous_owner = non_winner_ownership
        .get_previous_epa_owner("default", "test-epa")
        .await
        .ok_or("should have a previous owner")?;
    assert_eq!(
        previous_owner, winner_id,
        "Previous owner should be the draining winner"
    );

    Ok(())
}

#[tokio::test(start_paused = true)]
async fn should_serve_when_previous_owner_lease_expires() -> Result<(), Box<dyn std::error::Error>>
{
    let namespace = "test-ns";
    let replica_a = "replica-a";
    let replica_b = "replica-b";

    let lease_a = make_active_lease(replica_a, namespace, "10.0.0.1");
    let lease_b = make_active_lease(replica_b, namespace, "10.0.0.2");

    let client = ClientBuilder::new()
        .with_object(lease_a)
        .with_object(lease_b)
        .build()
        .await?;

    // Determine the hash winner with both replicas active.
    let winner = {
        let (_, probe) = make_ownership(client.clone(), replica_a, "10.0.0.1", namespace).await?;
        probe
            .get_epa_owner("default", "test-epa")
            .await
            .ok_or("should have an owner with two active replicas")?
    };

    // Identify winner and non-winner.
    let (winner_id, winner_ip, non_winner_id, non_winner_ip) = if winner == replica_a {
        (replica_a, "10.0.0.1", replica_b, "10.0.0.2")
    } else {
        (replica_b, "10.0.0.2", replica_a, "10.0.0.1")
    };

    // Delete the winner's active lease and create a draining lease for it.
    let lease_api: Api<Lease> = Api::namespaced(client.clone(), namespace);
    let winner_lease_name = format!("epa-replica-{}", winner_id);
    lease_api
        .delete(&winner_lease_name, &DeleteParams::default())
        .await?;

    let draining_lease = make_draining_active_lease(winner_id, namespace, winner_ip);
    lease_api
        .create(&PostParams::default(), &draining_lease)
        .await?;

    // Now delete the draining lease too — simulating lease expiry.
    lease_api
        .delete(&winner_lease_name, &DeleteParams::default())
        .await?;

    // Set up the non-winner's ownership and refresh membership.
    let (_non_winner_manager, non_winner_ownership) =
        make_ownership(client.clone(), non_winner_id, non_winner_ip, namespace).await?;

    // The non-winner is now the only replica (active or otherwise).
    non_winner_ownership
        .refresh_ownership("default", "test-epa", Duration::from_secs(60))
        .await;

    // Do NOT advance time — eval period has not elapsed.
    // The previous owner's draining lease is gone, so no other replica is
    // present in active or draining sets; the new owner should serve immediately.
    assert!(
        non_winner_ownership
            .should_serve_epa("default", "test-epa")
            .await,
        "Should serve when the previous owner's lease has expired"
    );

    Ok(())
}
