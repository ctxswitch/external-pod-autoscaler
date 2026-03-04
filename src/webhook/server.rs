use super::{admission, metrics};
use crate::membership::manager::MembershipManager;
use crate::membership::ownership::EpaOwnership;
use crate::store::MetricsStore;
use anyhow::{Context, Result};
use axum::{routing::get, routing::post, Router};
use axum_server::tls_rustls::RustlsConfig;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

/// Shared application state for webhook handlers
#[derive(Clone)]
pub struct AppState {
    pub metrics_store: Arc<MetricsStore>,
    pub epa_ownership: Arc<EpaOwnership>,
    pub membership: Arc<MembershipManager>,
    /// Shared HTTP client for forwarding requests to other replicas.
    /// Skips TLS verification for internal pod-to-pod communication.
    pub forward_client: reqwest::Client,
}

/// Webhook server for external metrics API and admission webhooks.
///
/// Provides three main functions:
///
/// 1. **External Metrics API**: Serves aggregated metrics to the Kubernetes HPA controller
/// 2. **Validating Webhook**: Validates EPA resource specifications before creation/update
/// 3. **Mutating Webhook**: Applies default values to EPA resources
///
/// The server runs over HTTPS with TLS certificates loaded from `/etc/webhook/tls/`.
/// Admission webhooks can be disabled via the `ENABLE_WEBHOOKS` environment variable.
pub struct WebhookServer {
    state: AppState,
    port: u16,
    enable_webhooks: bool,
}

impl WebhookServer {
    /// Creates a new webhook server.
    ///
    /// Admission webhooks are enabled by default but can be disabled by setting
    /// `ENABLE_WEBHOOKS=false` in the environment.
    ///
    /// # Arguments
    ///
    /// * `metrics_store` - Shared metrics store for aggregating and serving metrics
    /// * `epa_ownership` - EPA ownership coordinator for distributed scraping
    /// * `membership` - Membership manager for replica discovery
    /// * `port` - Port to listen on (typically 8443 for webhooks)
    pub fn new(
        metrics_store: MetricsStore,
        epa_ownership: Arc<EpaOwnership>,
        membership: Arc<MembershipManager>,
        port: u16,
    ) -> Result<Self> {
        metrics::telemetry::Telemetry::init();

        // Check environment variable for webhook configuration (default: true)
        let enable_webhooks = std::env::var("ENABLE_WEBHOOKS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(true);

        info!(enable_webhooks = enable_webhooks, "Webhook configuration");

        // Shared HTTP client for forwarding requests between replicas.
        // Uses TLS skip for internal pod-to-pod communication with
        // self-signed certificates.
        let forward_client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .context("Failed to create HTTP client for request forwarding")?;

        Ok(Self {
            state: AppState {
                metrics_store: Arc::new(metrics_store),
                epa_ownership,
                membership,
                forward_client,
            },
            port,
            enable_webhooks,
        })
    }

    /// Runs the webhook server.
    ///
    /// Loads TLS certificates from `/etc/webhook/tls/` and starts the HTTPS server.
    /// The server runs indefinitely until an error occurs or the process is terminated.
    ///
    /// # Routes
    ///
    /// - `GET /apis/external.metrics.k8s.io/v1beta1` - List available metrics
    /// - `GET /apis/external.metrics.k8s.io/v1beta1/namespaces/:namespace/:metric_name` - Get specific metric
    /// - `POST /validate-ctx-sh-v1beta1-externalpodautoscaler` - Validate EPA (if enabled)
    /// - `POST /mutate-ctx-sh-v1beta1-externalpodautoscaler` - Mutate EPA (if enabled)
    ///
    /// # Returns
    ///
    /// Returns an error if TLS certificates cannot be loaded or the server fails to start.
    pub async fn run(self) -> Result<()> {
        let addr = format!("0.0.0.0:{}", self.port);

        // Load TLS certificates
        let cert_path = PathBuf::from("/etc/webhook/tls/tls.crt");
        let key_path = PathBuf::from("/etc/webhook/tls/tls.key");

        info!(
            addr = %addr,
            cert = %cert_path.display(),
            key = %key_path.display(),
            "Starting Webhook server with TLS"
        );

        let tls_config = RustlsConfig::from_pem_file(&cert_path, &key_path)
            .await
            .context("Failed to load TLS certificates for Webhook server")?;

        let app = build_router(self.state, self.enable_webhooks);

        info!(addr = %addr, "Webhook server listening with TLS");

        axum_server::bind_rustls(addr.parse()?, tls_config)
            .serve(app.into_make_service())
            .await?;

        Ok(())
    }
}

/// Builds the axum router with all routes configured.
///
/// Separated from `WebhookServer::run` to allow testing the router
/// without TLS or environment variable reads.
pub(crate) fn build_router(state: AppState, enable_webhooks: bool) -> Router {
    let mut app = Router::new()
        .route(
            "/apis/external.metrics.k8s.io/v1beta1",
            get(metrics::handlers::api_resource_list),
        )
        .route(
            "/apis/external.metrics.k8s.io/v1beta1/namespaces/:namespace/:metric_name",
            get(metrics::handlers::get_external_metric),
        );

    if enable_webhooks {
        info!("Enabling admission webhooks");
        app = app
            .route(
                "/validate-ctx-sh-v1beta1-externalpodautoscaler",
                post(admission::handlers::validate_epa),
            )
            .route(
                "/mutate-ctx-sh-v1beta1-externalpodautoscaler",
                post(admission::handlers::mutate_epa),
            );
    } else {
        info!("Admission webhooks disabled");
    }

    app.with_state(Arc::new(state))
}
