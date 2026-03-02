use k8s_openapi::api::core::v1::Pod;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::apis::ctx_sh::v1beta1::{ExternalPodAutoscaler, MetricSpec, MetricTargetType};
use crate::scraper::worker::Worker;
use crate::store::{MetricType, MetricsStore};

fn load_base_epa() -> ExternalPodAutoscaler {
    let content = std::fs::read_to_string("tests/fixtures/epa-basic.yaml")
        .expect("failed to read epa-basic.yaml fixture");
    serde_yaml::from_str(&content).expect("failed to parse epa-basic.yaml fixture")
}

fn load_base_pod() -> Pod {
    let content = std::fs::read_to_string("tests/fixtures/pod-ready.yaml")
        .expect("failed to read pod-ready.yaml fixture");
    serde_yaml::from_str(&content).expect("failed to parse pod-ready.yaml fixture")
}

fn make_scrape_epa(name: &str, namespace: &str, port: u16) -> ExternalPodAutoscaler {
    let mut epa = load_base_epa();
    epa.metadata.name = Some(name.to_string());
    epa.metadata.namespace = Some(namespace.to_string());
    epa.metadata.uid = Some(format!("{}-uid", name));
    epa.spec.scrape.port = port as i32;
    epa.spec.scrape.timeout = "2s".to_string();
    epa
}

fn make_scrape_pod(name: &str, namespace: &str, ip: &str) -> Pod {
    let mut pod = load_base_pod();
    pod.metadata.name = Some(name.to_string());
    pod.metadata.namespace = Some(namespace.to_string());
    if let Some(ref mut status) = pod.status {
        status.pod_ip = Some(ip.to_string());
    }
    pod
}

