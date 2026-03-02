use super::parser::parse_prometheus_text;
use super::pod_cache::PodCache;
use super::telemetry::Telemetry;
use crate::apis::ctx_sh::v1beta1::ExternalPodAutoscaler;
use crate::controller::work_assigner::WorkAssigner;
use crate::store::{LabeledSample, MetricsStore, SampleKey};
use anyhow::{anyhow, Result};
use futures::stream::{self, StreamExt};
use k8s_openapi::api::core::v1::Pod;
use kube::{Api, ResourceExt};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::time;
use tracing::{debug, info, instrument, warn};

/// Maximum number of concurrent pod scrapes per EPA per scrape cycle.
const MAX_CONCURRENT_SCRAPES: usize = 64;

/// Worker that scrapes metrics from pods.
///
/// Each worker is assigned a subset of EPAs based on hash partitioning:
/// `hash(epa_key) % worker_count == worker_id`. This ensures each EPA is
/// handled by exactly one worker within the replica, eliminating redundant
/// polling across the worker pool.
pub struct Worker {
    id: usize,
    /// Total number of workers in the pool, used for hash-based EPA assignment.
    worker_count: usize,
    /// Kubernetes client retained for `get_pod_selector()` (workload API calls).
    client: kube::Client,
    /// Shared reflector-backed pod cache; pod lookups make zero API calls.
    pod_cache: Arc<PodCache>,
    metrics_store: MetricsStore,
    /// HTTP client with standard TLS verification
    http_client: reqwest::Client,
    /// HTTP client that skips TLS certificate verification (for insecureSkipVerify)
    http_client_insecure: reqwest::Client,
    work_assigner: Arc<WorkAssigner>,
    active_epas: Arc<RwLock<HashMap<String, Arc<ExternalPodAutoscaler>>>>,
    // Track next scrape time for each EPA
    next_scrape: HashMap<String, Instant>,
}

impl Worker {
    /// Creates a new scraper worker.
    ///
    /// # Arguments
    ///
    /// * `id` - Unique worker identifier (0-based) for hash-based EPA assignment
    /// * `worker_count` - Total number of workers in the pool
    /// * `client` - Kubernetes client for workload API calls (selector resolution)
    /// * `pod_cache` - Shared reflector-backed pod cache for zero-API-call pod lookups
    /// * `metrics_store` - Shared store for collected metric samples
    /// * `work_assigner` - Determines which EPAs this replica is responsible for
    /// * `active_epas` - Shared map of currently active EPA resources
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP clients cannot be constructed.
    pub fn new(
        id: usize,
        worker_count: usize,
        client: kube::Client,
        pod_cache: Arc<PodCache>,
        metrics_store: MetricsStore,
        work_assigner: Arc<WorkAssigner>,
        active_epas: Arc<RwLock<HashMap<String, Arc<ExternalPodAutoscaler>>>>,
    ) -> Result<Self> {
        // Create HTTP client with connection pooling and standard TLS verification
        let http_client = reqwest::Client::builder()
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(5))
            .build()?;

