use super::parser::{parse_labels, parse_prometheus_text};
use crate::store::MetricType;

#[test]
fn test_parse_simple_gauge() {
    let text = r#"
# TYPE memory_usage_bytes gauge
memory_usage_bytes 1048576
"#;
    let metrics = parse_prometheus_text(text).unwrap();
    assert_eq!(metrics.len(), 1);
    assert_eq!(metrics[0].name, "memory_usage_bytes");
    assert_eq!(metrics[0].value, 1048576.0);
    assert_eq!(metrics[0].metric_type, MetricType::Gauge);
    assert!(metrics[0].labels.is_empty());
}

#[test]
fn test_parse_counter_with_labels() {
    let text = r#"
# TYPE http_requests_total counter
http_requests_total{method="GET",endpoint="/api"} 123
http_requests_total{method="POST",endpoint="/api"} 45
"#;
    let metrics = parse_prometheus_text(text).unwrap();
    assert_eq!(metrics.len(), 2);

    assert_eq!(metrics[0].name, "http_requests_total");
    assert_eq!(metrics[0].value, 123.0);
    assert_eq!(metrics[0].metric_type, MetricType::Counter);
    assert_eq!(metrics[0].labels.get("method"), Some(&"GET".to_string()));
    assert_eq!(metrics[0].labels.get("endpoint"), Some(&"/api".to_string()));

    assert_eq!(metrics[1].name, "http_requests_total");
    assert_eq!(metrics[1].value, 45.0);
    assert_eq!(metrics[1].metric_type, MetricType::Counter);
    assert_eq!(metrics[1].labels.get("method"), Some(&"POST".to_string()));
}

#[test]
fn test_parse_mixed_metrics() {
    let text = r#"
# TYPE queue_depth gauge
queue_depth 42
queue_depth{priority="high"} 25
queue_depth{priority="low"} 17

# TYPE queue_errors_total counter
queue_errors_total 10
"#;
    let metrics = parse_prometheus_text(text).unwrap();
    assert_eq!(metrics.len(), 4);

    assert_eq!(metrics[0].name, "queue_depth");
    assert_eq!(metrics[0].value, 42.0);
    assert_eq!(metrics[0].metric_type, MetricType::Gauge);

    assert_eq!(metrics[1].name, "queue_depth");
    assert_eq!(metrics[1].value, 25.0);
    assert_eq!(metrics[1].labels.get("priority"), Some(&"high".to_string()));

    assert_eq!(metrics[3].name, "queue_errors_total");
    assert_eq!(metrics[3].metric_type, MetricType::Counter);
}

#[test]
fn test_parse_labels() {
    let labels_str = r#"method="GET",endpoint="/api",code="200""#;
    let labels = parse_labels(labels_str).unwrap();

    assert_eq!(labels.len(), 3);
    assert_eq!(labels.get("method"), Some(&"GET".to_string()));
    assert_eq!(labels.get("endpoint"), Some(&"/api".to_string()));
    assert_eq!(labels.get("code"), Some(&"200".to_string()));
}