// Serve a gauge metric and verify it's stored correctly.
#[tokio::test]
async fn scrape_gauge_metric_stored() {
    let server = MockServer::start().await;
    let port = server.address().port();

    Mock::given(method("GET"))
        .and(path("/metrics"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("# TYPE queue_depth gauge\nqueue_depth 42\n"),
        )
        .mount(&server)
        .await;

    let epa = make_scrape_epa("test-epa", "default", port);
    let pod = make_scrape_pod("pod-1", "default", "127.0.0.1");
    let store = MetricsStore::new();
    let client = reqwest::Client::new();

    let result = Worker::scrape_pod_static(&client, &epa, &pod, &store, 10).await;
    assert!(result.is_ok(), "scrape should succeed: {:?}", result.err());

    let windows = store.get_windows("default", "test-epa", "queue_depth");
    assert_eq!(windows.len(), 1, "should have one window for pod-1");

    let window = windows[0].1.read().await;
    assert_eq!(window.samples.len(), 1);
    assert_eq!(window.samples[0].value, 42.0);
    assert_eq!(window.samples[0].metric_type, MetricType::Gauge);
    assert!(window.samples[0].success);
}

// Serve a counter metric and verify MetricType::Counter.
#[tokio::test]
async fn scrape_counter_metric_stored() {
    let server = MockServer::start().await;
    let port = server.address().port();

    Mock::given(method("GET"))
        .and(path("/metrics"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("# TYPE http_requests_total counter\nhttp_requests_total 1234\n"),
        )
        .mount(&server)
        .await;

    let mut epa = make_scrape_epa("test-epa", "default", port);
    epa.spec.metrics = vec![MetricSpec {
        metric_name: "http_requests_total".to_string(),
        type_: MetricTargetType::AverageValue,
        target_value: "100".to_string(),
        aggregation_type: None,
        evaluation_period: None,
        label_selector: None,
    }];

    let pod = make_scrape_pod("pod-1", "default", "127.0.0.1");
    let store = MetricsStore::new();
    let client = reqwest::Client::new();

    let result = Worker::scrape_pod_static(&client, &epa, &pod, &store, 10).await;
    assert!(result.is_ok());

    let windows = store.get_windows("default", "test-epa", "http_requests_total");
    assert_eq!(windows.len(), 1);

    let window = windows[0].1.read().await;
    assert_eq!(window.samples[0].value, 1234.0);
    assert_eq!(window.samples[0].metric_type, MetricType::Counter);
}

// Serve two metrics, both should be stored.
#[tokio::test]
async fn scrape_multiple_metrics() {
    let server = MockServer::start().await;
    let port = server.address().port();

    Mock::given(method("GET"))
        .and(path("/metrics"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "# TYPE queue_depth gauge\nqueue_depth 10\n# TYPE queue_errors_total counter\nqueue_errors_total 3\n",
        ))
        .mount(&server)
        .await;

    let mut epa = make_scrape_epa("test-epa", "default", port);
    epa.spec.metrics = vec![
        MetricSpec {
            metric_name: "queue_depth".to_string(),
            type_: MetricTargetType::AverageValue,
            target_value: "50".to_string(),
            aggregation_type: None,
            evaluation_period: None,
            label_selector: None,
        },
        MetricSpec {
            metric_name: "queue_errors_total".to_string(),
            type_: MetricTargetType::AverageValue,
            target_value: "5".to_string(),
            aggregation_type: None,
            evaluation_period: None,
            label_selector: None,
        },
    ];

    let pod = make_scrape_pod("pod-1", "default", "127.0.0.1");
    let store = MetricsStore::new();
    let client = reqwest::Client::new();

    let result = Worker::scrape_pod_static(&client, &epa, &pod, &store, 10).await;
    assert!(result.is_ok());

    let depth_windows = store.get_windows("default", "test-epa", "queue_depth");
    assert_eq!(depth_windows.len(), 1);

    let errors_windows = store.get_windows("default", "test-epa", "queue_errors_total");
    assert_eq!(errors_windows.len(), 1);
}

// Serve metric with labels and EPA label selector — only matching stored.
#[tokio::test]
async fn scrape_with_labels_filtered() {
    let server = MockServer::start().await;
    let port = server.address().port();

    Mock::given(method("GET"))
        .and(path("/metrics"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "# TYPE queue_depth gauge\nqueue_depth{priority=\"high\"} 42\nqueue_depth{priority=\"low\"} 10\n",
        ))
        .mount(&server)
        .await;

    let mut epa = make_scrape_epa("test-epa", "default", port);
    epa.spec.metrics[0].label_selector = Some(crate::apis::ctx_sh::v1beta1::LabelSelector {
        match_labels: Some([("priority".to_string(), "high".to_string())].into()),
        match_expressions: None,
    });

    let pod = make_scrape_pod("pod-1", "default", "127.0.0.1");
    let store = MetricsStore::new();
    let client = reqwest::Client::new();

    let result = Worker::scrape_pod_static(&client, &epa, &pod, &store, 10).await;
    assert!(result.is_ok());

    let windows = store.get_windows("default", "test-epa", "queue_depth");
    assert_eq!(windows.len(), 1);

    let window = windows[0].1.read().await;
    // Only the "high" priority label should match
    assert_eq!(window.samples.len(), 1);
    assert_eq!(window.samples[0].value, 42.0);
}

// Mock delays beyond EPA timeout — should return an error.
#[tokio::test]
async fn scrape_timeout_returns_error() {
    let server = MockServer::start().await;
    let port = server.address().port();

    Mock::given(method("GET"))
        .and(path("/metrics"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("queue_depth 42\n")
                .set_delay(std::time::Duration::from_secs(5)),
        )
        .mount(&server)
        .await;

    let mut epa = make_scrape_epa("test-epa", "default", port);
    epa.spec.scrape.timeout = "100ms".to_string();

    let pod = make_scrape_pod("pod-1", "default", "127.0.0.1");
    let store = MetricsStore::new();
    let client = reqwest::Client::new();

    let result = Worker::scrape_pod_static(&client, &epa, &pod, &store, 10).await;
    assert!(result.is_err(), "should timeout");
}

// Mock returns 404 — should return an error.
#[tokio::test]
async fn scrape_404_returns_error() {
    let server = MockServer::start().await;
    let port = server.address().port();

    // No mock registered for /metrics, so wiremock returns 404
    let epa = make_scrape_epa("test-epa", "default", port);
    let pod = make_scrape_pod("pod-1", "default", "127.0.0.1");
    let store = MetricsStore::new();
    let client = reqwest::Client::new();

    let result = Worker::scrape_pod_static(&client, &epa, &pod, &store, 10).await;
    let err = result.expect_err("404 response should return an error");
    assert!(
        err.to_string().contains("HTTP 404"),
        "error should mention HTTP 404, got: {err}"
    );
}

// Mock returns garbage text — parser should fail or no metrics stored.
#[tokio::test]
async fn scrape_invalid_prometheus_format() {
    let server = MockServer::start().await;
    let port = server.address().port();

    Mock::given(method("GET"))
        .and(path("/metrics"))
        .respond_with(ResponseTemplate::new(200).set_body_string("this is not prometheus\n{{}}\n"))
        .mount(&server)
        .await;

    let epa = make_scrape_epa("test-epa", "default", port);
    let pod = make_scrape_pod("pod-1", "default", "127.0.0.1");
    let store = MetricsStore::new();
    let client = reqwest::Client::new();

    // 200 with garbage body — parser may return empty results or error,
    // but either way no metric should be stored.
    let _ = Worker::scrape_pod_static(&client, &epa, &pod, &store, 10).await;
    let windows = store.get_windows("default", "test-epa", "queue_depth");
    assert!(
        windows.is_empty(),
        "no windows should be created for invalid prometheus output"
    );
}

// Pod with no podIP should return an error.
#[tokio::test]
async fn scrape_pod_missing_ip_returns_error() {
    let epa = make_scrape_epa("test-epa", "default", 9090);
    let mut pod = Pod::default();
    pod.metadata.name = Some("no-ip-pod".to_string());
    pod.metadata.namespace = Some("default".to_string());
    // No status/podIP set

    let store = MetricsStore::new();
    let client = reqwest::Client::new();

    let result = Worker::scrape_pod_static(&client, &epa, &pod, &store, 10).await;
    let err = result.expect_err("pod without IP should fail");
    assert!(
        err.to_string().contains("no IP"),
        "error should mention missing IP, got: {err}"
    );
}
