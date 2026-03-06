use super::types::{CacheKey, CachedAggregation, LabeledSample, SampleKey};
use super::window::MetricWindow;
use super::MetricsStore;
use dashmap::DashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

impl MetricsStore {
    /// Creates a new empty metrics store.
    pub fn new() -> Self {
        Self {
            windows: Arc::new(DashMap::new()),
            cache: Arc::new(DashMap::new()),
            configs: Arc::new(DashMap::new()),
        }
    }

    /// Pushes a new sample into a sliding window.
    ///
    /// Creates the window if it doesn't exist. Old samples are evicted when the window
    /// reaches `max_samples` capacity.
    ///
    /// Uses DashMap for shard-level locking - no global lock contention.
    ///
    /// # Arguments
    ///
    /// * `key` - Identifies the window (namespace, EPA, metric, pod)
    /// * `sample` - The sample to store
    /// * `max_samples` - Maximum window size (used when creating new windows)
    pub async fn push_sample(&self, key: SampleKey, sample: LabeledSample, max_samples: usize) {
        // DashMap's entry API uses per-shard locking (much better than global lock)
        let window_arc = self
            .windows
            .entry(key)
            .or_insert_with(|| Arc::new(RwLock::new(MetricWindow::new(max_samples))))
            // Clone the Arc to release the DashMap shard guard before acquiring
            // the RwLock write lock. Without this, we'd hold both locks simultaneously,
            // risking deadlock with concurrent readers that lock in the opposite order.
            .clone();

        // Only lock the individual window for writing
        let mut window = window_arc.write().await;
        window.push(sample);
    }

    /// Returns all windows for a specific EPA and metric across all pods.
    ///
    /// Used for cross-pod aggregation. Each returned tuple contains the pod name
    /// and an Arc to its sliding window of samples. Cloning the Arc is cheap and
    /// avoids copying the window data.
    ///
    /// DashMap allows concurrent iteration with shard-level locking.
    ///
    /// # Arguments
    ///
    /// * `namespace` - Kubernetes namespace
    /// * `epa_name` - ExternalPodAutoscaler name
    /// * `metric_name` - Metric name to retrieve
    pub fn get_windows(
        &self,
        namespace: &str,
        epa_name: &str,
        metric_name: &str,
    ) -> Vec<(String, Arc<RwLock<MetricWindow>>)> {
        // DashMap iter() uses shard-level locking during iteration
        self.windows
            .iter()
            .filter(|entry| {
                let key = entry.key();
                key.namespace == namespace
                    && key.epa_name == epa_name
                    && key.metric_name == metric_name
            })
            .map(|entry| (entry.key().pod_name.clone(), Arc::clone(entry.value())))
            .collect()
    }

    /// Returns cached aggregation value if still valid.
    ///
    /// Checks the cache for a pre-computed aggregation and returns it if the TTL
    /// hasn't expired. Returns `None` if not cached or expired.
    ///
    /// DashMap provides shard-level locking for reads.
    ///
    /// # Arguments
    ///
    /// * `namespace` - Kubernetes namespace
    /// * `epa_name` - ExternalPodAutoscaler name
    /// * `metric_name` - Metric name
    pub fn get_cached(&self, namespace: &str, epa_name: &str, metric_name: &str) -> Option<f64> {
        let key = CacheKey::new(
            namespace.to_string(),
            epa_name.to_string(),
            metric_name.to_string(),
        );
        // DashMap get() returns a Ref guard that dereferences to the value
        self.cache
            .get(&key)
            .filter(|c| c.value().is_valid())
            .map(|c| c.value().value)
    }

    /// Stores an aggregated result in the cache with TTL.
    ///
    /// The cached value will be valid until the TTL expires. Overwrites any
    /// existing cached value for the same metric.
    ///
    /// DashMap provides shard-level locking for writes.
    ///
    /// # Arguments
    ///
    /// * `namespace` - Kubernetes namespace
    /// * `epa_name` - ExternalPodAutoscaler name
    /// * `metric_name` - Metric name
    /// * `value` - Aggregated value to cache
    /// * `ttl` - How long the cached value remains valid
    pub fn cache_result(
        &self,
        namespace: &str,
        epa_name: &str,
        metric_name: &str,
        value: f64,
        ttl: Duration,
    ) {
        let key = CacheKey::new(
            namespace.to_string(),
            epa_name.to_string(),
            metric_name.to_string(),
        );
        // DashMap insert() uses shard-level locking
        self.cache.insert(key, CachedAggregation::new(value, ttl));
    }

