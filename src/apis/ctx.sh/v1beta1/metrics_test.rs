#[cfg(test)]
mod tests {
    use super::super::metric::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn test_metric_target_gauge_default_aggregation() {
        let json = json!({
            "metricName": "gauge_metric",
            "valueType": "gauge"
        });
        
        let target: MetricTarget = serde_json::from_value(json).unwrap();
        assert!(matches!(target.aggregation, AggregationType::Avg));
    }

    #[test]
    fn test_metric_target_counter_default_aggregation() {
        let json = json!({
            "metricName": "counter_metric",
            "valueType": "counter"
        });
        
        let target: MetricTarget = serde_json::from_value(json).unwrap();
        assert!(matches!(target.aggregation, AggregationType::Rate));
    }

    #[test]
    fn test_metric_target_explicit_aggregation_overrides_default() {
        let json = json!({
            "metricName": "test_metric",
            "valueType": "gauge",
            "aggregation": "sum"
        });
        
        let target: MetricTarget = serde_json::from_value(json).unwrap();
        assert!(matches!(target.aggregation, AggregationType::Sum));
        
        let json = json!({
            "metricName": "test_metric",
            "valueType": "counter",
            "aggregation": "max"
        });
        
        let target: MetricTarget = serde_json::from_value(json).unwrap();
        assert!(matches!(target.aggregation, AggregationType::Max));
    }

    #[test]
    fn test_metric_target_applies_all_defaults() {
        let json = json!({
            "metricName": "test_metric"
        });
        
        let target: MetricTarget = serde_json::from_value(json).unwrap();
        
        assert_eq!(target.port, 8080);
        assert_eq!(target.path, "/metrics");
        assert_eq!(target.interval_seconds, 30);
        assert_eq!(target.timeout_seconds, 10);
        assert_eq!(target.window_duration_seconds, 120);
        assert!(matches!(target.value_type, MetricValueType::Gauge));
        assert!(matches!(target.aggregation, AggregationType::Avg));
    }

    #[test]
    fn test_metric_target_partial_overrides() {
        let json = json!({
            "metricName": "custom_metric",
            "port": 9090,
            "intervalSeconds": 60,
            "valueType": "counter"
        });
        
        let target: MetricTarget = serde_json::from_value(json).unwrap();
        
        assert_eq!(target.port, 9090);
        assert_eq!(target.path, "/metrics");
        assert_eq!(target.interval_seconds, 60);
        assert_eq!(target.timeout_seconds, 10);
        assert!(matches!(target.value_type, MetricValueType::Counter));
        assert!(matches!(target.aggregation, AggregationType::Rate));
    }

    #[test]
    fn test_selector_operator_equality() {
        assert_eq!(SelectorOperator::In, SelectorOperator::In);
        assert_ne!(SelectorOperator::In, SelectorOperator::NotIn);
        assert_ne!(SelectorOperator::Exists, SelectorOperator::DoesNotExist);
    }

    #[test]
    fn test_selector_requirement() {
        let requirement = SelectorRequirement {
            key: "environment".to_string(),
            operator: SelectorOperator::In,
            values: Some(vec!["production".to_string(), "staging".to_string()]),
        };
        
        let json_value = serde_json::to_value(&requirement).unwrap();
        assert_eq!(json_value["key"], "environment");
        assert_eq!(json_value["operator"], "In");
        assert_eq!(json_value["values"][0], "production");
        
        let deserialized: SelectorRequirement = serde_json::from_value(json_value).unwrap();
        assert_eq!(deserialized.key, "environment");
    }

    #[test]
    fn test_metric_spec_selector() {
        // Test with matchLabels only
        let json = json!({
            "selector": {
                "matchLabels": {
                    "app": "myapp"
                }
            },
            "target": {
                "metricName": "test_metric"
            }
        });
        
        let spec: MetricSpec = serde_json::from_value(json).unwrap();
        assert!(spec.selector.match_labels.is_some());
        assert!(spec.selector.match_expressions.is_none());
        
        // Test with matchExpressions only
        let json_with_expressions = json!({
            "selector": {
                "matchExpressions": [{
                    "key": "environment",
                    "operator": "In",
                    "values": ["production", "staging"]
                }]
            },
            "target": {
                "metricName": "test_metric"
            }
        });
        
        let spec_with_expressions: MetricSpec = serde_json::from_value(json_with_expressions).unwrap();
        assert!(spec_with_expressions.selector.match_labels.is_none());
        assert!(spec_with_expressions.selector.match_expressions.is_some());
        
        // Test with both matchLabels and matchExpressions
        let json_with_both = json!({
            "selector": {
                "matchLabels": {
                    "app": "myapp"
                },
                "matchExpressions": [{
                    "key": "tier",
                    "operator": "Exists"
                }]
            },
            "target": {
                "metricName": "test_metric"
            }
        });
        
        let spec_with_both: MetricSpec = serde_json::from_value(json_with_both).unwrap();
        assert!(spec_with_both.selector.match_labels.is_some());
        assert!(spec_with_both.selector.match_expressions.is_some());
    }

    #[test]
    fn test_metric_status_default_values() {
        let status = MetricStatus::default();
        
        assert_eq!(status.total_targets, 0);
        assert_eq!(status.scraped_targets, 0);
        assert!(status.last_scrape_time.is_none());
    }

    #[test]
    fn test_metric_status_with_values() {
        let json = json!({
            "totalTargets": 5,
            "scrapedTargets": 4,
            "lastScrapeTime": "2024-01-01T12:00:00Z"
        });
        
        let status: MetricStatus = serde_json::from_value(json).unwrap();
        
        assert_eq!(status.total_targets, 5);
        assert_eq!(status.scraped_targets, 4);
        assert_eq!(status.last_scrape_time, Some("2024-01-01T12:00:00Z".to_string()));
    }
}