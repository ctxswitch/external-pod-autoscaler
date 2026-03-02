use super::types::{CachedAggregation, LabeledSample, MetricConfig, MetricType, SampleKey};
use super::MetricsStore;
use crate::apis::ctx_sh::v1beta1::AggregationType;
use std::time::{Duration, Instant};

#[test]
fn test_cached_aggregation_validity() {
    let cached = CachedAggregation::new(42.0, Duration::from_millis(100));
    assert!(cached.is_valid());

    std::thread::sleep(Duration::from_millis(150));
    assert!(!cached.is_valid());
}

#[tokio::test]
async fn test_store_push_and_get() {
    let store = MetricsStore::new();
    let key = SampleKey::new(
        "default".to_string(),
        "test-epa".to_string(),
        "http_requests".to_string(),
        "pod-1".to_string(),
    );

    let sample = LabeledSample {
        value: 100.0,
        scraped_at: Instant::now(),
        success: true,
        metric_type: MetricType::Gauge,
    };

    store.push_sample(key, sample, 10).await;

    let windows = store.get_windows("default", "test-epa", "http_requests");
    assert_eq!(windows.len(), 1);

    // Lock the window to read samples
    let window = windows[0].1.read().await;
    assert_eq!(window.samples.len(), 1);
}

#[tokio::test]
async fn test_cache() {
    let store = MetricsStore::new();

    // No cached value initially
    let cached = store.get_cached("default", "test-epa", "http_requests");
    assert!(cached.is_none());

    // Store value
    store.cache_result(
        "default",
        "test-epa",
        "http_requests",
        42.0,
        Duration::from_secs(10),
    );

    // Should be cached now
    let cached = store.get_cached("default", "test-epa", "http_requests");
    assert_eq!(cached, Some(42.0));
}

// Push samples for 3 pods, get_windows returns all 3.
#[tokio::test]
async fn store_multi_pod_windows() {
    let store = MetricsStore::new();

    for i in 1..=3 {
        let key = SampleKey::new(
            "default".to_string(),
            "test-epa".to_string(),
            "http_requests".to_string(),
            format!("pod-{}", i),
        );
        let sample = LabeledSample {
            value: i as f64 * 10.0,
            scraped_at: Instant::now(),
            success: true,
            metric_type: MetricType::Gauge,
        };
        store.push_sample(key, sample, 10).await;
    }

    let windows = store.get_windows("default", "test-epa", "http_requests");
    assert_eq!(windows.len(), 3, "should have windows for 3 pods");

    let pod_names: Vec<&str> = windows.iter().map(|(name, _)| name.as_str()).collect();
    assert!(pod_names.contains(&"pod-1"));
    assert!(pod_names.contains(&"pod-2"));
    assert!(pod_names.contains(&"pod-3"));
}

// Push for two EPAs, remove one, verify only one remains.
#[tokio::test]
async fn store_remove_epa_windows() {
    let store = MetricsStore::new();

    // EPA 1
    let key1 = SampleKey::new(
        "default".to_string(),
        "epa-1".to_string(),
        "metric_a".to_string(),
        "pod-1".to_string(),
    );
    store
        .push_sample(
            key1,
            LabeledSample {
                value: 10.0,
                scraped_at: Instant::now(),
                success: true,
                metric_type: MetricType::Gauge,
            },
            10,
        )
        .await;

    // EPA 2
    let key2 = SampleKey::new(
        "default".to_string(),
        "epa-2".to_string(),
        "metric_b".to_string(),
        "pod-2".to_string(),
    );
    store
        .push_sample(
            key2,
            LabeledSample {
                value: 20.0,
                scraped_at: Instant::now(),
                success: true,
                metric_type: MetricType::Gauge,
            },
            10,
        )
        .await;

    // Also cache and config for EPA 1
    store.cache_result(
        "default",
        "epa-1",
        "metric_a",
        42.0,
        Duration::from_secs(10),
    );
    store.set_metric_config(
        "default",
        "epa-1",
        "metric_a",
        MetricConfig::new(AggregationType::Max, Duration::from_secs(120)),
    );

    // Remove EPA 1
    store.remove_epa_windows("default", "epa-1");

    // EPA 1 should be gone
    assert!(store.get_windows("default", "epa-1", "metric_a").is_empty());
    assert!(store.get_cached("default", "epa-1", "metric_a").is_none());

    // EPA 2 should still exist
    assert_eq!(store.get_windows("default", "epa-2", "metric_b").len(), 1);
}