        // Create a separate client that skips TLS verification for EPAs with insecureSkipVerify
        let http_client_insecure = reqwest::Client::builder()
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(5))
            .danger_accept_invalid_certs(true)
            .build()?;

        Ok(Self {
            id,
            worker_count,
            client,
            pod_cache,
            metrics_store,
            http_client,
            http_client_insecure,
            work_assigner,
            active_epas,
            next_scrape: HashMap::new(),
        })
    }

    /// Returns `true` if this worker is responsible for the given EPA key.
    ///
    /// Uses hash-based partitioning so that each EPA is assigned to exactly
    /// one worker within the replica, preventing all workers from polling
    /// every EPA on each tick.
    fn is_assigned(&self, epa_key: &str) -> bool {
        epa_key_to_worker(epa_key, self.worker_count) == self.id
    }

    /// Runs the worker loop, polling assigned EPAs every second.
    pub async fn run(mut self) {
        info!(worker_id = self.id, "Worker started");

        // Check for EPAs to scrape every second
        let mut interval = time::interval(Duration::from_secs(1));

        loop {
            interval.tick().await;

            // Get snapshot of active EPAs
            let epas = {
                let active = self.active_epas.read().await;
                active.clone()
            };

            let now = Instant::now();

            // Check each EPA to see if it's time to scrape
            for (key, epa) in epas.iter() {
                // Hash-based partitioning: each EPA is assigned to exactly one
                // worker within this replica, avoiding redundant polling.
                if !self.is_assigned(key) {
                    continue;
                }

                // Parse namespace/name from key
                let parts: Vec<&str> = key.split('/').collect();
                if parts.len() != 2 {
                    warn!(worker_id = self.id, epa = %key, "Invalid EPA key format");
                    continue;
                }
                let namespace = parts[0];
                let name = parts[1];

                // Check if this replica should handle this EPA (inter-replica assignment)
                if !self.work_assigner.should_handle_epa(namespace, name).await {
                    continue;
                }

                // Check if it's time to scrape
                let should_scrape = match self.next_scrape.get(key) {
                    Some(next_time) => now >= *next_time,
                    None => true, // First time seeing this EPA
                };

                if should_scrape {
                    if let Err(e) = self.scrape_epa(epa).await {
                        warn!(
                            worker_id = self.id,
                            epa = %key,
                            error = %e,
                            "Failed to scrape EPA"
                        );
                    }

                    // Schedule next scrape
                    if let Ok(interval) = parse_duration(&epa.spec.scrape.interval) {
                        self.next_scrape.insert(key.clone(), now + interval);
                    }
                }
            }

            // Clean up stale entries (removed EPAs or EPAs no longer assigned
            // to this worker).
            let id = self.id;
            let wc = self.worker_count;
            self.next_scrape
                .retain(|key, _| epas.contains_key(key) && epa_key_to_worker(key, wc) == id);
        }
    }

    /// Scrape an EPA
    #[instrument(skip(self, epa), fields(
        worker_id = self.id,
        epa = %epa.metadata.name.as_deref().unwrap_or("<unnamed>"),
        namespace = %epa.metadata.namespace.as_deref().unwrap_or("<no-namespace>")
    ))]
    async fn scrape_epa(&self, epa: &Arc<ExternalPodAutoscaler>) -> Result<()> {
        let namespace = epa
            .metadata
            .namespace
            .as_deref()
            .ok_or_else(|| anyhow!("EPA missing namespace metadata"))?;
        let epa_name = epa
            .metadata
            .name
            .as_deref()
            .ok_or_else(|| anyhow!("EPA missing name metadata"))?;

        debug!(
            worker_id = self.id,
            epa = %epa_name,
            namespace = %namespace,
            "Starting scrape"
        );

        let telemetry = Telemetry::global();

        // Get target reference for pod selection
        let target_ref = epa
            .spec
            .scrape_target_ref
            .as_ref()
            .unwrap_or(&epa.spec.scale_target_ref);

        // List pods based on target
        let pods = self.list_target_pods(namespace, target_ref).await?;

        info!(
            worker_id = self.id,
            epa = %epa_name,
            namespace = %namespace,
            pod_count = pods.len(),
            "Found {} pods to scrape",
            pods.len()
        );

        // Calculate max samples per window (at least 1 to avoid empty windows)
        let interval_duration = parse_duration(&epa.spec.scrape.interval)?;
        let evaluation_period = parse_duration(&epa.spec.scrape.evaluation_period)?;
        let interval_secs = interval_duration.as_secs().max(1);
        let max_samples = (evaluation_period.as_secs().div_ceil(interval_secs) as usize).max(1);

        // Store metric configurations for aggregation (used by webhook handler)
        for metric_spec in &epa.spec.metrics {
            // Use per-metric overrides if specified, otherwise use defaults from scrape config
            let agg_type = metric_spec
                .aggregation_type
                .clone()
                .unwrap_or(epa.spec.scrape.aggregation_type.clone());

            let eval_period = if let Some(ref period_str) = metric_spec.evaluation_period {
                parse_duration(period_str)?
            } else {
                evaluation_period
            };

            let config = crate::store::MetricConfig::new(agg_type, eval_period);
            self.metrics_store.set_metric_config(
                namespace,
                epa_name,
                &metric_spec.metric_name,
                config,
            );
        }

        // Select the appropriate HTTP client based on TLS config
        let use_insecure = epa
            .spec
            .scrape
            .tls
            .as_ref()
            .map(|tls| tls.insecure_skip_verify)
            .unwrap_or(false);
        let selected_client = if use_insecure {
            &self.http_client_insecure
        } else {
            &self.http_client
        };

        // Scrape all pods concurrently with bounded parallelism.
        let results: Vec<Result<()>> = stream::iter(pods)
            .map(|pod| {
                let http_client = selected_client.clone();
                let epa_ref = epa.clone();
                let store = self.metrics_store.clone();
                async move {
                    Self::scrape_pod_static(&http_client, &epa_ref, &pod, &store, max_samples).await
                }
            })
            .buffer_unordered(MAX_CONCURRENT_SCRAPES)
            .collect()
            .await;

        // Tally results
        let mut success_count = 0u64;
        let mut failure_count = 0u64;

        for result in &results {
            match result {
                Ok(()) => success_count += 1,
                Err(_) => failure_count += 1,
            }
        }

        info!(
            worker_id = self.id,
            epa = %epa_name,
            namespace = %namespace,
            success_count = success_count,
            failure_count = failure_count,
            "Scrape completed"
        );

        // Record metrics
        telemetry
            .pods_scraped
            .with_label_values(&[epa_name, namespace])
            .inc_by(success_count);

        if failure_count > 0 {
            telemetry
                .scrape_errors
                .with_label_values(&[epa_name, namespace, "pod_scrape_failed"])
                .inc_by(failure_count);
        }

        Ok(())
    }

    /// List pods for the target reference.
    ///
    /// Resolves the workload label selector via the Kubernetes API, then reads
    /// ready pods from the in-memory [`PodCache`] reflector store — zero
    /// additional API calls for pod enumeration.
    async fn list_target_pods(
        &self,
        namespace: &str,
        target_ref: &crate::apis::ctx_sh::v1beta1::TargetRef,
    ) -> Result<Vec<Arc<Pod>>> {
        let selector = self.get_pod_selector(namespace, target_ref).await?;
        let pods = self.pod_cache.get_ready_pods(namespace, &selector).await?;
        Ok(pods)
    }

    /// Get the actual pod selector from a workload resource.
    ///
    /// Delegates to the module-level [`get_pod_selector`] function.
    async fn get_pod_selector(
        &self,
        namespace: &str,
        target_ref: &crate::apis::ctx_sh::v1beta1::TargetRef,
    ) -> Result<String> {
        get_pod_selector(&self.client, namespace, target_ref).await
    }

    /// Scrape a single pod (static method for concurrent execution)
    #[instrument(skip(http_client, epa, pod, store), fields(
        pod = %pod.name_any(),
        epa = %epa.metadata.name.as_deref().unwrap_or("<unnamed>")
    ))]
    pub(crate) async fn scrape_pod_static(
        http_client: &reqwest::Client,
        epa: &ExternalPodAutoscaler,
        pod: &Pod,
        store: &MetricsStore,
        max_samples: usize,
    ) -> Result<()> {
        let start = Instant::now();
        let pod_name = pod.name_any();
        let namespace = pod
            .namespace()
            .ok_or_else(|| anyhow!("Pod {} has no namespace", pod_name))?;
        let epa_name = epa
            .metadata
            .name
            .as_deref()
            .ok_or_else(|| anyhow!("EPA missing name metadata"))?;

        let pod_ip = pod
            .status
            .as_ref()
            .and_then(|s| s.pod_ip.as_ref())
            .ok_or_else(|| anyhow!("Pod {} has no IP", pod_name))?;

        // Build URL
        let port = epa.spec.scrape.port;
        let path = &epa.spec.scrape.path;
        let scheme = &epa.spec.scrape.scheme;
        let url = format!("{}://{}:{}{}", scheme, pod_ip, port, path);

        debug!(pod = %pod_name, url = %url, "Scraping pod");

        // Parse timeout
        let timeout_duration = parse_duration(&epa.spec.scrape.timeout)?;

        // Fetch metrics (with timeout)
        let response =
            match tokio::time::timeout(timeout_duration, http_client.get(&url).send()).await {
                Ok(Ok(resp)) => resp,
                Ok(Err(e)) => {
                    warn!(pod = %pod_name, error = %e, "HTTP error while scraping");
                    return Err(anyhow!("HTTP error: {}", e));
                }
                Err(_) => {
                    warn!(pod = %pod_name, "Scrape timeout");
                    return Err(anyhow!("Request timeout"));
                }
            };

        let status = response.status();
        if !status.is_success() {
            warn!(pod = %pod_name, status = %status, "Non-success HTTP status while scraping");
            return Err(anyhow!("HTTP {} from pod {}", status, pod_name));
        }

        let text = response
            .text()
            .await
            .map_err(|e| anyhow!("Failed to read response: {}", e))?;

        // Parse Prometheus metrics
        let parsed_metrics = parse_prometheus_text(&text)?;

        debug!(
            pod = %pod_name,
            metric_count = parsed_metrics.len(),
            "Parsed metrics from pod"
        );

        let scraped_at = Instant::now();

        // Store each configured metric in the sliding window
        for metric_spec in &epa.spec.metrics {
            // Find matching Prometheus metric(s)
            let matching_metrics: Vec<_> = parsed_metrics
                .iter()
                .filter(|m| m.name == metric_spec.metric_name)
                .collect();

            if matching_metrics.is_empty() {
                warn!(
                    pod = %pod_name,
                    metric = %metric_spec.metric_name,
                    "Metric not found in scrape response"
                );
                continue;
            }

            // For each matching metric (could have different labels), store a sample
            for prom_metric in matching_metrics {
                // Filter by label selector if specified
                if let Some(ref label_selector) = metric_spec.label_selector {
                    if !matches_label_selector(&prom_metric.labels, label_selector) {
                        continue;
                    }
                }

                let sample = LabeledSample {
                    value: prom_metric.value,
                    scraped_at,
                    success: true,
                    metric_type: prom_metric.metric_type,
                };

                let key = SampleKey::new(
                    namespace.clone(),
                    epa_name.to_string(),
                    metric_spec.metric_name.clone(),
                    pod_name.clone(),
                );

                store.push_sample(key, sample, max_samples).await;

                debug!(
                    pod = %pod_name,
                    metric = %metric_spec.metric_name,
                    value = prom_metric.value,
                    "Stored sample in sliding window"
                );
            }
        }

        // Record scrape duration
        let telemetry = Telemetry::global();
        telemetry
            .scrape_duration
            .with_label_values(&[epa_name, &namespace])
            .observe(start.elapsed().as_secs_f64());

        Ok(())
    }
}

