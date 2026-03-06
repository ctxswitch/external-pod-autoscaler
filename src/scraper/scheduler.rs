use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::anyhow;
use k8s_openapi::api::core::v1::Pod;
use tokio::sync::RwLock;
use tokio::time;
use tracing::{debug, info, warn};

use super::pod_cache::PodCache;
use super::telemetry::Telemetry;
use super::worker::{get_pod_selector, parse_duration};
use crate::apis::ctx_sh::v1beta1::ExternalPodAutoscaler;
use crate::membership::ownership::EpaOwnership;
use crate::store::{MetricConfig, MetricsStore};

/// A single unit of scrape work targeting one pod for one EPA.
pub struct ScrapeJob {
    /// The EPA resource that defines the scrape configuration.
    pub epa: Arc<ExternalPodAutoscaler>,
    /// The pod to scrape metrics from.
    pub pod: Arc<Pod>,
    /// Maximum number of metric samples to retain.
    pub max_samples: usize,
    /// Whether to skip TLS certificate verification.
    pub use_insecure_tls: bool,
}

/// Produces `ScrapeJob` items on a per-pod basis and sends them to workers
/// through an MPMC async channel.
///
/// `async_channel` is used instead of `tokio::sync::mpsc` because multiple
/// worker tasks consume from the same channel concurrently; `mpsc` only
/// supports a single consumer.
pub struct Scheduler {
    /// Shared EPA map; the update task writes, the scheduler only reads.
    active_epas: Arc<RwLock<HashMap<String, Arc<ExternalPodAutoscaler>>>>,
    pod_cache: Arc<PodCache>,
    client: kube::Client,
    epa_ownership: Arc<EpaOwnership>,
    metrics_store: MetricsStore,
    job_tx: async_channel::Sender<ScrapeJob>,
    /// Tracks the next scheduled scrape time keyed by EPA name.
    next_scrape: HashMap<String, (Instant, Duration)>,
    /// Last time stale window eviction ran.
    last_eviction: Instant,
}

impl Scheduler {
    /// Creates a new `Scheduler`.
    pub(super) fn new(
        active_epas: Arc<RwLock<HashMap<String, Arc<ExternalPodAutoscaler>>>>,
        pod_cache: Arc<PodCache>,
        client: kube::Client,
        epa_ownership: Arc<EpaOwnership>,
        metrics_store: MetricsStore,
        job_tx: async_channel::Sender<ScrapeJob>,
    ) -> Self {
        Self {
            active_epas,
            pod_cache,
            client,
            epa_ownership,
            metrics_store,
            job_tx,
            next_scrape: HashMap::new(),
            last_eviction: Instant::now(),
        }
    }

    /// Runs the scheduler loop, producing scrape jobs for workers.
    ///
    /// Ticks every second, checks each active EPA for scrape readiness,
    /// resolves target pods, and dispatches `ScrapeJob` items to the worker
    /// pool via the async channel.
    ///
    /// # Errors
    ///
    /// Returns `Ok(())` when the job channel is closed (all workers dropped).
    /// Returns `Err` on fatal scheduling failures.
    pub(super) async fn run(mut self) -> anyhow::Result<()> {
        info!("Scheduler started");

        let mut interval = time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

        loop {
            interval.tick().await;

            if self.job_tx.is_closed() {
                info!("Job channel closed; stopping scheduler");
                return Ok(());
            }

            let tick_now = Instant::now();

            let epas: Vec<(String, Arc<ExternalPodAutoscaler>)> = {
                let active = self.active_epas.read().await;
                active
                    .iter()
                    .map(|(k, v)| (k.clone(), Arc::clone(v)))
                    .collect()
            };

            for (key, epa) in &epas {
                if let Err(e) = self.process_epa(key, epa, tick_now).await {
                    if self.job_tx.is_closed() {
                        info!("Job channel closed; stopping scheduler");
                        return Ok(());
                    }
                    warn!(epa = %key, error = %e, "Failed to process EPA");
                }
            }

            let active_keys: HashSet<&str> = epas.iter().map(|(k, _)| k.as_str()).collect();
            self.next_scrape
                .retain(|key, _| active_keys.contains(key.as_str()));

            if tick_now.duration_since(self.last_eviction) >= Duration::from_secs(60) {
                let evicted = self
                    .metrics_store
                    .evict_stale_windows(Duration::from_secs(120))
                    .await;
                if evicted > 0 {
                    debug!(evicted_count = evicted, "Evicted stale metric windows");
                }
                self.last_eviction = tick_now;
            }
        }
    }

