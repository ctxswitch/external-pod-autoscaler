use super::{pod_cache::PodCache, telemetry, worker};
use crate::apis::ctx_sh::v1beta1::ExternalPodAutoscaler;
use crate::controller::work_assigner::WorkAssigner;
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

/// Scraper service that manages a pool of workers for collecting metrics.
///
/// The scraper service maintains a pool of workers that periodically scrape metrics
/// from application pods based on EPA specifications. Workers use hash-based load
/// balancing to distribute EPAs across the pool.
///
/// # Architecture
///
/// - Worker pool scrapes metrics from pods on configured intervals
/// - Each EPA is assigned to a worker using consistent hashing
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
    work_assigner: Arc<WorkAssigner>,
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
    /// * `work_assigner` - Work assignment coordinator for distributed scraping
    pub fn new(
        metrics_store: MetricsStore,
        client: Client,
        work_assigner: Arc<WorkAssigner>,
    ) -> Self {
        // Initialize telemetry
        telemetry::Telemetry::init();

        // Get worker count from env or use default (minimum 1 to avoid
        // division by zero in hash-based worker assignment).
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
            work_assigner,
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
    /// Starts the worker pool and update handler, running indefinitely until all workers
    /// and the update channel are closed. Workers periodically scrape metrics from pods
    /// based on EPA specifications using hash-based load balancing.
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` when all workers and handlers have stopped, or an error if worker
    /// initialization fails.
    pub async fn run(mut self) -> Result<()> {
        info!(
            worker_count = self.worker_count,
            "Starting scraper service with {} workers", self.worker_count
        );

        // Spawn worker pool
        let mut worker_handles = Vec::new();
        for worker_id in 0..self.worker_count {
            let worker = worker::Worker::new(
                worker_id,
                self.worker_count,
                self.client.clone(),
                self.pod_cache.clone(),
                self.metrics_store.clone(),
                self.work_assigner.clone(),
                self.active_epas.clone(),
            )?;

            let handle = tokio::spawn(async move {
                worker.run().await;
            });

            worker_handles.push(handle);
        }

        // Run update listener - take ownership of receiver
        let active_epas = self.active_epas.clone();
        let update_rx = self
            .update_rx
            .take()
            .ok_or_else(|| anyhow::anyhow!("update_rx already consumed; run() called twice"))?;
        let update_handle = tokio::spawn(async move {
            Self::handle_updates(update_rx, active_epas).await;
        });

        // Wait for workers and update handler
        for handle in worker_handles {
            if let Err(e) = handle.await {
                warn!(error = %e, "Worker task failed");
            }
        }

        if let Err(e) = update_handle.await {
            warn!(error = %e, "Update handler task failed");
        }

        Ok(())
    }

    /// Handle EPA update messages
    async fn handle_updates(
        mut update_rx: mpsc::Receiver<EpaUpdate>,
        active_epas: Arc<RwLock<HashMap<String, Arc<ExternalPodAutoscaler>>>>,
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
