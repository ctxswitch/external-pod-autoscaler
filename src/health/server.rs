use anyhow::Result;
use axum::{Router, extract::State, http::StatusCode, http::header, response::IntoResponse};
use axum::{response::Response, routing::get};
use prometheus::{Encoder, TextEncoder};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::info;

/// Startup conditions that must hold before the replica reports ready.
///
/// Each condition is set once and never cleared; the replica keeps serving
/// while it drains.
#[derive(Default)]
pub struct Readiness {
    tls_loaded: AtomicBool,
    lease_registered: AtomicBool,
    membership_populated: AtomicBool,
}

impl Readiness {
    /// Records that the webhook TLS certificate and key were loaded.
    pub fn mark_tls_loaded(&self) {
        self.tls_loaded.store(true, Ordering::Release);
    }

    /// Records that the membership lease was registered.
    pub fn mark_lease_registered(&self) {
        self.lease_registered.store(true, Ordering::Release);
    }

    /// Records that the active replica set was populated.
    pub fn mark_membership_populated(&self) {
        self.membership_populated.store(true, Ordering::Release);
    }

    /// Returns the names of the unmet conditions in the order `tls`, `lease`,
    /// `membership`. An empty list means the replica is ready.
    pub fn pending(&self) -> Vec<&'static str> {
        [
            ("tls", &self.tls_loaded),
            ("lease", &self.lease_registered),
            ("membership", &self.membership_populated),
        ]
        .into_iter()
        .filter(|(_, met)| !met.load(Ordering::Acquire))
        .map(|(name, _)| name)
        .collect()
    }
}

/// Serves `/metrics`, `/healthz` and `/readyz` over plain HTTP on `listener`.
///
/// Takes a bound listener so the caller can fail fast on a bind error and
/// tests can bind port 0. Runs until the server fails.
pub async fn serve(listener: tokio::net::TcpListener, readiness: Arc<Readiness>) -> Result<()> {
    info!(addr = %listener.local_addr()?, "Health server listening");
    axum::serve(listener, router(readiness)).await?;
    Ok(())
}

/// Builds the router for the health listener.
pub(crate) fn router(readiness: Arc<Readiness>) -> Router {
    Router::new()
        .route("/metrics", get(metrics))
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(readiness)
}

/// Renders the default Prometheus registry in the text exposition format.
async fn metrics() -> Response {
    let encoder = TextEncoder::new();
    let mut buf = Vec::new();
    match encoder.encode(&prometheus::gather(), &mut buf) {
        Ok(()) => (
            [(header::CONTENT_TYPE, encoder.format_type().to_string())],
            buf,
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn healthz() -> &'static str {
    "ok"
}

async fn readyz(State(readiness): State<Arc<Readiness>>) -> impl IntoResponse {
    let pending = readiness.pending();
    if pending.is_empty() {
        (StatusCode::OK, "ok".to_string())
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("not ready: {}", pending.join(", ")),
        )
    }
}
