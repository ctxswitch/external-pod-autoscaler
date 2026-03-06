use super::pod_cache::*;
use k8s_openapi::api::core::v1::Pod;
use std::sync::Arc;

#[test]
fn parse_label_selector_basic() {
    let pairs = parse_label_selector("app=worker,tier=backend");
    assert_eq!(pairs.len(), 2);
    assert_eq!(pairs[0], ("app".to_string(), "worker".to_string()));
    assert_eq!(pairs[1], ("tier".to_string(), "backend".to_string()));
}

#[test]
fn parse_label_selector_empty() {
    let pairs = parse_label_selector("");
    assert!(pairs.is_empty());
}

#[test]
fn parse_label_selector_single() {
    let pairs = parse_label_selector("app=queue");
    assert_eq!(pairs, vec![("app".to_string(), "queue".to_string())]);
}

#[test]
fn parse_label_selector_value_with_equals() {
    // Values that themselves contain '=' should be preserved.
    let pairs = parse_label_selector("key=val=ue");
    assert_eq!(pairs, vec![("key".to_string(), "val=ue".to_string())]);
}

fn make_pod(
    phase: Option<&str>,
    pod_ip: Option<&str>,
    ready: Option<&str>,
    labels: Option<Vec<(&str, &str)>>,
) -> Arc<Pod> {
    use k8s_openapi::api::core::v1::{PodCondition, PodStatus};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

    let conditions = ready.map(|status| {
        vec![PodCondition {
            type_: "Ready".to_string(),
            status: status.to_string(),
            ..Default::default()
        }]
    });

    Arc::new(Pod {
        metadata: ObjectMeta {
            labels: labels.map(|ls| {
                ls.into_iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect()
            }),
            ..Default::default()
        },
        status: Some(PodStatus {
            phase: phase.map(str::to_string),
            pod_ip: pod_ip.map(str::to_string),
            conditions,
            ..Default::default()
        }),
        ..Default::default()
    })
}

#[test]
fn is_pod_ready_running_with_ip_and_ready() {
    let pod = make_pod(Some("Running"), Some("10.0.0.1"), Some("True"), None);
    assert!(is_pod_ready(&pod));
}

#[test]
fn is_pod_ready_pending_phase() {
    let pod = make_pod(Some("Pending"), Some("10.0.0.1"), Some("True"), None);
    assert!(!is_pod_ready(&pod));
}

#[test]
fn is_pod_ready_no_ip() {
    let pod = make_pod(Some("Running"), None, Some("True"), None);
    assert!(!is_pod_ready(&pod));
}

#[test]
fn is_pod_ready_condition_false() {
    let pod = make_pod(Some("Running"), Some("10.0.0.1"), Some("False"), None);
    assert!(!is_pod_ready(&pod));
}

#[test]
fn is_pod_ready_terminating() {
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
    let mut pod = make_pod(Some("Running"), Some("10.0.0.1"), Some("True"), None);
    Arc::make_mut(&mut pod).metadata.deletion_timestamp =
        Some(Time(k8s_openapi::jiff::Timestamp::now()));
    assert!(!is_pod_ready(&pod));
}

#[test]
fn pod_matches_selector_all_labels_present() {
    let pod = make_pod(
        Some("Running"),
        Some("10.0.0.1"),
        Some("True"),
        Some(vec![("app", "worker"), ("tier", "backend")]),
    );
    let pairs = parse_label_selector("app=worker,tier=backend");
    assert!(pod_matches_selector(&pod, &pairs));
}

#[test]
fn pod_matches_selector_missing_label() {
    let pod = make_pod(
        Some("Running"),
        Some("10.0.0.1"),
        Some("True"),
        Some(vec![("app", "worker")]),
    );
    let pairs = parse_label_selector("app=worker,tier=backend");
    assert!(!pod_matches_selector(&pod, &pairs));
}

#[test]
fn pod_matches_selector_wrong_value() {
    let pod = make_pod(
        Some("Running"),
        Some("10.0.0.1"),
        Some("True"),
        Some(vec![("app", "other")]),
    );
    let pairs = parse_label_selector("app=worker");
    assert!(!pod_matches_selector(&pod, &pairs));
}

#[test]
fn pod_matches_selector_empty_selector() {
    let pod = make_pod(Some("Running"), Some("10.0.0.1"), Some("True"), None);
    assert!(pod_matches_selector(&pod, &[]));
}
