use super::scheduler::Scheduler;
use super::{pod_cache::PodCache, telemetry, worker};
use crate::apis::ctx_sh::v1beta1::ExternalPodAutoscaler;
use crate::membership::ownership::EpaOwnership;
use crate::store::MetricsStore;
use anyhow::Result;
use kube::Client;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

/// Message to notify scraper of EPA changes.
///
/// The controller sends these messages to notify the scraper service when
/// EPA resources are created, updated, or deleted.
#[derive(Debug, Clone)]
pub enum EpaUpdate {
    /// EPA was created or updated
    Upsert(Arc<ExternalPodAutoscaler>),
    /// EPA was deleted with its namespace and name
    Delete { namespace: String, name: String },
}

/// Scraper service that manages a scheduler and worker pool for collecting metrics.
///
/// The scraper service maintains a scheduler that produces per-pod scrape jobs and
/// a pool of workers that consume those jobs through a shared MPMC async channel.
///
/// # Architecture
///
/// - A single scheduler ticks every second, resolves target pods for each active
///   EPA, and dispatches `ScrapeJob` items to the worker pool via an async channel
/// - Workers pull jobs from the shared channel and perform the actual HTTP scrape
/// - Metrics are stored in time-windowed buffers for aggregation
/// - Update channel receives notifications from controller about EPA changes
///
/// # Worker Count
///
/// The number of workers can be configured via the `SCRAPER_WORKERS` environment
/// variable (default: 20).
pub struct ScraperService {
    metrics_store: MetricsStore,
    client: Client,
    pod_cache: Arc<PodCache>,
    worker_count: usize,
    epa_ownership: Arc<EpaOwnership>,
    update_tx: mpsc::Sender<EpaUpdate>,
    update_rx: Option<mpsc::Receiver<EpaUpdate>>,
    // Track active EPAs
    active_epas: Arc<RwLock<HashMap<String, Arc<ExternalPodAutoscaler>>>>,
}