/// Resolve a workload resource to its pod label selector string.
///
/// Supports `Deployment`, `StatefulSet`, and `DaemonSet`. Returns an error for
/// any other kind or when the named resource cannot be found.
pub(crate) async fn get_pod_selector(
    client: &kube::Client,
    namespace: &str,
    target_ref: &crate::apis::ctx_sh::v1beta1::TargetRef,
) -> Result<String> {
    use k8s_openapi::api::apps::v1::{DaemonSet, Deployment, StatefulSet};

    match target_ref.kind.as_str() {
        "Deployment" => {
            let api: Api<Deployment> = Api::namespaced(client.clone(), namespace);
            let deployment = api.get(&target_ref.name).await?;
            let selector = deployment
                .spec
                .ok_or_else(|| anyhow!("Deployment has no spec"))?
                .selector;
            build_label_selector_string(&selector)
        }
        "StatefulSet" => {
            let api: Api<StatefulSet> = Api::namespaced(client.clone(), namespace);
            let sts = api.get(&target_ref.name).await?;
            let selector = sts
                .spec
                .ok_or_else(|| anyhow!("StatefulSet has no spec"))?
                .selector;
            build_label_selector_string(&selector)
        }
        "DaemonSet" => {
            let api: Api<DaemonSet> = Api::namespaced(client.clone(), namespace);
            let ds = api.get(&target_ref.name).await?;
            let selector = ds
                .spec
                .ok_or_else(|| anyhow!("DaemonSet has no spec"))?
                .selector;
            build_label_selector_string(&selector)
        }
        kind => Err(anyhow!("Unsupported target kind: {}", kind)),
    }
}

