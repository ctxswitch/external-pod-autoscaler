mod ops;
mod types;
mod window;

#[cfg(test)]
mod ops_test;
#[cfg(test)]
mod window_test;

pub use types::{
    CacheKey, CachedAggregation, LabeledSample, MetricConfig, MetricType, SampleKey, ScrapeStats,
};
pub use window::MetricWindow;

use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Metrics store with sliding windows and aggregation cache.
///
/// Thread-safe storage for time-windowed metric samples. Supports:
/// - Per-pod sliding windows of samples
/// - Cached aggregation results with TTL
/// - Metric configuration (aggregation type)
/// - Cleanup on EPA deletion
///
/// # Storage Strategy
///
/// - **Windows**: One sliding window per (namespace, EPA, metric, pod) combination
///   - Each window wrapped in Arc<RwLock<>> for cheap cloning during aggregation
///   - Uses DashMap for lock-free sharded access (eliminates global lock contention)
/// - **Cache**: One cached aggregation per (namespace, EPA, metric) combination
///   - Uses DashMap for concurrent read/write access
/// - **Configs**: One metric config per (namespace, EPA, metric) combination
///   - Uses DashMap for concurrent access
pub struct MetricsStore {
    windows: Arc<DashMap<SampleKey, Arc<RwLock<MetricWindow>>>>,
    cache: Arc<DashMap<CacheKey, CachedAggregation>>,
    configs: Arc<DashMap<CacheKey, MetricConfig>>,
    scrape_stats: Arc<DashMap<String, Arc<ScrapeStats>>>,
}
