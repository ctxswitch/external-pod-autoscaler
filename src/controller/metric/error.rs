use thiserror::Error;
use tracing::error;

#[derive(Error, Debug)]
pub enum Error{
    #[error("Failed to reconcile Metric; {0}")]
    ReconcileError(String),
    
    #[error("Kubernetes API error; {0}")]
    KubernetesError(String),
    
    #[error("Status updater error; {0}")]
    StatusError(String),
}
