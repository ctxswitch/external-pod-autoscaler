use std::time::SystemTime;

use kube_fake_client::ClientBuilder;

use crate::apis::ctx_sh::v1beta1::ExternalPodAutoscaler;
use crate::controller::externalpodautoscaler::observer::{ObservedState, StateObserver};

// EPA exists — ObservedState should have the epa field populated.
#[tokio::test]
async fn observe_existing_epa() -> Result<(), Box<dyn std::error::Error>> {
    let client = ClientBuilder::new()
        .with_resource::<ExternalPodAutoscaler>()
        .with_fixture_dir("tests/fixtures")
        .load_fixture_or_panic("epa-basic.yaml")
        .build()
        .await?;

    let observer = StateObserver::new(
        client,
        "production".to_string(),
        "queue-worker-scaler".to_string(),
    );

    let mut observed = ObservedState {
        epa: None,
        observe_time: SystemTime::UNIX_EPOCH,
    };

    observer.observe(&mut observed).await?;

    assert!(
        observed.epa_exists(),
        "epa should be populated when the resource exists"
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

    assert_ne!(
        observed.observe_time,
        SystemTime::UNIX_EPOCH,
        "observe_time should be updated after a successful observation"
    );

    Ok(())
}

// EPA exists but uses a different fixture — epa should be Some.
#[tokio::test]
async fn observe_epa_self_scaling() -> Result<(), Box<dyn std::error::Error>> {
    let client = ClientBuilder::new()
        .with_resource::<ExternalPodAutoscaler>()
        .with_fixture_dir("tests/fixtures")
        .load_fixture_or_panic("epa-self-scaling.yaml")
        .build()
        .await?;

    let observer = StateObserver::new(client, "staging".to_string(), "api-scaler".to_string());

    let mut observed = ObservedState {
        epa: None,
        observe_time: SystemTime::UNIX_EPOCH,
    };

    observer.observe(&mut observed).await?;

    assert!(
        observed.epa_exists(),
        "epa should be populated when the resource exists"
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

// EPA does not exist — epa should be None and no error should be returned.
#[tokio::test]
async fn observe_missing_epa() -> Result<(), Box<dyn std::error::Error>> {
    let client = ClientBuilder::new()
        .with_resource::<ExternalPodAutoscaler>()
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
        observe_time: initial_time,
    };

    observer.observe(&mut observed).await?;

    assert!(
        !observed.epa_exists(),
        "epa should be None when the resource does not exist"
    );

    assert_eq!(
        observed.observe_time, initial_time,
        "observe_time should not be updated when the epa is not found (early return path)"
    );

    Ok(())
}

// EPA has a deletionTimestamp — is_deleting() should return true.
#[tokio::test]
async fn observe_deleting_epa() -> Result<(), Box<dyn std::error::Error>> {
    let client = ClientBuilder::new()
        .with_resource::<ExternalPodAutoscaler>()
        .with_fixture_dir("tests/fixtures")
        .load_fixture_or_panic("epa-deleting.yaml")
        .build()
        .await?;

    let observer = StateObserver::new(
        client,
        "production".to_string(),
        "queue-worker-scaler".to_string(),
    );

    let mut observed = ObservedState {
        epa: None,
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

    Ok(())
}
