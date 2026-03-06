use crate::apis::ctx_sh::v1beta1::AggregationType;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

/// Type of Prometheus metric (auto-detected from scrape).
///
/// Determines how the metric is aggregated:
/// - `Gauge`: Aggregated using the configured aggregation type (avg, max, min, median, last)
/// - `Counter`: Rate-of-change is calculated instead of direct aggregation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricType {
    /// Gauge metric (point-in-time value)
    Gauge,
    /// Counter metric (monotonically increasing value)
    Counter,
}

/// A single labeled sample from a metrics scrape.
///
/// Contains the metric value, Prometheus labels, scrape timestamp, and success status.
/// Used for time-windowed aggregation.
#[derive(Debug, Clone)]
pub struct LabeledSample {
    /// Metric value
    pub value: f64,
    /// When this sample was scraped
    pub scraped_at: Instant,
    /// Whether the scrape was successful
    pub success: bool,
    /// Type of metric (gauge or counter)
    pub metric_type: MetricType,
}

/// Key for identifying a metric window.
///
/// Uniquely identifies a metric window by EPA, metric name, and pod.
/// Each pod/metric combination has its own sliding window.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SampleKey {
    /// Kubernetes namespace
    pub namespace: String,
    /// ExternalPodAutoscaler name
    pub epa_name: String,
    /// Metric name being tracked
    pub metric_name: String,
    /// Pod name being scraped
    pub pod_name: String,
}

impl SampleKey {
    /// Creates a new sample key.
    pub fn new(namespace: String, epa_name: String, metric_name: String, pod_name: String) -> Self {
        Self {
            namespace,
            epa_name,
            metric_name,
            pod_name,
        }
    }
}

/// Key for cached aggregation results.
///
/// Identifies aggregated metric values by EPA and metric name (across all pods).
/// Used for caching aggregation results with TTL.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    /// Kubernetes namespace
    pub namespace: String,
    /// ExternalPodAutoscaler name
    pub epa_name: String,
    /// Metric name
    pub metric_name: String,
}

impl CacheKey {
    /// Creates a new cache key.
    pub fn new(namespace: String, epa_name: String, metric_name: String) -> Self {
        Self {
            namespace,
            epa_name,
            metric_name,
        }
    }
}

/// Cached aggregation result with TTL.
///
/// Stores a pre-computed aggregation value with a timestamp and TTL.
/// Used to avoid re-aggregating metrics on every HPA query.
#[derive(Debug, Clone)]
pub struct CachedAggregation {
    /// Aggregated metric value
    pub value: f64,
    /// When the aggregation was computed
    pub computed_at: Instant,
    /// Time-to-live for this cached value
    pub ttl: Duration,
}

impl CachedAggregation {
    /// Creates a new cached aggregation with the current timestamp.
    pub fn new(value: f64, ttl: Duration) -> Self {
        Self {
            value,
            computed_at: Instant::now(),
            ttl,
        }
    }

    /// Returns true if this cached value is still valid (within TTL).
    pub fn is_valid(&self) -> bool {
        self.computed_at.elapsed() < self.ttl
    }
}

/// Configuration for metric aggregation.
///
/// Stores aggregation settings from the EPA spec (aggregation type).
/// Can be set per-metric or use defaults from the EPA scrape config.
#[derive(Debug, Clone)]
pub struct MetricConfig {
    /// Aggregation type (avg, max, min, median, last)
    pub aggregation_type: AggregationType,
}

impl MetricConfig {
    /// Creates a new metric configuration.
    pub fn new(aggregation_type: AggregationType) -> Self {
        Self { aggregation_type }
    }
}

impl Default for MetricConfig {
    fn default() -> Self {
        Self {
            aggregation_type: AggregationType::Avg,
        }
    }
}

/// Per-EPA scrape statistics, updated by the worker pool.
#[derive(Debug)]
pub struct ScrapeStats {
    scraped: AtomicI32,
    errors: AtomicI32,
}

impl ScrapeStats {
    pub fn new() -> Self {
        Self {
            scraped: AtomicI32::new(0),
            errors: AtomicI32::new(0),
        }
    }

    pub fn record_success(&self) {
        self.scraped.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot_and_reset(&self) -> (i32, i32) {
        (
            self.scraped.swap(0, Ordering::Relaxed),
            self.errors.swap(0, Ordering::Relaxed),
        )
    }

    pub fn restore(&self, scraped: i32, errors: i32) {
        self.scraped.fetch_add(scraped, Ordering::Relaxed);
        self.errors.fetch_add(errors, Ordering::Relaxed);
    }
}

impl Default for ScrapeStats {
    fn default() -> Self {
        Self::new()
    }
}
