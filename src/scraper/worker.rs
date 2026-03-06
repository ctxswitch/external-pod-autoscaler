use super::parser::parse_prometheus_text;
use super::telemetry::Telemetry;
use crate::apis::ctx_sh::v1beta1::ExternalPodAutoscaler;
use crate::store::{LabeledSample, MetricsStore, SampleKey};
use anyhow::{anyhow, Result};
use k8s_openapi::api::core::v1::Pod;
use kube::{Api, ResourceExt};
use std::time::{Duration, Instant};
use tracing::{debug, instrument, warn};

/// Worker that scrapes metrics from pods.
///
/// Each worker consumes [`super::scheduler::ScrapeJob`] items from a shared
/// MPMC async channel. The scheduler produces jobs on a per-pod basis and
/// workers pull them off the channel, performing the actual HTTP scrape and
/// storing the resulting metric samples.
pub struct Worker {
    /// Receives scrape jobs dispatched by the scheduler.
    job_rx: async_channel::Receiver<super::scheduler::ScrapeJob>,
    metrics_store: MetricsStore,
    /// HTTP client with standard TLS verification.
    http_client: reqwest::Client,
    /// HTTP client that skips TLS certificate verification (for insecureSkipVerify).
    http_client_insecure: reqwest::Client,
}

impl Worker {
    /// Creates a new scraper worker.
    ///
    /// # Arguments
    ///
    /// * `job_rx` - Receiver end of the shared MPMC job channel
    /// * `metrics_store` - Shared store for collected metric samples
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP clients cannot be constructed.
    pub fn new(
        job_rx: async_channel::Receiver<super::scheduler::ScrapeJob>,
        metrics_store: MetricsStore,
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
            job_rx,
            metrics_store,
            http_client,
            http_client_insecure,
        })
    }

    /// Runs the worker loop, consuming scrape jobs from the channel.
    ///
    /// Blocks until the channel is closed (all senders dropped), processing
    /// one job at a time. Each job triggers an HTTP scrape of a single pod.
    pub async fn run(self) {
        debug!("Worker started");

        while let Ok(job) = self.job_rx.recv().await {
            let epa_name = job.epa.metadata.name.as_deref().unwrap_or("<unnamed>");
            let namespace = job
                .epa
                .metadata
                .namespace
                .as_deref()
                .unwrap_or("<no-namespace>");

            let http_client = if job.use_insecure_tls {
                &self.http_client_insecure
            } else {
                &self.http_client
            };

            let telemetry = Telemetry::global();
            let scrape_stats = self.metrics_store.get_scrape_stats(namespace, epa_name);

            match Self::scrape_pod_static(
                http_client,
                &job.epa,
                &job.pod,
                &self.metrics_store,
                job.max_samples,
            )
            .await
            {
                Ok(()) => {
                    telemetry
                        .pods_scraped
                        .with_label_values(&[epa_name, namespace])
                        .inc();
                    scrape_stats.record_success();
                }
                Err(e) => {
                    warn!(epa = %epa_name, namespace = %namespace, error = %e, "Pod scrape failed");
                    telemetry
                        .scrape_errors
                        .with_label_values(&[epa_name, namespace, "pod_scrape_failed"])
                        .inc();
                    scrape_stats.record_error();
                }
            }
        }
    }

    /// Scrape a single pod (static method for concurrent execution).
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

    // matchLabels -> key=value pairs
    if let Some(ref labels) = selector.match_labels {
        for (k, v) in labels {
            parts.push(format!("{}={}", k, v));
        }
    }

    // matchExpressions -> convert supported operators to equality format.
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

/// Parse duration string using humantime (supports "15s", "5m", "1h", etc.)
pub(crate) fn parse_duration(s: &str) -> Result<Duration> {
    humantime::parse_duration(s).map_err(|e| anyhow!("Invalid duration '{}': {}", s, e))
}

/// Check if metric labels match a label selector.
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
