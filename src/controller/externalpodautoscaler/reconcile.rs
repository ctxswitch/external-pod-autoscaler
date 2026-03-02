use crate::scraper::EpaUpdate;
use thiserror::Error;
use tokio::sync::mpsc;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Kubernetes API error: {0}")]
    Kube(#[from] kube::Error),

    #[error("Observation error: {0}")]
    Observation(#[from] anyhow::Error),

    #[error("HPA management error: {0}")]
    Hpa(String),
}

/// Shared context for the ExternalPodAutoscaler controller.
///
/// Passed to each reconciliation call via `Arc<Context>`.
pub struct Context {
    /// Kubernetes API client for cluster operations.
    pub client: kube::Client,
    /// Channel to notify the scraper service of EPA changes.
    pub scraper_tx: mpsc::Sender<EpaUpdate>,
}

impl Context {
    /// Creates a new controller context.
    pub fn new(client: kube::Client, scraper_tx: mpsc::Sender<EpaUpdate>) -> Self {
        Self { client, scraper_tx }
    }
}
