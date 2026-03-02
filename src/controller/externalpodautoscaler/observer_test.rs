use std::time::SystemTime;

use k8s_openapi::api::autoscaling::v2::HorizontalPodAutoscaler;
use kube_fake_client::ClientBuilder;

use crate::apis::ctx_sh::v1beta1::ExternalPodAutoscaler;
use crate::controller::externalpodautoscaler::observer::{ObservedState, StateObserver};

// Both EPA and HPA exist — ObservedState should have both fields populated.
#[tokio::test]
async fn observe_existing_epa_and_hpa() -> Result<(), Box<dyn std::error::Error>> {
    let client = ClientBuilder::new()
        .with_resource::<ExternalPodAutoscaler>()
        .with_resource::<HorizontalPodAutoscaler>()
        .with_fixture_dir("tests/fixtures")
        .load_fixture_or_panic("epa-basic.yaml")
        .load_fixture_or_panic("hpa-basic.yaml")
        .build()
        .await?;

    let observer = StateObserver::new(
        client,
        "production".to_string(),
        "queue-worker-scaler".to_string(),
    );

    let mut observed = ObservedState {
        epa: None,
        hpa: None,
        observe_time: SystemTime::UNIX_EPOCH,
    };

    observer.observe(&mut observed).await?;

    assert!(
        observed.epa_exists(),
        "epa should be populated when the resource exists"
    );
    assert!(
        observed.hpa_exists(),
        "hpa should be populated when both exist and the epa is not being deleted"
    );

    let observed_epa = observed
        .epa
        .as_ref()
        .ok_or("expected epa to be Some after observe")?;
    assert_eq!(
        observed_epa.metadata.name.as_deref(),
        Some("queue-worker-scaler"),
        "epa name should match"
    );
    assert_eq!(
        observed_epa.metadata.namespace.as_deref(),
        Some("production"),
        "epa namespace should match"
    );

    let observed_hpa = observed
        .hpa
        .as_ref()
        .ok_or("expected hpa to be Some after observe")?;
    assert_eq!(
        observed_hpa.metadata.name.as_deref(),
        Some("queue-worker-scaler"),
        "hpa name should match"
    );
    assert_eq!(
        observed_hpa.metadata.namespace.as_deref(),
        Some("production"),
        "hpa namespace should match"
    );

    assert_ne!(
        observed.observe_time,
        SystemTime::UNIX_EPOCH,
        "observe_time should be updated after a successful observation"
    );

    Ok(())
}

// EPA exists but no HPA — epa should be Some, hpa should be None.
#[tokio::test]
async fn observe_epa_without_hpa() -> Result<(), Box<dyn std::error::Error>> {
    let client = ClientBuilder::new()
        .with_resource::<ExternalPodAutoscaler>()
        .with_resource::<HorizontalPodAutoscaler>()
        .with_fixture_dir("tests/fixtures")
        .load_fixture_or_panic("epa-self-scaling.yaml")
        .build()
        .await?;

    let observer = StateObserver::new(client, "staging".to_string(), "api-scaler".to_string());

    let mut observed = ObservedState {
        epa: None,
        hpa: None,
        observe_time: SystemTime::UNIX_EPOCH,
    };

    observer.observe(&mut observed).await?;

    assert!(
        observed.epa_exists(),
        "epa should be populated when the resource exists"
    );
    assert!(
        !observed.hpa_exists(),
        "hpa should be None when no HPA exists for the given name"
    );

    let observed_epa = observed
        .epa
        .as_ref()
        .ok_or("expected epa to be Some after observe")?;
    assert_eq!(
        observed_epa.metadata.name.as_deref(),
        Some("api-scaler"),
        "epa name should match"
    );

    assert_ne!(
        observed.observe_time,
        SystemTime::UNIX_EPOCH,
        "observe_time should be updated after a successful observation"
    );

    Ok(())
}

// Neither EPA nor HPA exist — both fields should be None and no error should be returned.
#[tokio::test]
async fn observe_missing_epa() -> Result<(), Box<dyn std::error::Error>> {
    let client = ClientBuilder::new()
        .with_resource::<ExternalPodAutoscaler>()
        .with_resource::<HorizontalPodAutoscaler>()
        .build()
        .await?;

    let observer = StateObserver::new(
        client,
        "default".to_string(),
        "nonexistent-scaler".to_string(),
    );

    let initial_time = SystemTime::UNIX_EPOCH;
    let mut observed = ObservedState {
        epa: None,
        hpa: None,
        observe_time: initial_time,
    };

    observer.observe(&mut observed).await?;

    assert!(
        !observed.epa_exists(),
        "epa should be None when the resource does not exist"
    );
    assert!(
        !observed.hpa_exists(),
        "hpa should be None when the epa does not exist (observe returns early)"
    );

    assert_eq!(
        observed.observe_time, initial_time,
        "observe_time should not be updated when the epa is not found (early return path)"
    );

    Ok(())
}

// EPA has a deletionTimestamp — HPA lookup should be skipped even if an HPA exists.
#[tokio::test]
async fn observe_deleting_epa_skips_hpa() -> Result<(), Box<dyn std::error::Error>> {
    let client = ClientBuilder::new()
        .with_resource::<ExternalPodAutoscaler>()
        .with_resource::<HorizontalPodAutoscaler>()
        .with_fixture_dir("tests/fixtures")
        .load_fixture_or_panic("epa-deleting.yaml")
        .load_fixture_or_panic("hpa-basic.yaml")
        .build()
        .await?;

    let observer = StateObserver::new(
        client,
        "production".to_string(),
        "queue-worker-scaler".to_string(),
    );

    let mut observed = ObservedState {
        epa: None,
        hpa: None,
        observe_time: SystemTime::UNIX_EPOCH,
    };

    observer.observe(&mut observed).await?;

    assert!(
        observed.epa_exists(),
        "epa should be populated even when it has a deletionTimestamp"
    );

    let observed_epa = observed
        .epa
        .as_ref()
        .ok_or("expected epa to be Some after observe")?;
    assert!(
        observed_epa.metadata.deletion_timestamp.is_some(),
        "observed epa should retain the deletion_timestamp"
    );

    assert!(
        observed.is_deleting(),
        "is_deleting() should return true when deletionTimestamp is set"
    );

    assert!(
        !observed.hpa_exists(),
        "hpa should not be fetched when the epa has a deletionTimestamp set"
    );

    Ok(())
}
