use std::sync::Arc;

use super::server::{Readiness, serve};

/// Starts the health server on an ephemeral port and returns its base URL.
async fn start(readiness: Arc<Readiness>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, readiness));
    format!("http://{addr}")
}

#[test]
fn test_pending_order() {
    let readiness = Readiness::default();
    assert_eq!(readiness.pending(), vec!["tls", "lease", "membership"]);
    readiness.mark_lease_registered();
    assert_eq!(readiness.pending(), vec!["tls", "membership"]);
}

#[tokio::test]
async fn test_healthz_ok_when_nothing_marked() {
    let base = start(Arc::new(Readiness::default())).await;

    let resp = reqwest::get(format!("{base}/healthz")).await.unwrap();

    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn test_readyz_follows_readiness() {
    let readiness = Arc::new(Readiness::default());
    let base = start(readiness.clone()).await;

    let resp = reqwest::get(format!("{base}/readyz")).await.unwrap();
    assert_eq!(resp.status(), 503);
    assert_eq!(
        resp.text().await.unwrap(),
        "not ready: tls, lease, membership"
    );

    readiness.mark_tls_loaded();
    readiness.mark_lease_registered();
    let resp = reqwest::get(format!("{base}/readyz")).await.unwrap();
    assert_eq!(resp.status(), 503);
    assert_eq!(resp.text().await.unwrap(), "not ready: membership");

    readiness.mark_membership_populated();
    let resp = reqwest::get(format!("{base}/readyz")).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn test_metrics_exposes_every_subsystem() {
    crate::controller::externalpodautoscaler::telemetry::Telemetry::global()
        .reconcile_errors
        .with_label_values(&["health-test", "ns", "boom"])
        .inc();
    crate::scraper::telemetry::Telemetry::global()
        .pods_scraped
        .with_label_values(&["health-test", "ns"])
        .inc();
    crate::webhook::metrics::telemetry::Telemetry::global()
        .api_requests
        .with_label_values(&["health-test", "metric", "200"])
        .inc();
    let base = start(Arc::new(Readiness::default())).await;

    let resp = reqwest::get(format!("{base}/metrics")).await.unwrap();

    assert_eq!(resp.status(), 200);
    let content_type = resp.headers()["content-type"].to_str().unwrap().to_string();
    assert!(
        content_type.starts_with("text/plain; version=0.0.4"),
        "unexpected content type: {content_type}"
    );
    let body = resp.text().await.unwrap();
    for series in [
        "epa_reconcile_errors_total{epa=\"health-test\"",
        "epa_pods_scraped_total{epa=\"health-test\"",
        "epa_api_requests_total{metric=\"metric\",namespace=\"health-test\"",
    ] {
        assert!(body.contains(series), "missing {series} in:\n{body}");
    }
}
