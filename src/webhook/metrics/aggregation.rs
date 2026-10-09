use crate::apis::ctx_sh::v1beta1::AggregationType;
use crate::store::{LabeledSample, MetricType, MetricWindow};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::debug;

/// Aggregate metric across all pods using two-stage aggregation
/// Stage 1: Per-pod window aggregation
/// Stage 2: Cross-pod sum
///
/// Returns the sum and the number of pods that contributed a value. A counter
/// pod with fewer than two successful samples has no rate and is left out of
/// both the sum and the count.
pub async fn aggregate_metric(
    windows: &[(String, Arc<RwLock<MetricWindow>>)],
    aggregation_type: &AggregationType,
) -> (f64, usize) {
    let mut per_pod_values = Vec::new();

    for (pod_name, window_arc) in windows {
        // Lock window for reading
        let window = window_arc.read().await;

        if window.samples.is_empty() {
            debug!(pod = %pod_name, "No samples in window");
            continue;
        }

        // Filter successful samples only
        let successful_samples: Vec<_> = window.samples.iter().filter(|s| s.success).collect();

        if successful_samples.is_empty() {
            debug!(pod = %pod_name, "No successful samples in window");
            continue;
        }

        // Determine if this is a counter or gauge
        let metric_type = successful_samples[0].metric_type;

        let pod_value = match metric_type {
            MetricType::Counter => match calculate_rate(&successful_samples) {
                Some(rate) => rate,
                None => {
                    debug!(
                        pod = %pod_name,
                        sample_count = successful_samples.len(),
                        "Too few counter samples to compute a rate"
                    );
                    continue;
                }
            },
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

/// Per-second rate of a counter over the window, or `None` when fewer than two
/// samples exist.
///
/// The increase is summed pair by pair; a pair whose value drops is a counter
/// restart and contributes the new value. Zero elapsed time yields 0. The rate
/// is capped at 1 billion per second.
fn calculate_rate(samples: &[&LabeledSample]) -> Option<f64> {
    const MAX_RATE: f64 = 1_000_000_000.0;

    let [first, .., last] = samples else {
        return None;
    };

    let elapsed = last
        .scraped_at
        .duration_since(first.scraped_at)
        .as_secs_f64();

    if elapsed == 0.0 {
        return Some(0.0);
    }

    let increase: f64 = samples
        .windows(2)
        .map(|pair| {
            if pair[1].value >= pair[0].value {
                pair[1].value - pair[0].value
            } else {
                pair[1].value
            }
        })
        .sum();

    Some((increase / elapsed).min(MAX_RATE))
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
