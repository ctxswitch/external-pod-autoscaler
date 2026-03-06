use super::aggregation::{aggregate_metric, aggregate_samples};
use crate::apis::ctx_sh::v1beta1::AggregationType;
use crate::store::{LabeledSample, MetricType, MetricWindow};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

fn create_sample(value: f64, metric_type: MetricType) -> LabeledSample {
    LabeledSample {
        value,
        scraped_at: Instant::now(),
        success: true,
        metric_type,
    }
}

#[test]
fn test_aggregate_avg() {
    let samples = [
        create_sample(10.0, MetricType::Gauge),
        create_sample(20.0, MetricType::Gauge),
        create_sample(30.0, MetricType::Gauge),
    ];
    let sample_refs: Vec<&LabeledSample> = samples.iter().collect();

    let result = aggregate_samples(&sample_refs, &AggregationType::Avg);
    assert_eq!(result, 20.0);
}

#[test]
fn test_aggregate_max() {
    let samples = [
        create_sample(10.0, MetricType::Gauge),
        create_sample(30.0, MetricType::Gauge),
        create_sample(20.0, MetricType::Gauge),
    ];
    let sample_refs: Vec<&LabeledSample> = samples.iter().collect();

    let result = aggregate_samples(&sample_refs, &AggregationType::Max);
    assert_eq!(result, 30.0);
}

#[test]
fn test_aggregate_min() {
    let samples = [
        create_sample(10.0, MetricType::Gauge),
        create_sample(30.0, MetricType::Gauge),
        create_sample(20.0, MetricType::Gauge),
    ];
    let sample_refs: Vec<&LabeledSample> = samples.iter().collect();

    let result = aggregate_samples(&sample_refs, &AggregationType::Min);
    assert_eq!(result, 10.0);
}

#[test]
fn test_aggregate_median_odd() {
    let samples = [
        create_sample(10.0, MetricType::Gauge),
        create_sample(20.0, MetricType::Gauge),
        create_sample(30.0, MetricType::Gauge),
    ];
    let sample_refs: Vec<&LabeledSample> = samples.iter().collect();

    let result = aggregate_samples(&sample_refs, &AggregationType::Median);
    assert_eq!(result, 20.0);
}

#[test]
fn test_aggregate_median_even() {
    let samples = [
        create_sample(10.0, MetricType::Gauge),
        create_sample(20.0, MetricType::Gauge),
        create_sample(30.0, MetricType::Gauge),
        create_sample(40.0, MetricType::Gauge),
    ];
    let sample_refs: Vec<&LabeledSample> = samples.iter().collect();

    let result = aggregate_samples(&sample_refs, &AggregationType::Median);
    assert_eq!(result, 25.0);
}

#[test]
fn test_aggregate_last() {
    let samples = [
        create_sample(10.0, MetricType::Gauge),
        create_sample(20.0, MetricType::Gauge),
        create_sample(30.0, MetricType::Gauge),
    ];
    let sample_refs: Vec<&LabeledSample> = samples.iter().collect();

    let result = aggregate_samples(&sample_refs, &AggregationType::Last);
    assert_eq!(result, 30.0);
}

// --- Full two-stage aggregation tests (aggregate_metric) ---

fn make_window(samples: Vec<LabeledSample>, max_samples: usize) -> Arc<RwLock<MetricWindow>> {
    let mut window = MetricWindow::new(max_samples);
    for s in samples {
        window.push(s);
    }
    Arc::new(RwLock::new(window))
}

fn create_sample_at(value: f64, metric_type: MetricType, age: Duration) -> LabeledSample {
    LabeledSample {
        value,
        scraped_at: Instant::now() - age,
        success: true,
        metric_type,
    }
}

fn create_failed_sample(value: f64, metric_type: MetricType) -> LabeledSample {
    LabeledSample {
        value,
        scraped_at: Instant::now(),
        success: false,
        metric_type,
    }
}

// Single pod gauge window — per-pod avg aggregated, cross-pod sum (same value).
#[tokio::test]
async fn aggregate_metric_single_pod_gauge() {
    let samples = vec![
        create_sample(10.0, MetricType::Gauge),
        create_sample(20.0, MetricType::Gauge),
        create_sample(30.0, MetricType::Gauge),
    ];
    let window = make_window(samples, 10);
    let windows: Vec<(String, Arc<RwLock<MetricWindow>>)> = vec![("pod-1".to_string(), window)];

    let (result, pod_count) =
        aggregate_metric(&windows, &AggregationType::Avg, Duration::from_secs(60)).await;
    assert_eq!(result, 20.0, "single pod avg of [10,20,30] should be 20");
    assert_eq!(pod_count, 1);
}

// Three pods — cross-pod sum of per-pod averages.
#[tokio::test]
async fn aggregate_metric_multi_pod_sum() {
    let w1 = make_window(
        vec![
            create_sample(10.0, MetricType::Gauge),
            create_sample(20.0, MetricType::Gauge),
        ],
        10,
    );
    let w2 = make_window(
        vec![
            create_sample(30.0, MetricType::Gauge),
            create_sample(40.0, MetricType::Gauge),
        ],
        10,
    );
    let w3 = make_window(vec![create_sample(50.0, MetricType::Gauge)], 10);

    let windows: Vec<(String, Arc<RwLock<MetricWindow>>)> = vec![
        ("pod-1".to_string(), w1),
        ("pod-2".to_string(), w2),
        ("pod-3".to_string(), w3),
    ];

    let (result, pod_count) =
        aggregate_metric(&windows, &AggregationType::Avg, Duration::from_secs(60)).await;
    // pod-1: avg(10,20)=15, pod-2: avg(30,40)=35, pod-3: avg(50)=50
    // cross-pod sum: 15+35+50 = 100
    assert_eq!(result, 100.0);
    assert_eq!(pod_count, 3);
}

