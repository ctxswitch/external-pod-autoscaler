use std::sync::Arc;

use axum::body::Body;
use hyper::Request;
use tower::ServiceExt;

use crate::controller::membership::MembershipManager;
use crate::controller::work_assigner::WorkAssigner;
use crate::store::MetricsStore;
use crate::webhook::admission::{
    AdmissionRequest, AdmissionReview, GroupVersionKind, GroupVersionResource,
};
use crate::webhook::server::{build_router, AppState};

use kube_fake_client::ClientBuilder;

type TestResult = Result<(), Box<dyn std::error::Error>>;

async fn make_test_app_state() -> Result<AppState, Box<dyn std::error::Error>> {
    let client = ClientBuilder::new().build().await?;
    let membership = Arc::new(MembershipManager::new(
        client,
        "test-replica".to_string(),
        "10.0.0.1".to_string(),
        "test-pod".to_string(),
        "test-ns".to_string(),
        8443,
    ));
    let work_assigner = Arc::new(WorkAssigner::new(membership.clone()));
    let forward_client = reqwest::Client::new();

    Ok(AppState {
        metrics_store: Arc::new(MetricsStore::new()),
        work_assigner,
        membership,
        forward_client,
    })
}

fn make_admission_review(uid: &str, object: Option<serde_json::Value>) -> AdmissionReview {
    AdmissionReview {
        api_version: "admission.k8s.io/v1".to_string(),
        kind: "AdmissionReview".to_string(),
        request: Some(AdmissionRequest {
            uid: uid.to_string(),
            kind: GroupVersionKind {
                group: "ctx.sh".to_string(),
                version: "v1beta1".to_string(),
                kind: "ExternalPodAutoscaler".to_string(),
            },
            resource: GroupVersionResource {
                group: "ctx.sh".to_string(),
                version: "v1beta1".to_string(),
                resource: "externalpodautoscalers".to_string(),
            },
            operation: "CREATE".to_string(),
            object,
            old_object: None,
        }),
        response: None,
    }
}

fn valid_epa_json() -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "ctx.sh/v1beta1",
        "kind": "ExternalPodAutoscaler",
        "metadata": { "name": "test-epa", "namespace": "default" },
        "spec": {
            "minReplicas": 1,
            "maxReplicas": 5,
            "scrape": {
                "port": 8080,
                "path": "/metrics",
                "interval": "15s",
                "timeout": "1s",
                "scheme": "http",
                "evaluationPeriod": "60s",
                "aggregationType": "avg"
            },
            "scaleTargetRef": {
                "apiVersion": "apps/v1",
                "kind": "Deployment",
                "name": "worker"
            },
            "metrics": [{
                "metricName": "queue_depth",
                "type": "AverageValue",
                "targetValue": "50"
            }]
        }
    })
}

async fn post_validate(
    body: &AdmissionReview,
) -> Result<(u16, AdmissionReview), Box<dyn std::error::Error>> {
    let state = make_test_app_state().await?;
    let app = build_router(state, true);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/validate-ctx-sh-v1beta1-externalpodautoscaler")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(body)?))?,
        )
        .await?;

    let status = response.status().as_u16();
    let body_bytes = hyper::body::to_bytes(response.into_body()).await?;
    let review: AdmissionReview = serde_json::from_slice(&body_bytes)?;
    Ok((status, review))
}

async fn post_mutate(
    body: &AdmissionReview,
) -> Result<(u16, AdmissionReview), Box<dyn std::error::Error>> {
    let state = make_test_app_state().await?;
    let app = build_router(state, true);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mutate-ctx-sh-v1beta1-externalpodautoscaler")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(body)?))?,
        )
        .await?;

    let status = response.status().as_u16();
    let body_bytes = hyper::body::to_bytes(response.into_body()).await?;
    let review: AdmissionReview = serde_json::from_slice(&body_bytes)?;
    Ok((status, review))
}

// --- Validation tests ---

#[tokio::test]
async fn validate_valid_epa_returns_allowed() -> TestResult {
    let review = make_admission_review("test-uid-1", Some(valid_epa_json()));
    let (status, response) = post_validate(&review).await?;

    assert_eq!(status, 200);
    let resp = response.response.expect("should have response");
    assert!(resp.allowed, "valid EPA should be allowed");
    Ok(())
}

#[tokio::test]
async fn validate_invalid_min_replicas_denied() -> TestResult {
    let mut epa = valid_epa_json();
    epa["spec"]["minReplicas"] = serde_json::json!(0);

    let review = make_admission_review("test-uid-2", Some(epa));
    let (status, response) = post_validate(&review).await?;

    assert_eq!(status, 200);
    let resp = response.response.expect("should have response");
    assert!(!resp.allowed, "minReplicas=0 should be denied");
    assert!(
        resp.status.unwrap().message.contains("minReplicas"),
        "message should mention minReplicas"
    );
    Ok(())
}

#[tokio::test]
async fn validate_invalid_max_replicas_denied() -> TestResult {
    let mut epa = valid_epa_json();
    epa["spec"]["minReplicas"] = serde_json::json!(10);
    epa["spec"]["maxReplicas"] = serde_json::json!(5);

    let review = make_admission_review("test-uid-3", Some(epa));
    let (status, response) = post_validate(&review).await?;

    assert_eq!(status, 200);
    let resp = response.response.expect("should have response");
    assert!(!resp.allowed, "maxReplicas < minReplicas should be denied");
    assert!(
        resp.status.unwrap().message.contains("maxReplicas"),
        "message should mention maxReplicas"
    );
    Ok(())
}

