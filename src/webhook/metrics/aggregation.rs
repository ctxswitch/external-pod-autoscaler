use crate::apis::ctx_sh::v1beta1::AggregationType;
use crate::store::{LabeledSample, MetricType, MetricWindow};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::debug;

/// Aggregate metric across all pods using two-stage aggregation
/// Stage 1: Per-pod window aggregation
/// Stage 2: Cross-pod sum
pub async fn aggregate_metric(
    windows: &[(String, Arc<RwLock<MetricWindow>>)],
    aggregation_type: &AggregationType,
    evaluation_period: Duration,
) -> (f64, usize) {
    let mut per_pod_values = Vec::new();

    for (pod_name, window_arc) in windows {
        // Lock window for reading
        let window = window_arc.read().await;

        // Get samples within evaluation period
        let samples = window.get_samples_in_period(evaluation_period);

        if samples.is_empty() {
            debug!(pod = %pod_name, "No samples in evaluation period");
            continue;
        }

        // Filter successful samples only
        let successful_samples: Vec<_> = samples.iter().filter(|s| s.success).copied().collect();

        if successful_samples.is_empty() {
            debug!(pod = %pod_name, "No successful samples in evaluation period");
            continue;
        }

        // Determine if this is a counter or gauge
        let metric_type = successful_samples[0].metric_type;

        let pod_value = match metric_type {
            MetricType::Counter => {
                // For counters, calculate rate instead of aggregation
                calculate_rate(&successful_samples)
            }
            MetricType::Gauge => {
                // For gauges, apply aggregation type
                aggregate_samples(&successful_samples, aggregation_type)
            }
        };

        debug!(
            pod = %pod_name,
            value = pod_value,
            metric_type = ?metric_type,
            sample_count = successful_samples.len(),
            "Computed per-pod value"
        );

        per_pod_values.push(pod_value);
    }

    // Stage 2: Cross-pod sum
    let total: f64 = per_pod_values.iter().sum();

    debug!(
        pod_count = per_pod_values.len(),
        total = total,
        "Computed cross-pod sum"
    );

    (total, per_pod_values.len())
}

/// Calculate rate for counter metrics
///
/// Applies rate limiting to prevent unrealistic values during counter resets.
/// Maximum rate is capped at 1 billion per second to prevent overflow/DoS.
fn calculate_rate(samples: &[&LabeledSample]) -> f64 {
    // Maximum allowed rate (1 billion/sec) prevents unrealistic values and overflow
    const MAX_RATE: f64 = 1_000_000_000.0;

    if samples.is_empty() {
        return 0.0;
    }

    if samples.len() < 2 {
        // Not enough samples to calculate rate, return current value capped
        return samples[0].value.min(MAX_RATE);
    }

    let first = &samples[0];
    let last = &samples[samples.len() - 1];

    let time_diff = last
        .scraped_at
        .duration_since(first.scraped_at)
        .as_secs_f64();

    if time_diff == 0.0 {
        return 0.0;
    }

    let delta = last.value - first.value;

    let raw_rate = if delta < 0.0 {
        // Counter was reset, use current value / time
        last.value / time_diff
    } else {
        // Normal counter increase
        delta / time_diff
    };

    // Cap rate at maximum to prevent unrealistic values
    raw_rate.min(MAX_RATE)
}

/// Aggregate samples using specified aggregation type.
pub(crate) fn aggregate_samples(
    samples: &[&LabeledSample],
    aggregation_type: &AggregationType,
) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }

    match aggregation_type {
        AggregationType::Avg => {
            let sum: f64 = samples.iter().map(|s| s.value).sum();
            sum / samples.len() as f64
        }
        AggregationType::Max => samples
            .iter()
            .map(|s| s.value)
            .fold(f64::NEG_INFINITY, f64::max),
        AggregationType::Min => samples
            .iter()
            .map(|s| s.value)
            .fold(f64::INFINITY, f64::min),
        AggregationType::Median => {
            let mut values: Vec<f64> = samples.iter().map(|s| s.value).collect();
            values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let mid = values.len() / 2;
            #[allow(clippy::manual_is_multiple_of)]
            if values.len() % 2 == 0 {
                (values[mid - 1] + values[mid]) / 2.0
            } else {
                values[mid]
            }
        }
        AggregationType::Last => samples.last().map(|s| s.value).unwrap_or(0.0),
    }
}
