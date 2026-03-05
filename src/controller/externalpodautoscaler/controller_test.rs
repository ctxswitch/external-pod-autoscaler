use std::sync::Arc;

use k8s_openapi::api::autoscaling::v2::HorizontalPodAutoscaler;
use k8s_openapi::api::coordination::v1::Lease;
use kube::api::Api;
use kube_fake_client::ClientBuilder;
use tokio::sync::mpsc;

use crate::apis::ctx_sh::v1beta1::ExternalPodAutoscaler;
use crate::controller::externalpodautoscaler::reconcile::Reconciler;
use crate::membership::manager::MembershipManager;
use crate::membership::ownership::EpaOwnership;
use crate::scraper::EpaUpdate;
use crate::store::MetricsStore;

/// Creates an `EpaOwnership` backed by a single-replica `MembershipManager`.
///
/// Because only one replica is registered, rendezvous hashing always elects it
/// as the owner -- making the returned ownership suitable for tests that expect
/// this replica to be the owner.
async fn test_ownership(
    client: kube::Client,
) -> Result<Arc<EpaOwnership>, Box<dyn std::error::Error>> {
    let membership = Arc::new(MembershipManager::new(
        client,
        "test-uid".to_string(),
        "127.0.0.1".to_string(),
        "test-pod".to_string(),
        "default".to_string(),
        8443,
    ));
    membership.register_and_renew().await?;
    membership.update_active_replicas().await?;
    Ok(Arc::new(EpaOwnership::new(membership)))
}

/// Creates an `EpaOwnership` where the active set is empty, so `is_epa_owner`
/// always returns `false`. Used to test non-owner reconciliation behavior.
fn test_ownership_not_owner(client: kube::Client) -> Arc<EpaOwnership> {
    // No lease registration or active replica refresh -- active set stays empty,
    // so get_epa_owner returns None and is_epa_owner returns false.
    let membership = Arc::new(MembershipManager::new(
        client,
        "test-uid".to_string(),
        "127.0.0.1".to_string(),
        "test-pod".to_string(),
        "default".to_string(),
        8443,
    ));
    Arc::new(EpaOwnership::new(membership))
}

