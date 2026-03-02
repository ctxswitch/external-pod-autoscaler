use super::handlers::parse_external_metric_name;

#[test]
fn test_parse_external_metric_name() {
    let (epa_name, epa_namespace, metric_name) =
        parse_external_metric_name("production", "queue-worker-scaler-production-queue_depth")
            .unwrap();

    assert_eq!(epa_name, "queue-worker-scaler");
    assert_eq!(epa_namespace, "production");
    assert_eq!(metric_name, "queue_depth");
}

#[test]
fn test_parse_external_metric_name_with_underscores() {
    let (epa_name, epa_namespace, metric_name) =
        parse_external_metric_name("staging", "api-scaler-staging-http_requests_total").unwrap();

    assert_eq!(epa_name, "api-scaler");
    assert_eq!(epa_namespace, "staging");
    assert_eq!(metric_name, "http_requests_total");
}

#[test]
fn test_parse_external_metric_name_hyphenated_namespace() {
    // This was the critical bug: hyphenated namespaces like "my-namespace"
    let (epa_name, epa_namespace, metric_name) =
        parse_external_metric_name("my-namespace", "queue-scaler-my-namespace-queue_depth")
            .unwrap();

    assert_eq!(epa_name, "queue-scaler");
    assert_eq!(epa_namespace, "my-namespace");
    assert_eq!(metric_name, "queue_depth");
}

#[test]
fn test_parse_external_metric_name_complex_hyphenated_namespace() {
    // Multiple hyphens in both EPA name and namespace
    let (epa_name, epa_namespace, metric_name) = parse_external_metric_name(
        "kube-system",
        "my-queue-worker-scaler-kube-system-http_requests_total",
    )
    .unwrap();

    assert_eq!(epa_name, "my-queue-worker-scaler");
    assert_eq!(epa_namespace, "kube-system");
    assert_eq!(metric_name, "http_requests_total");
}

#[test]
fn test_parse_external_metric_name_single_word_metric() {
    let (epa_name, epa_namespace, metric_name) =
        parse_external_metric_name("default", "scaler-default-requests").unwrap();

    assert_eq!(epa_name, "scaler");
    assert_eq!(epa_namespace, "default");
    assert_eq!(metric_name, "requests");
}

#[test]
fn test_parse_external_metric_name_no_hyphens() {
    let result = parse_external_metric_name("default", "nohyphens");
    assert!(result.is_err());
}

#[test]
fn test_parse_external_metric_name_wrong_namespace() {
    // Namespace in URL doesn't match what's in the metric name
    let result = parse_external_metric_name("other-ns", "scaler-production-queue_depth");
    assert!(result.is_err());
}