    /// Stores metric configuration for aggregation.
    ///
    /// DashMap provides shard-level locking for writes.
    ///
    /// # Arguments
    ///
    /// * `namespace` - Kubernetes namespace
    /// * `epa_name` - ExternalPodAutoscaler name
    /// * `metric_name` - Metric name
    /// * `config` - Metric configuration (aggregation type and evaluation period)
    pub fn set_metric_config(
        &self,
        namespace: &str,
        epa_name: &str,
        metric_name: &str,
        config: super::types::MetricConfig,
    ) {
        let key = CacheKey::new(
            namespace.to_string(),
            epa_name.to_string(),
            metric_name.to_string(),
        );
        self.configs.insert(key, config);
    }

    /// Retrieves metric configuration for aggregation.
    ///
    /// DashMap provides shard-level locking for reads.
    ///
    /// # Arguments
    ///
    /// * `namespace` - Kubernetes namespace
    /// * `epa_name` - ExternalPodAutoscaler name
    /// * `metric_name` - Metric name
    ///
    /// # Returns
    ///
    /// Returns the metric configuration if found, otherwise returns default config.
    pub fn get_metric_config(
        &self,
        namespace: &str,
        epa_name: &str,
        metric_name: &str,
    ) -> super::types::MetricConfig {
        let key = CacheKey::new(
            namespace.to_string(),
            epa_name.to_string(),
            metric_name.to_string(),
        );
        self.configs
            .get(&key)
            .map(|entry| entry.value().clone())
            .unwrap_or_default()
    }

    /// Evicts windows whose newest sample is older than `max_age`.
    ///
    /// Snapshots all window keys and Arc handles first (releasing DashMap shard
    /// guards), then checks staleness under individual RwLock reads, and finally
    /// removes stale entries in a separate pass.
    ///
    /// Empty windows (newly created, no samples yet) are retained to avoid
    /// racing with concurrent `push_sample` calls.
    pub async fn evict_stale_windows(&self, max_age: Duration) -> usize {
        // Snapshot keys and Arc handles, releasing DashMap shard guards before
        // awaiting any RwLock reads.
        let entries: Vec<(SampleKey, Arc<RwLock<MetricWindow>>)> = self
            .windows
            .iter()
            .map(|entry| (entry.key().clone(), Arc::clone(entry.value())))
            .collect();

        let mut stale_keys = Vec::new();
        for (key, window_arc) in entries {
            let window = window_arc.read().await;
            // Only evict windows that have samples AND those samples are stale.
            // Empty windows are retained (may be newly created).
            let is_stale = window.newest_sample_age().is_some_and(|age| age > max_age);
            if is_stale {
                stale_keys.push(key);
            }
        }

        let count = stale_keys.len();
        for key in stale_keys {
            self.windows.remove(&key);
        }

        count
    }

    /// Removes all windows, cache entries, and configs for a specific EPA.
    ///
    /// Called when an EPA is deleted to clean up all associated metric data.
    ///
    /// DashMap retain() uses shard-level locking during iteration.
    ///
    /// # Arguments
    ///
    /// * `namespace` - Kubernetes namespace
    /// * `epa_name` - ExternalPodAutoscaler name
    pub fn remove_epa_windows(&self, namespace: &str, epa_name: &str) {
        // DashMap retain() uses shard-level locking during iteration
        self.windows
            .retain(|key, _| !(key.namespace == namespace && key.epa_name == epa_name));

        self.cache
            .retain(|key, _| !(key.namespace == namespace && key.epa_name == epa_name));

        self.configs
            .retain(|key, _| !(key.namespace == namespace && key.epa_name == epa_name));
    }

    /// Cleans up expired cache entries.
    ///
    /// Should be called periodically to prevent unbounded memory growth from stale cache entries.
    /// This method removes all cached aggregations that have exceeded their TTL.
    ///
    /// # Returns
    ///
    /// Returns the number of expired entries that were removed.
    pub fn cleanup_expired_cache(&self) -> usize {
        let initial_count = self.cache.len();

        // DashMap retain() efficiently removes expired entries
        self.cache.retain(|_, value| value.is_valid());

        let final_count = self.cache.len();
        initial_count - final_count
    }

    /// Starts a background task that periodically cleans up expired cache entries.
    ///
    /// This task runs every 60 seconds and removes all expired cached aggregations.
    /// Returns a JoinHandle that can be used to cancel the task.
    ///
    /// # Arguments
    ///
    /// * `interval` - How often to run cleanup (defaults to 60s if not specified)
    pub fn spawn_cache_cleanup_task(self, interval: Duration) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                ticker.tick().await;

                let removed = self.cleanup_expired_cache();
                if removed > 0 {
                    tracing::debug!(removed_count = removed, "Cleaned up expired cache entries");
                }
            }
        })
    }
}

impl Clone for MetricsStore {
    fn clone(&self) -> Self {
        Self {
            windows: self.windows.clone(),
            cache: self.cache.clone(),
            configs: self.configs.clone(),
        }
    }
}

impl Default for MetricsStore {
    fn default() -> Self {
        Self::new()
    }
}