// New EPA with no existing HPA — HPA should be created with correct ownerRef and metrics.
#[tokio::test]
async fn reconcile_creates_hpa() -> Result<(), Box<dyn std::error::Error>> {
    crate::controller::externalpodautoscaler::telemetry::Telemetry::init();

    let client = ClientBuilder::new()
        .with_resource::<ExternalPodAutoscaler>()
        .with_resource::<HorizontalPodAutoscaler>()
        .with_resource::<Lease>()
        .with_fixture_dir("tests/fixtures")
        .load_fixture_or_panic("epa-basic.yaml")
        .build()
        .await?;

    let epa_api = Api::<ExternalPodAutoscaler>::namespaced(client.clone(), "production");
    let epa = epa_api.get("queue-worker-scaler").await?;

    let (scraper_tx, _scraper_rx) = mpsc::channel::<EpaUpdate>(100);
    let metrics_store = MetricsStore::new();
    let epa_ownership = test_ownership(client.clone()).await?;

    let reconciler = Arc::new(Reconciler::new(
        client.clone(),
        scraper_tx,
        metrics_store,
        epa_ownership,
    ));
    let result = reconciler
        .reconcile(Arc::new(epa), reconciler.clone())
        .await;
    assert!(result.is_ok(), "reconcile failed: {:?}", result.err());

    let hpa_api = Api::<HorizontalPodAutoscaler>::namespaced(client, "production");
    let hpa = hpa_api.get("queue-worker-scaler").await?;

    assert_eq!(hpa.metadata.name.as_deref(), Some("queue-worker-scaler"));

    let owner_ref = hpa
        .metadata
        .owner_references
        .as_ref()
        .and_then(|refs| refs.first())
        .ok_or("HPA should have at least one ownerReference")?;
    assert_eq!(owner_ref.uid, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
    assert_eq!(owner_ref.name, "queue-worker-scaler");

    let spec = hpa.spec.as_ref().ok_or("HPA should have a spec")?;
    assert_eq!(spec.scale_target_ref.name, "worker");
    assert_eq!(spec.max_replicas, 5);

    let external_metric = spec
        .metrics
        .as_ref()
        .and_then(|m| m.first())
        .ok_or("HPA should have at least one metric")?
        .external
        .as_ref()
        .ok_or("metric should be External type")?;
    let expected_metric_name = "queue-worker-scaler-production-queue_depth";
    assert_eq!(external_metric.metric.name, expected_metric_name);

    Ok(())
}

// EPA changed — existing HPA should be replaced with updated spec.
#[tokio::test]
async fn reconcile_updates_existing_hpa() -> Result<(), Box<dyn std::error::Error>> {
    crate::controller::externalpodautoscaler::telemetry::Telemetry::init();

    let client = ClientBuilder::new()
        .with_resource::<ExternalPodAutoscaler>()
        .with_resource::<HorizontalPodAutoscaler>()
        .with_resource::<Lease>()
        .with_fixture_dir("tests/fixtures")
        .load_fixture_or_panic("epa-basic.yaml")
        .load_fixture_or_panic("hpa-stale.yaml")
        .build()
        .await?;

    let epa_api = Api::<ExternalPodAutoscaler>::namespaced(client.clone(), "production");
    let epa = epa_api.get("queue-worker-scaler").await?;

    let (scraper_tx, _scraper_rx) = mpsc::channel::<EpaUpdate>(100);
    let metrics_store = MetricsStore::new();
    let epa_ownership = test_ownership(client.clone()).await?;

    let reconciler = Arc::new(Reconciler::new(
        client.clone(),
        scraper_tx,
        metrics_store,
        epa_ownership,
    ));
    let result = reconciler
        .reconcile(Arc::new(epa), reconciler.clone())
        .await;
    assert!(result.is_ok(), "reconcile failed: {:?}", result.err());

    let hpa_api = Api::<HorizontalPodAutoscaler>::namespaced(client, "production");
    let updated_hpa = hpa_api.get("queue-worker-scaler").await?;

    let spec = updated_hpa.spec.as_ref().ok_or("HPA should have a spec")?;
    assert_eq!(
        spec.max_replicas, 5,
        "HPA maxReplicas should be updated to match EPA"
    );

    Ok(())
}

// Successful reconcile — EPA status should be patched with Ready=True condition.
#[tokio::test]
async fn reconcile_sets_ready_status() -> Result<(), Box<dyn std::error::Error>> {
    crate::controller::externalpodautoscaler::telemetry::Telemetry::init();

    let client = ClientBuilder::new()
        .with_resource::<ExternalPodAutoscaler>()
        .with_resource::<HorizontalPodAutoscaler>()
        .with_resource::<Lease>()
        .with_fixture_dir("tests/fixtures")
        .load_fixture_or_panic("epa-basic.yaml")
        .build()
        .await?;

    let epa_api = Api::<ExternalPodAutoscaler>::namespaced(client.clone(), "production");
    let epa = epa_api.get("queue-worker-scaler").await?;

    let (scraper_tx, _scraper_rx) = mpsc::channel::<EpaUpdate>(100);
    let metrics_store = MetricsStore::new();
    let epa_ownership = test_ownership(client.clone()).await?;

    let reconciler = Arc::new(Reconciler::new(
        client.clone(),
        scraper_tx,
        metrics_store,
        epa_ownership,
    ));
    let result = reconciler
        .reconcile(Arc::new(epa), reconciler.clone())
        .await;
    assert!(
        result.is_ok(),
        "reconcile should succeed without error: {:?}",
        result.err()
    );

    // Read back the EPA and verify the Ready status condition was patched.
    let updated_epa = epa_api.get("queue-worker-scaler").await?;

    let status = updated_epa
        .status
        .ok_or("EPA should have status after reconcile")?;
    let ready_condition = status
        .conditions
        .iter()
        .find(|c| c.type_ == "Ready")
        .ok_or("EPA status should have a Ready condition")?;
    assert_eq!(
        ready_condition.status, "True",
        "Ready condition should be True after successful reconcile"
    );

    Ok(())
}

// EPA with deletionTimestamp — scraper should receive EpaUpdate::Delete.
#[tokio::test]
async fn reconcile_deletion_notifies_scraper() -> Result<(), Box<dyn std::error::Error>> {
    crate::controller::externalpodautoscaler::telemetry::Telemetry::init();

    let client = ClientBuilder::new()
        .with_resource::<ExternalPodAutoscaler>()
        .with_resource::<HorizontalPodAutoscaler>()
        .with_resource::<Lease>()
        .with_fixture_dir("tests/fixtures")
        .load_fixture_or_panic("epa-deleting.yaml")
        .build()
        .await?;

    let epa_api = Api::<ExternalPodAutoscaler>::namespaced(client.clone(), "production");
    let epa = epa_api.get("queue-worker-scaler").await?;

    let (scraper_tx, mut scraper_rx) = mpsc::channel::<EpaUpdate>(100);
    let metrics_store = MetricsStore::new();
    let epa_ownership = test_ownership(client.clone()).await?;

    let reconciler = Arc::new(Reconciler::new(
        client.clone(),
        scraper_tx,
        metrics_store,
        epa_ownership,
    ));
    let result = reconciler
        .reconcile(Arc::new(epa), reconciler.clone())
        .await;
    assert!(result.is_ok(), "reconcile failed: {:?}", result.err());

    // The scraper channel should have received a Delete update.
    let message = tokio::time::timeout(std::time::Duration::from_secs(1), scraper_rx.recv())
        .await
        .map_err(|_| "timed out waiting for scraper message")?
        .ok_or("scraper channel was closed")?;

    match message {
        EpaUpdate::Delete {
            namespace: ns,
            name: n,
        } => {
            assert_eq!(ns, "production");
            assert_eq!(n, "queue-worker-scaler");
        }
        EpaUpdate::Upsert(_) => {
            return Err("expected EpaUpdate::Delete, got EpaUpdate::Upsert".into());
        }
    }

    Ok(())
}

// Successful reconcile should send EpaUpdate::Upsert to the scraper channel.
#[tokio::test]
async fn reconcile_notifies_scraper_on_upsert() -> Result<(), Box<dyn std::error::Error>> {
    crate::controller::externalpodautoscaler::telemetry::Telemetry::init();

    let client = ClientBuilder::new()
        .with_resource::<ExternalPodAutoscaler>()
        .with_resource::<HorizontalPodAutoscaler>()
        .with_resource::<Lease>()
        .with_fixture_dir("tests/fixtures")
        .load_fixture_or_panic("epa-basic.yaml")
        .build()
        .await?;

    let epa_api = Api::<ExternalPodAutoscaler>::namespaced(client.clone(), "production");
    let epa = epa_api.get("queue-worker-scaler").await?;

    let (scraper_tx, mut scraper_rx) = mpsc::channel::<EpaUpdate>(100);
    let metrics_store = MetricsStore::new();
    let epa_ownership = test_ownership(client.clone()).await?;

    let reconciler = Arc::new(Reconciler::new(
        client.clone(),
        scraper_tx,
        metrics_store,
        epa_ownership,
    ));
    let result = reconciler
        .reconcile(Arc::new(epa), reconciler.clone())
        .await;
    assert!(result.is_ok(), "reconcile failed: {:?}", result.err());

    // The scraper channel should have received an Upsert update.
    let message = tokio::time::timeout(std::time::Duration::from_secs(1), scraper_rx.recv())
        .await
        .map_err(|_| "timed out waiting for scraper message")?
        .ok_or("scraper channel was closed")?;

    match &message {
        EpaUpdate::Upsert(epa_arc) => {
            assert_eq!(
                epa_arc.metadata.name.as_deref(),
                Some("queue-worker-scaler"),
                "Upsert should contain the reconciled EPA"
            );
        }
        EpaUpdate::Delete { .. } => {
            return Err("expected EpaUpdate::Upsert, got EpaUpdate::Delete".into());
        }
    }

    Ok(())
}

// Non-owner replica deletion — scraper should NOT receive Delete notification.
#[tokio::test]
async fn reconcile_deletion_works_when_not_owner() -> Result<(), Box<dyn std::error::Error>> {
    crate::controller::externalpodautoscaler::telemetry::Telemetry::init();

    let client = ClientBuilder::new()
        .with_resource::<ExternalPodAutoscaler>()
        .with_resource::<HorizontalPodAutoscaler>()
        .with_resource::<Lease>()
        .with_fixture_dir("tests/fixtures")
        .load_fixture_or_panic("epa-deleting.yaml")
        .build()
        .await?;

    let epa_api = Api::<ExternalPodAutoscaler>::namespaced(client.clone(), "production");
    let epa = epa_api.get("queue-worker-scaler").await?;

    let (scraper_tx, mut scraper_rx) = mpsc::channel::<EpaUpdate>(100);
    let metrics_store = MetricsStore::new();
    let epa_ownership = test_ownership_not_owner(client.clone());

    let reconciler = Arc::new(Reconciler::new(
        client.clone(),
        scraper_tx,
        metrics_store,
        epa_ownership,
    ));
    let result = reconciler
        .reconcile(Arc::new(epa), reconciler.clone())
        .await;
    assert!(result.is_ok(), "reconcile failed: {:?}", result.err());

    // Non-owner should NOT send a Delete to the scraper.
    let recv = tokio::time::timeout(std::time::Duration::from_millis(100), scraper_rx.recv()).await;
    assert!(
        recv.is_err(),
        "non-owner should not send scraper messages on deletion"
    );

    Ok(())
}

// Non-owner replica — HPA should NOT be created, but scraper should still receive Upsert.
#[tokio::test]
async fn reconcile_skips_hpa_when_not_owner() -> Result<(), Box<dyn std::error::Error>> {
    crate::controller::externalpodautoscaler::telemetry::Telemetry::init();

    let client = ClientBuilder::new()
        .with_resource::<ExternalPodAutoscaler>()
        .with_resource::<HorizontalPodAutoscaler>()
        .with_resource::<Lease>()
        .with_fixture_dir("tests/fixtures")
        .load_fixture_or_panic("epa-basic.yaml")
        .build()
        .await?;

    let epa_api = Api::<ExternalPodAutoscaler>::namespaced(client.clone(), "production");
    let epa = epa_api.get("queue-worker-scaler").await?;

    let (scraper_tx, mut scraper_rx) = mpsc::channel::<EpaUpdate>(100);
    let metrics_store = MetricsStore::new();
    let epa_ownership = test_ownership_not_owner(client.clone());

    let reconciler = Arc::new(Reconciler::new(
        client.clone(),
        scraper_tx,
        metrics_store,
        epa_ownership,
    ));
    let result = reconciler
        .reconcile(Arc::new(epa), reconciler.clone())
        .await;
    assert!(result.is_ok(), "reconcile failed: {:?}", result.err());

    // HPA should NOT have been created.
    let hpa_api = Api::<HorizontalPodAutoscaler>::namespaced(client, "production");
    let hpa_result = hpa_api.get("queue-worker-scaler").await;
    assert!(
        hpa_result.is_err(),
        "HPA should not exist when replica is not the owner"
    );

    // Scraper should still receive Upsert even though HPA was not managed.
    let message = tokio::time::timeout(std::time::Duration::from_secs(1), scraper_rx.recv())
        .await
        .map_err(|_| "timed out waiting for scraper message")?
        .ok_or("scraper channel was closed")?;

    match &message {
        EpaUpdate::Upsert(epa_arc) => {
            assert_eq!(
                epa_arc.metadata.name.as_deref(),
                Some("queue-worker-scaler"),
                "Upsert should contain the reconciled EPA"
            );
        }
        EpaUpdate::Delete { .. } => {
            return Err("expected EpaUpdate::Upsert, got EpaUpdate::Delete".into());
        }
    }

    Ok(())
}