/// Build a label selector string from a LabelSelector.
///
/// Handles both `matchLabels` (equality) and `matchExpressions` (`In`
/// operator converted to equality pairs). Returns an error only when the
/// selector yields no usable label constraints.
pub(crate) fn build_label_selector_string(
    selector: &k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector,
) -> Result<String> {
    let mut parts = Vec::new();

    // matchLabels → key=value pairs
    if let Some(ref labels) = selector.match_labels {
        for (k, v) in labels {
            parts.push(format!("{}={}", k, v));
        }
    }

    // matchExpressions → convert supported operators to equality format.
    // The pod cache's label matching only supports equality, so we convert
    // `In` with exactly one value to `key=value`. All other operators and
    // multi-value `In` are rejected to avoid silently widening the selector.
    if let Some(ref expressions) = selector.match_expressions {
        for expr in expressions {
            match expr.operator.as_str() {
                "In" => match expr.values.as_ref() {
                    Some(values) if values.len() == 1 => {
                        parts.push(format!("{}={}", expr.key, values[0]));
                    }
                    _ => {
                        return Err(anyhow!(
                            "matchExpressions 'In' on key '{}' requires exactly one value",
                            expr.key
                        ));
                    }
                },
                op => {
                    return Err(anyhow!(
                        "matchExpressions operator '{}' on key '{}' is not supported",
                        op,
                        expr.key
                    ));
                }
            }
        }
    }

    if parts.is_empty() {
        return Err(anyhow!("Selector has no usable label constraints"));
    }

    Ok(parts.join(","))
}

