use super::worker::{build_label_selector_string, get_pod_selector};
use crate::apis::ctx_sh::v1beta1::TargetRef;
use k8s_openapi::api::apps::v1::{DaemonSet, Deployment, StatefulSet};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, LabelSelectorRequirement};
use kube_fake_client::ClientBuilder;
use std::collections::BTreeMap;

fn make_target_ref(kind: &str, name: &str) -> TargetRef {
    TargetRef {
        api_version: "apps/v1".to_string(),
        kind: kind.to_string(),
        name: name.to_string(),
    }
}

// Deployment with matchLabels `app=worker` — get_pod_selector should return "app=worker".
#[tokio::test]
async fn get_pod_selector_deployment() -> Result<(), Box<dyn std::error::Error>> {
    let client = ClientBuilder::new()
        .with_resource::<Deployment>()
        .with_fixture_dir("tests/fixtures")
        .load_fixture_or_panic("deployment-worker.yaml")
        .build()
        .await?;

    let target_ref = make_target_ref("Deployment", "worker");
    let selector = get_pod_selector(&client, "default", &target_ref).await?;

    assert_eq!(selector, "app=worker");

    Ok(())
}

// StatefulSet with matchLabels `app=cache` — get_pod_selector should return "app=cache".
#[tokio::test]
async fn get_pod_selector_statefulset() -> Result<(), Box<dyn std::error::Error>> {
    let client = ClientBuilder::new()
        .with_resource::<StatefulSet>()
        .with_fixture_dir("tests/fixtures")
        .load_fixture_or_panic("statefulset-cache.yaml")
        .build()
        .await?;

    let target_ref = make_target_ref("StatefulSet", "cache");
    let selector = get_pod_selector(&client, "default", &target_ref).await?;

    assert_eq!(selector, "app=cache");

    Ok(())
}

// DaemonSet with matchLabels `app=agent` — get_pod_selector should return "app=agent".
#[tokio::test]
async fn get_pod_selector_daemonset() -> Result<(), Box<dyn std::error::Error>> {
    let client = ClientBuilder::new()
        .with_resource::<DaemonSet>()
        .with_fixture_dir("tests/fixtures")
        .load_fixture_or_panic("daemonset-agent.yaml")
        .build()
        .await?;

    let target_ref = make_target_ref("DaemonSet", "agent");
    let selector = get_pod_selector(&client, "default", &target_ref).await?;

    assert_eq!(selector, "app=agent");

    Ok(())
}

// Unsupported kind — get_pod_selector should return an error.
#[tokio::test]
async fn get_pod_selector_unsupported_kind() -> Result<(), Box<dyn std::error::Error>> {
    let client = ClientBuilder::new().build().await?;

    let target_ref = make_target_ref("CronJob", "my-job");
    let result = get_pod_selector(&client, "default", &target_ref).await;

    assert!(
        result.is_err(),
        "expected error for unsupported kind, got Ok"
    );
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("Unsupported target kind"),
        "error message should mention unsupported kind, got: {msg}"
    );

    Ok(())
}

// Deployment does not exist — get_pod_selector should return an error.
#[tokio::test]
async fn get_pod_selector_not_found() -> Result<(), Box<dyn std::error::Error>> {
    let client = ClientBuilder::new()
        .with_resource::<Deployment>()
        .build()
        .await?;

    let target_ref = make_target_ref("Deployment", "nonexistent");
    let result = get_pod_selector(&client, "default", &target_ref).await;

    assert!(
        result.is_err(),
        "expected error when deployment does not exist"
    );

    Ok(())
}

// LabelSelector with multiple matchLabels — both pairs should appear in the output.
#[test]
fn build_label_selector_string_match_labels() {
    let mut labels = BTreeMap::new();
    labels.insert("app".to_string(), "worker".to_string());
    labels.insert("tier".to_string(), "backend".to_string());

    let selector = LabelSelector {
        match_labels: Some(labels),
        match_expressions: None,
    };

    let result = build_label_selector_string(&selector).expect("should succeed");

    // BTreeMap iterates in sorted key order, so both pairs must be present.
    assert!(
        result.contains("app=worker"),
        "output should contain app=worker, got: {result}"
    );
    assert!(
        result.contains("tier=backend"),
        "output should contain tier=backend, got: {result}"
    );
}

// LabelSelector with a single-value `In` matchExpression — should convert to `key=value`.
#[test]
fn build_label_selector_string_match_expressions_in() {
    let selector = LabelSelector {
        match_labels: None,
        match_expressions: Some(vec![LabelSelectorRequirement {
            key: "app".to_string(),
            operator: "In".to_string(),
            values: Some(vec!["worker".to_string()]),
        }]),
    };

    let result = build_label_selector_string(&selector).expect("should succeed");

    assert_eq!(result, "app=worker");
}

// LabelSelector with an unsupported operator — should return an error.
#[test]
fn build_label_selector_string_unsupported_operator() {
    let selector = LabelSelector {
        match_labels: None,
        match_expressions: Some(vec![LabelSelectorRequirement {
            key: "app".to_string(),
            operator: "NotIn".to_string(),
            values: Some(vec!["worker".to_string()]),
        }]),
    };

    let result = build_label_selector_string(&selector);

    assert!(
        result.is_err(),
        "expected error for unsupported operator, got Ok"
    );
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("not supported"),
        "error should mention unsupported operator, got: {msg}"
    );
}

// Empty LabelSelector with no matchLabels and no matchExpressions — should return an error.
#[test]
fn build_label_selector_string_empty() {
    let selector = LabelSelector {
        match_labels: None,
        match_expressions: None,
    };

    let result = build_label_selector_string(&selector);

    assert!(result.is_err(), "expected error for empty selector, got Ok");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("no usable label constraints"),
        "error should mention no usable label constraints, got: {msg}"
    );
}