    /// Process a single EPA: check ownership, timing, resolve pods, update
    /// metric configs, and enqueue jobs.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the EPA key is malformed, a duration string is invalid,
    /// `get_pod_selector` fails, the pod cache returns an error, or the job
    /// channel is closed while dispatching.
    async fn process_epa(
        &mut self,
        key: &str,
        epa: &Arc<ExternalPodAutoscaler>,
        now: Instant,
    ) -> anyhow::Result<()> {
        let (namespace, name) = key
            .split_once('/')
            .ok_or_else(|| anyhow!("Invalid EPA key format: {}", key))?;

        let evaluation_period = parse_duration(&epa.spec.scrape.evaluation_period)?;
        let interval_duration = parse_duration(&epa.spec.scrape.interval)?;

        self.epa_ownership
            .refresh_ownership(namespace, name, evaluation_period)
            .await;

        if !self.epa_ownership.should_scrape_epa(namespace, name).await {
            return Ok(());
        }

        if self
            .next_scrape
            .get(key)
            .is_some_and(|(next_time, stored_interval)| {
                stored_interval == &interval_duration && now < *next_time
            })
        {
            return Ok(());
        }
        let interval_secs = interval_duration.as_secs().max(1);
        let max_samples = (evaluation_period.as_secs().div_ceil(interval_secs) as usize).max(1) + 1;

        // Explicit opt-in; default to secure when the TLS block is absent.
        let use_insecure_tls = epa
            .spec
            .scrape
            .tls
            .as_ref()
            .map(|tls| tls.insecure_skip_verify)
            .unwrap_or(false);

        let target_ref = epa
            .spec
            .scrape_target_ref
            .as_ref()
            .unwrap_or(&epa.spec.scale_target_ref);

        let selector = get_pod_selector(&self.client, namespace, target_ref).await?;
        let pods = self.pod_cache.get_ready_pods(namespace, &selector).await?;

        // Inserted before dispatch so the schedule advances consistently regardless
        // of whether the pod list is empty or a send fails mid-loop.
        self.next_scrape
            .insert(key.to_owned(), (now + interval_duration, interval_duration));

        for metric_spec in &epa.spec.metrics {
            let agg_type = metric_spec
                .aggregation_type
                .as_ref()
                .unwrap_or(&epa.spec.scrape.aggregation_type)
                .clone();

            let eval_period = if let Some(period_str) = &metric_spec.evaluation_period {
                parse_duration(period_str)?
            } else {
                evaluation_period
            };

            let config = MetricConfig::new(agg_type, eval_period);
            self.metrics_store
                .set_metric_config(namespace, name, &metric_spec.metric_name, config);
        }

        if pods.is_empty() {
            return Ok(());
        }

        debug!(
            epa = %key,
            pod_count = pods.len(),
            "Dispatching scrape jobs"
        );

        let telemetry = Telemetry::global();

        for pod in pods {
            let job = ScrapeJob {
                epa: Arc::clone(epa),
                pod,
                max_samples,
                use_insecure_tls,
            };

            let enqueue_start = Instant::now();
            let send_result = self.job_tx.send(job).await;
            telemetry
                .enqueue_wait
                .with_label_values(&[name, namespace])
                .observe(enqueue_start.elapsed().as_secs_f64());
            send_result.map_err(|_| {
                anyhow!(
                    "Job channel closed while sending job for EPA {}/{}",
                    namespace,
                    name
                )
            })?;
        }

        Ok(())
    }
}