/// Maps an EPA key to a worker index in `[0, worker_count)`.
///
/// Uses `DefaultHasher` (SipHash) which is deterministic within a single
/// process but **not** guaranteed stable across Rust versions or processes.
/// This is acceptable because all workers run in the same process; the
/// assignment only needs to be consistent for the lifetime of the process.
fn epa_key_to_worker(epa_key: &str, worker_count: usize) -> usize {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    epa_key.hash(&mut hasher);
    (hasher.finish() % worker_count as u64) as usize
}

/// Parse duration string using humantime (supports "15s", "5m", "1h", etc.)
fn parse_duration(s: &str) -> Result<Duration> {
    humantime::parse_duration(s).map_err(|e| anyhow!("Invalid duration '{}': {}", s, e))
}

/// Check if metric labels match a label selector
fn matches_label_selector(
    labels: &std::collections::BTreeMap<String, String>,
    selector: &crate::apis::ctx_sh::v1beta1::LabelSelector,
) -> bool {
    // Check matchLabels
    if let Some(ref match_labels) = selector.match_labels {
        for (key, value) in match_labels {
            if labels.get(key) != Some(value) {
                return false;
            }
        }
    }

    // Check matchExpressions
    if let Some(ref match_expressions) = selector.match_expressions {
        for expr in match_expressions {
            let label_value = labels.get(&expr.key);

            use crate::apis::ctx_sh::v1beta1::LabelSelectorOperator;
            match expr.operator {
                LabelSelectorOperator::In => {
                    if let Some(ref values) = expr.values {
                        if let Some(v) = label_value {
                            if !values.contains(v) {
                                return false;
                            }
                        } else {
                            return false;
                        }
                    }
                }
                LabelSelectorOperator::NotIn => {
                    if let Some(ref values) = expr.values {
                        if let Some(v) = label_value {
                            if values.contains(v) {
                                return false;
                            }
                        }
                    }
                }
                LabelSelectorOperator::Exists => {
                    if label_value.is_none() {
                        return false;
                    }
                }
                LabelSelectorOperator::DoesNotExist => {
                    if label_value.is_some() {
                        return false;
                    }
                }
            }
        }
    }

    true
}
