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

    let (result, pod_count) = aggregate_metric(&windows, &AggregationType::Avg).await;
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

    let (result, pod_count) = aggregate_metric(&windows, &AggregationType::Avg).await;
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

    let (result, pod_count) = aggregate_metric(&windows, &AggregationType::Avg).await;
    // Only successful: avg(10, 30) = 20
    assert_eq!(result, 20.0);
    assert_eq!(pod_count, 1);
}

// No windows — should return 0.
#[tokio::test]
async fn aggregate_metric_empty_windows_returns_zero() {
    let windows: Vec<(String, Arc<RwLock<MetricWindow>>)> = vec![];
    let (result, pod_count) = aggregate_metric(&windows, &AggregationType::Avg).await;
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

    let (result, pod_count) = aggregate_metric(&windows, &AggregationType::Avg).await;
    assert_eq!(pod_count, 1);
    // rate = (200 - 100) / 10s = 10.0/s
    assert!(
        (result - 10.0).abs() < 0.1,
        "counter rate should be ~10.0/s, got {}",
        result
    );
}

// Counter reset: a drop contributes the new value as the increase for that pair.
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

    let (result, pod_count) = aggregate_metric(&windows, &AggregationType::Avg).await;
    assert_eq!(pod_count, 1);
    // Reset pair contributes 50, so rate = 50 / 10s = 5.0
    assert!(
        (result - 5.0).abs() < 0.1,
        "counter reset rate should be ~5.0/s, got {}",
        result
    );
}

fn counter_series(values: &[f64], step_secs: u64) -> Vec<LabeledSample> {
    let base = Instant::now();
    values
        .iter()
        .enumerate()
        .map(|(i, &value)| LabeledSample {
            value,
            scraped_at: base + Duration::from_secs(i as u64 * step_secs),
            success: true,
            metric_type: MetricType::Counter,
        })
        .collect()
}

fn pod_window(name: &str, samples: Vec<LabeledSample>) -> (String, Arc<RwLock<MetricWindow>>) {
    (name.to_string(), make_window(samples, 10))
}

// A single counter sample has no rate: the pod contributes nothing.
#[tokio::test]
async fn aggregate_metric_single_counter_sample() {
    let windows = vec![pod_window(
        "pod-1",
        vec![create_sample(42.0, MetricType::Counter)],
    )];

    let (result, pod_count) = aggregate_metric(&windows, &AggregationType::Avg).await;
    assert_eq!(result, 0.0);
    assert_eq!(pod_count, 0);
}

// One successful and one failed counter sample leave a single usable sample.
#[tokio::test]
async fn aggregate_metric_counter_one_success_one_failure() {
    let windows = vec![pod_window(
        "pod-1",
        vec![
            create_sample(42.0, MetricType::Counter),
            create_failed_sample(100.0, MetricType::Counter),
        ],
    )];

    let (result, pod_count) = aggregate_metric(&windows, &AggregationType::Avg).await;
    assert_eq!(result, 0.0);
    assert_eq!(pod_count, 0);
}

// A pod without a rate is excluded while another pod's rate is summed.
#[tokio::test]
async fn aggregate_metric_counter_mixed_pods() {
    let windows = vec![
        pod_window("pod-1", vec![create_sample(5000.0, MetricType::Counter)]),
        pod_window("pod-2", counter_series(&[100.0, 200.0], 10)),
    ];

    let (result, pod_count) = aggregate_metric(&windows, &AggregationType::Avg).await;
    assert_eq!(result, 10.0);
    assert_eq!(pod_count, 1);
}

// A mid-window reset counts even when the last value is above the first.
#[tokio::test]
async fn aggregate_metric_counter_mid_window_reset() {
    let windows = vec![pod_window(
        "pod-1",
        counter_series(&[100.0, 900.0, 50.0, 150.0], 10),
    )];

    let (result, pod_count) = aggregate_metric(&windows, &AggregationType::Avg).await;
    assert_eq!(result, 950.0 / 30.0);
    assert_eq!(pod_count, 1);
}

// Monotonic window: rate is (last - first) / elapsed.
#[tokio::test]
async fn aggregate_metric_counter_monotonic() {
    let windows = vec![pod_window(
        "pod-1",
        counter_series(&[100.0, 150.0, 300.0], 10),
    )];

    let (result, pod_count) = aggregate_metric(&windows, &AggregationType::Avg).await;
    assert_eq!(result, 10.0);
    assert_eq!(pod_count, 1);
}