// Mix of success=true/false — only successful samples should be aggregated.
#[tokio::test]
async fn aggregate_metric_filters_failed_samples() {
    let samples = vec![
        create_sample(10.0, MetricType::Gauge),
        create_failed_sample(999.0, MetricType::Gauge),
        create_sample(30.0, MetricType::Gauge),
    ];
    let window = make_window(samples, 10);
    let windows: Vec<(String, Arc<RwLock<MetricWindow>>)> = vec![("pod-1".to_string(), window)];

    let (result, pod_count) =
        aggregate_metric(&windows, &AggregationType::Avg, Duration::from_secs(60)).await;
    // Only successful: avg(10, 30) = 20
    assert_eq!(result, 20.0);
    assert_eq!(pod_count, 1);
}

// No windows — should return 0.
#[tokio::test]
async fn aggregate_metric_empty_windows_returns_zero() {
    let windows: Vec<(String, Arc<RwLock<MetricWindow>>)> = vec![];
    let (result, pod_count) =
        aggregate_metric(&windows, &AggregationType::Avg, Duration::from_secs(60)).await;
    assert_eq!(result, 0.0);
    assert_eq!(pod_count, 0);
}

// Counter rate: two samples at different times, verify rate calculation.
#[tokio::test]
async fn aggregate_metric_counter_rate() {
    let now = Instant::now();
    let samples = vec![
        LabeledSample {
            value: 100.0,
            scraped_at: now - Duration::from_secs(10),
            success: true,
            metric_type: MetricType::Counter,
        },
        LabeledSample {
            value: 200.0,
            scraped_at: now,
            success: true,
            metric_type: MetricType::Counter,
        },
    ];
    let window = make_window(samples, 10);
    let windows: Vec<(String, Arc<RwLock<MetricWindow>>)> = vec![("pod-1".to_string(), window)];

    let (result, pod_count) =
        aggregate_metric(&windows, &AggregationType::Avg, Duration::from_secs(60)).await;
    assert_eq!(pod_count, 1);
    // rate = (200 - 100) / 10s = 10.0/s
    assert!(
        (result - 10.0).abs() < 0.1,
        "counter rate should be ~10.0/s, got {}",
        result
    );
}

// Counter reset: value decreases, rate should use last.value/time_diff.
#[tokio::test]
async fn aggregate_metric_counter_reset() {
    let now = Instant::now();
    let samples = vec![
        LabeledSample {
            value: 1000.0,
            scraped_at: now - Duration::from_secs(10),
            success: true,
            metric_type: MetricType::Counter,
        },
        LabeledSample {
            value: 50.0,
            scraped_at: now,
            success: true,
            metric_type: MetricType::Counter,
        },
    ];
    let window = make_window(samples, 10);
    let windows: Vec<(String, Arc<RwLock<MetricWindow>>)> = vec![("pod-1".to_string(), window)];

    let (result, pod_count) =
        aggregate_metric(&windows, &AggregationType::Avg, Duration::from_secs(60)).await;
    assert_eq!(pod_count, 1);
    // Counter reset: rate = last.value / time_diff = 50 / 10 = 5.0
    assert!(
        (result - 5.0).abs() < 0.1,
        "counter reset rate should be ~5.0/s, got {}",
        result
    );
}

// Single counter sample — returns capped value.
#[tokio::test]
async fn aggregate_metric_single_counter_sample() {
    let samples = vec![create_sample(42.0, MetricType::Counter)];
    let window = make_window(samples, 10);
    let windows: Vec<(String, Arc<RwLock<MetricWindow>>)> = vec![("pod-1".to_string(), window)];

    let (result, pod_count) =
        aggregate_metric(&windows, &AggregationType::Avg, Duration::from_secs(60)).await;
    assert_eq!(pod_count, 1);
    // Single sample: returns value capped at MAX_RATE
    assert_eq!(result, 42.0);
}

// Samples outside evaluation period should be excluded.
#[tokio::test]
async fn aggregate_metric_evaluation_period_filters() {
    // Old sample (120s ago) should be excluded with 60s evaluation period
    let old_sample = create_sample_at(999.0, MetricType::Gauge, Duration::from_secs(120));
    let recent_sample = create_sample_at(10.0, MetricType::Gauge, Duration::from_secs(5));

    let window = make_window(vec![old_sample, recent_sample], 10);
    let windows: Vec<(String, Arc<RwLock<MetricWindow>>)> = vec![("pod-1".to_string(), window)];

    let (result, pod_count) =
        aggregate_metric(&windows, &AggregationType::Avg, Duration::from_secs(60)).await;
    // Only the recent sample (10.0) should be included
    assert_eq!(result, 10.0);
    assert_eq!(pod_count, 1);
}

// Two pods but only one has samples in the evaluation period — contributing
// pod count should be 1, not 2 (the whole point of this change).
#[tokio::test]
async fn aggregate_metric_stale_pod_excluded_from_count() {
    let recent_sample = create_sample(10.0, MetricType::Gauge);
    let old_sample = create_sample_at(999.0, MetricType::Gauge, Duration::from_secs(120));

    let w1 = make_window(vec![recent_sample], 10);
    let w2 = make_window(vec![old_sample], 10);

    let windows: Vec<(String, Arc<RwLock<MetricWindow>>)> =
        vec![("pod-1".to_string(), w1), ("pod-2".to_string(), w2)];

    let (result, pod_count) =
        aggregate_metric(&windows, &AggregationType::Avg, Duration::from_secs(60)).await;
    assert_eq!(result, 10.0);
    assert_eq!(
        pod_count, 1,
        "only pod-1 has samples in the evaluation period"
    );
}