impl ScraperService {
    /// Creates a new scraper service.
    ///
    /// Initializes telemetry and creates a worker pool. The number of workers
    /// can be configured via the `SCRAPER_WORKERS` environment variable (default: 20).
    ///
    /// # Arguments
    ///
    /// * `metrics_store` - Shared store for collected metrics
    /// * `client` - Kubernetes client for discovering pods
    /// * `epa_ownership` - EPA ownership coordinator for distributed scraping
    pub fn new(
        metrics_store: MetricsStore,
        client: Client,
        epa_ownership: Arc<EpaOwnership>,
    ) -> Self {
        // Initialize telemetry
        telemetry::Telemetry::init();

        // Minimum 1: a zero-worker pool would cause the scheduler to block
        // permanently on channel send with no consumers.
        let worker_count = std::env::var("SCRAPER_WORKERS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(20)
            .max(1);

        let (update_tx, update_rx) = mpsc::channel(1000);

        // Shared pod reflector cache: one watcher per namespace, shared across
        // all workers so pod lookups make zero Kubernetes API calls after the
        // initial list-watch.
        let pod_cache = Arc::new(PodCache::new(client.clone()));

        info!(worker_count = worker_count, "Initializing scraper service");

        Self {
            metrics_store,
            client,
            pod_cache,
            worker_count,
            epa_ownership,
            update_tx,
            update_rx: Some(update_rx),
            active_epas: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Returns a sender for EPA updates.
    ///
    /// The controller uses this sender to notify the scraper service when EPAs
    /// are created, updated, or deleted.
    pub fn update_sender(&self) -> mpsc::Sender<EpaUpdate> {
        self.update_tx.clone()
    }

    /// Runs the scraper service.
    ///
    /// Starts the scheduler, worker pool, and update handler, running indefinitely
    /// until all tasks complete. The scheduler produces per-pod scrape jobs that
    /// workers consume from a shared async channel.
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` when all workers, the scheduler, and handlers have stopped,
    /// or an error if worker initialization fails.
    pub async fn run(mut self) -> Result<()> {
        info!(
            worker_count = self.worker_count,
            "Starting scraper service with {} workers", self.worker_count
        );

        // 64 slots per worker provides roughly one scrape-cycle worth of
        // buffering before the scheduler blocks under backpressure.
        let (job_tx, job_rx) = async_channel::bounded(self.worker_count * 64);

        // Spawn worker pool
        let mut worker_handles = Vec::new();
        for _ in 0..self.worker_count {
            let worker = worker::Worker::new(job_rx.clone(), self.metrics_store.clone())?;

            let handle = tokio::spawn(async move {
                worker.run().await;
            });

            worker_handles.push(handle);
        }

        // Spawn the scheduler
        let scheduler = Scheduler::new(
            self.active_epas.clone(),
            self.pod_cache.clone(),
            self.client.clone(),
            self.epa_ownership.clone(),
            self.metrics_store.clone(),
            job_tx,
        );
        let scheduler_handle = tokio::spawn(async move {
            if let Err(e) = scheduler.run().await {
                warn!(error = %e, "Scheduler failed");
            }
        });

        // Run update listener - take ownership of receiver
        let active_epas = self.active_epas.clone();
        let metrics_store = self.metrics_store.clone();
        let update_rx = self
            .update_rx
            .take()
            .ok_or_else(|| anyhow::anyhow!("update_rx already consumed; run() called twice"))?;
        let update_handle = tokio::spawn(async move {
            Self::handle_updates(update_rx, active_epas, metrics_store).await;
        });

        // Wait for workers, scheduler, and update handler
        for handle in worker_handles {
            if let Err(e) = handle.await {
                warn!(error = %e, "Worker task failed");
            }
        }

        if let Err(e) = scheduler_handle.await {
            warn!(error = %e, "Scheduler task failed");
        }

        if let Err(e) = update_handle.await {
            warn!(error = %e, "Update handler task failed");
        }

        Ok(())
    }

    /// Handle EPA update messages.
    async fn handle_updates(
        mut update_rx: mpsc::Receiver<EpaUpdate>,
        active_epas: Arc<RwLock<HashMap<String, Arc<ExternalPodAutoscaler>>>>,
        metrics_store: MetricsStore,
    ) {
        loop {
            let msg = update_rx.recv().await;

            match msg {
                Some(EpaUpdate::Upsert(epa)) => {
                    let namespace = match epa.metadata.namespace.as_deref() {
                        Some(ns) => ns,
                        None => {
                            warn!("Received EPA upsert with no namespace; skipping");
                            continue;
                        }
                    };
                    let name = match epa.metadata.name.as_deref() {
                        Some(n) => n,
                        None => {
                            warn!("Received EPA upsert with no name; skipping");
                            continue;
                        }
                    };
                    let key = format!("{}/{}", namespace, name);

                    info!(epa = %key, "Registering EPA for scraping");

                    // Pre-populate metric configs so the webhook handler can
                    // respond before the first scheduler tick. The scheduler
                    // also writes configs each tick to pick up spec changes.
                    let evaluation_period = match super::worker::parse_duration(
                        &epa.spec.scrape.evaluation_period,
                    ) {
                        Ok(d) => d,
                        Err(e) => {
                            warn!(epa = %key, error = %e, "Invalid evaluation_period; skipping metric config registration");
                            // Still register the EPA; the scheduler will
                            // re-register configs on the first tick.
                            let mut epas = active_epas.write().await;
                            epas.insert(key, epa);
                            continue;
                        }
                    };

                    for metric_spec in &epa.spec.metrics {
                        let agg_type = metric_spec
                            .aggregation_type
                            .as_ref()
                            .unwrap_or(&epa.spec.scrape.aggregation_type)
                            .clone();
                        let eval_period = if let Some(period_str) = &metric_spec.evaluation_period {
                            match super::worker::parse_duration(period_str) {
                                Ok(d) => d,
                                Err(e) => {
                                    warn!(epa = %key, error = %e, "Invalid metric evaluation_period");
                                    continue;
                                }
                            }
                        } else {
                            evaluation_period
                        };
                        let config = crate::store::MetricConfig::new(agg_type, eval_period);
                        metrics_store.set_metric_config(
                            namespace,
                            name,
                            &metric_spec.metric_name,
                            config,
                        );
                    }

                    let mut epas = active_epas.write().await;
                    epas.insert(key, epa);
                }
                Some(EpaUpdate::Delete { namespace, name }) => {
                    let key = format!("{}/{}", namespace, name);

                    info!(epa = %key, "Unregistering EPA from scraping");

                    let mut epas = active_epas.write().await;
                    epas.remove(&key);
                }
                None => {
                    info!("Update channel closed, stopping update handler");
                    break;
                }
            }
        }
    }
}