#[tokio::test]
async fn validate_empty_metrics_denied() -> TestResult {
    let mut epa = valid_epa_json();
    epa["spec"]["metrics"] = serde_json::json!([]);

    let review = make_admission_review("test-uid-4", Some(epa));
    let (status, response) = post_validate(&review).await?;

    assert_eq!(status, 200);
    let resp = response.response.expect("should have response");
    assert!(!resp.allowed, "empty metrics should be denied");
    assert!(
        resp.status.unwrap().message.contains("metric"),
        "message should mention metrics"
    );
    Ok(())
}

#[tokio::test]
async fn validate_invalid_port_denied() -> TestResult {
    let mut epa = valid_epa_json();
    epa["spec"]["scrape"]["port"] = serde_json::json!(0);

    let review = make_admission_review("test-uid-5", Some(epa));
    let (status, response) = post_validate(&review).await?;

    assert_eq!(status, 200);
    let resp = response.response.expect("should have response");
    assert!(!resp.allowed, "port=0 should be denied");
    assert!(
        resp.status.unwrap().message.contains("port"),
        "message should mention port"
    );
    Ok(())
}

#[tokio::test]
async fn validate_path_traversal_denied() -> TestResult {
    let mut epa = valid_epa_json();
    epa["spec"]["scrape"]["path"] = serde_json::json!("/metrics/../etc/passwd");

    let review = make_admission_review("test-uid-6", Some(epa));
    let (status, response) = post_validate(&review).await?;

    assert_eq!(status, 200);
    let resp = response.response.expect("should have response");
    assert!(!resp.allowed, "path traversal should be denied");
    assert!(
        resp.status.unwrap().message.contains(".."),
        "message should mention path traversal"
    );
    Ok(())
}

#[tokio::test]
async fn validate_invalid_duration_denied() -> TestResult {
    let mut epa = valid_epa_json();
    epa["spec"]["scrape"]["interval"] = serde_json::json!("not-a-duration");

    let review = make_admission_review("test-uid-7", Some(epa));
    let (status, response) = post_validate(&review).await?;

    assert_eq!(status, 200);
    let resp = response.response.expect("should have response");
    assert!(!resp.allowed, "invalid duration should be denied");
    Ok(())
}

#[tokio::test]
async fn validate_missing_request_returns_400() -> TestResult {
    let review = AdmissionReview {
        api_version: "admission.k8s.io/v1".to_string(),
        kind: "AdmissionReview".to_string(),
        request: None,
        response: None,
    };

    let state = make_test_app_state().await?;
    let app = build_router(state, true);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/validate-ctx-sh-v1beta1-externalpodautoscaler")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&review)?))?,
        )
        .await?;

    assert_eq!(
        response.status().as_u16(),
        400,
        "missing request should return 400"
    );
    Ok(())
}

// --- Mutation tests ---

#[tokio::test]
async fn mutate_applies_defaults() -> TestResult {
    let epa = serde_json::json!({
        "apiVersion": "ctx.sh/v1beta1",
        "kind": "ExternalPodAutoscaler",
        "metadata": { "name": "test-epa", "namespace": "default" },
        "spec": {
            "minReplicas": 1,
            "maxReplicas": 5,
            "scrape": {
                "port": 0,
                "path": "",
                "interval": "",
                "timeout": "",
                "scheme": "",
                "evaluationPeriod": "",
                "aggregationType": "avg"
            },
            "scaleTargetRef": {
                "apiVersion": "apps/v1",
                "kind": "Deployment",
                "name": "worker"
            },
            "metrics": [{
                "metricName": "queue_depth",
                "type": "AverageValue",
                "targetValue": "50"
            }]
        }
    });

    let review = make_admission_review("mutate-uid-1", Some(epa));
    let (status, response) = post_mutate(&review).await?;

    assert_eq!(status, 200);
    let resp = response.response.expect("should have response");
    assert!(resp.allowed, "mutation should allow the object");
    assert!(
        resp.patch.is_some(),
        "mutation should produce a patch for defaults"
    );
    assert_eq!(
        resp.patch_type.as_deref(),
        Some("JSONPatch"),
        "patch type should be JSONPatch"
    );
    Ok(())
}

#[tokio::test]
async fn mutate_sets_scrape_target_ref() -> TestResult {
    let review = make_admission_review("mutate-uid-2", Some(valid_epa_json()));
    let (status, response) = post_mutate(&review).await?;

    assert_eq!(status, 200);
    let resp = response.response.expect("should have response");
    assert!(resp.allowed);
    assert!(
        resp.patch.is_some(),
        "should produce patch to set scrapeTargetRef"
    );
    Ok(())
}

#[tokio::test]
async fn mutate_no_changes_no_patch() -> TestResult {
    let mut epa = valid_epa_json();
    epa["spec"]["scrapeTargetRef"] = serde_json::json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "name": "worker"
    });

    let review = make_admission_review("mutate-uid-3", Some(epa));
    let (status, response) = post_mutate(&review).await?;

    assert_eq!(status, 200);
    let resp = response.response.expect("should have response");
    assert!(resp.allowed);
    assert!(
        resp.patch.is_none(),
        "fully specified EPA should not produce a patch"
    );
    Ok(())
}

#[tokio::test]
async fn mutate_missing_object_returns_allowed() -> TestResult {
    let review = make_admission_review("mutate-uid-4", None);
    let (status, response) = post_mutate(&review).await?;

    assert_eq!(status, 200);
    let resp = response.response.expect("should have response");
    assert!(resp.allowed, "missing object should still be allowed");
    assert!(resp.patch.is_none(), "no object means no patch");
    Ok(())
}