// set_metric_config then get_metric_config, verify values.
#[tokio::test]
async fn store_metric_config_roundtrip() {
    let store = MetricsStore::new();

    let config = MetricConfig::new(AggregationType::Median, Duration::from_secs(300));
    store.set_metric_config("prod", "scaler", "queue_depth", config);

    let retrieved = store.get_metric_config("prod", "scaler", "queue_depth");
    assert!(
        matches!(retrieved.aggregation_type, AggregationType::Median),
        "aggregation type should be Median"
    );
    assert_eq!(retrieved.evaluation_period, Duration::from_secs(300));

    // Non-existent config should return defaults
    let default_config = store.get_metric_config("prod", "scaler", "nonexistent");
    assert!(matches!(
        default_config.aggregation_type,
        AggregationType::Avg
    ));
    assert_eq!(default_config.evaluation_period, Duration::from_secs(60));
}

// Cache with short TTL, sleep, cleanup removes expired entries.
#[tokio::test]
async fn store_cleanup_expired_cache() {
    let store = MetricsStore::new();

    // Short-lived cache entry
    store.cache_result(
        "default",
        "test-epa",
        "metric_1",
        42.0,
        Duration::from_millis(50),
    );

    // Long-lived cache entry
    store.cache_result(
        "default",
        "test-epa",
        "metric_2",
        99.0,
        Duration::from_secs(60),
    );

    // Both should be present initially
    assert!(store
        .get_cached("default", "test-epa", "metric_1")
        .is_some());
    assert!(store
        .get_cached("default", "test-epa", "metric_2")
        .is_some());

    // Wait for short-lived entry to expire
    tokio::time::sleep(Duration::from_millis(100)).await;

    let removed = store.cleanup_expired_cache();
    assert!(removed >= 1, "at least one entry should be cleaned up");

    // Short-lived should be gone
    assert!(store
        .get_cached("default", "test-epa", "metric_1")
        .is_none());
    // Long-lived should still be there
    assert!(store
        .get_cached("default", "test-epa", "metric_2")
        .is_some());
}

// Push samples at different times, get_samples_in_period filters correctly.
#[tokio::test]
async fn window_get_samples_in_period() {
    let store = MetricsStore::new();
    let key = SampleKey::new(
        "default".to_string(),
        "test-epa".to_string(),
        "metric_x".to_string(),
        "pod-1".to_string(),
    );

    // Push an old sample (120s ago)
    let old_sample = LabeledSample {
        value: 999.0,
        scraped_at: Instant::now() - Duration::from_secs(120),
        success: true,
        metric_type: MetricType::Gauge,
    };
    store.push_sample(key.clone(), old_sample, 100).await;

    // Push a recent sample (5s ago)
    let recent_sample = LabeledSample {
        value: 42.0,
        scraped_at: Instant::now() - Duration::from_secs(5),
        success: true,
        metric_type: MetricType::Gauge,
    };
    store.push_sample(key, recent_sample, 100).await;

    let windows = store.get_windows("default", "test-epa", "metric_x");
    assert_eq!(windows.len(), 1);

    let window = windows[0].1.read().await;
    assert_eq!(
        window.samples.len(),
        2,
        "window should have 2 total samples"
    );

    // Filter for last 60s
    let in_period = window.get_samples_in_period(Duration::from_secs(60));
    assert_eq!(
        in_period.len(),
        1,
        "only 1 sample should be within 60s period"
    );
    assert_eq!(in_period[0].value, 42.0);
}
