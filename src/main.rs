mod apis;
mod controller;
mod scraper;
mod store;
mod webhook;

use anyhow::{Context, Result};
use controller::{membership::MembershipManager, work_assigner::WorkAssigner};
use kube::Client;
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize rustls crypto provider at the very start of the process
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Initialize tracing with JSON or human-readable format based on LOG_FORMAT env var
    let log_format = std::env::var("LOG_FORMAT").unwrap_or_else(|_| "text".to_string());

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "external_pod_autoscaler=info,kube=info".into());

    match log_format.as_str() {
        "json" => {
            // JSON structured logging for production
            tracing_subscriber::registry()
                .with(env_filter)
                .with(fmt::layer().json().flatten_event(true))
                .init();
        }
        _ => {
            // Human-readable logging for development
            tracing_subscriber::registry()
                .with(env_filter)
                .with(fmt::layer().with_target(true).with_thread_ids(false))
                .init();
        }
    }

    info!("Starting External Pod Autoscaler");

    // Create Kubernetes client
    let client = Client::try_default().await?;

    // Read pod identity from environment (injected by Downward API)
    let pod_uid = std::env::var("POD_UID")
        .context("POD_UID environment variable not set (required for distributed scraping)")?;
    let pod_ip = std::env::var("POD_IP")
        .context("POD_IP environment variable not set (required for distributed scraping)")?;
    let pod_name = std::env::var("POD_NAME")
        .context("POD_NAME environment variable not set (required for distributed scraping)")?;
    let pod_namespace = std::env::var("POD_NAMESPACE").context(
        "POD_NAMESPACE environment variable not set (required for distributed scraping)",
    )?;

    info!(
        pod_uid = %pod_uid,
        pod_ip = %pod_ip,
        pod_name = %pod_name,
        pod_namespace = %pod_namespace,
        "Pod identity loaded from environment"
    );

    // Get webhook server port from env or use default
    let webhook_port: u16 = std::env::var("WEBHOOK_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8443);

    info!("Webhook server will listen on port {}", webhook_port);

    // Initialize membership manager for distributed scraping
    let membership = Arc::new(MembershipManager::new(
        client.clone(),
        pod_uid.clone(),
        pod_ip.clone(),
        pod_name.clone(),
        pod_namespace.clone(),
        webhook_port,
    ));

    // Initialize work assigner for EPA ownership determination
    let work_assigner = Arc::new(WorkAssigner::new(membership.clone()));

    info!(
        replica_id = %membership.my_replica_id(),
        "Initialized distributed scraping components"
    );

    // Create shared metrics store
    let metrics_store = store::MetricsStore::new();

    // Create scraper service with work assigner
    let scraper_service =
        scraper::ScraperService::new(metrics_store.clone(), client.clone(), work_assigner.clone());
    let scraper_tx = scraper_service.update_sender();

    // Start cache cleanup background task (runs every 60 seconds)
    let _cleanup_handle = metrics_store
        .clone()
        .spawn_cache_cleanup_task(std::time::Duration::from_secs(60));
    info!("Started cache cleanup background task");

    // Run all components concurrently
    tokio::select! {
        result = controller::run_all(metrics_store.clone(), scraper_tx) => {
            result?;
            info!("Controllers stopped");
        },
        result = scraper_service.run() => {
            result?;
            info!("Scraper service stopped");
        },
        result = run_webhook_server(metrics_store, work_assigner, membership.clone(), webhook_port) => {
            result?;
            info!("Webhook server stopped");
        },
        result = membership.run() => {
            result?;
            info!("Membership manager stopped");
        },
    }

    Ok(())
}

/// Run the webhook server (external metrics API + admission webhooks)
async fn run_webhook_server(
    metrics_store: store::MetricsStore,
    work_assigner: Arc<WorkAssigner>,
    membership: Arc<MembershipManager>,
    port: u16,
) -> Result<()> {
    let server = webhook::WebhookServer::new(metrics_store, work_assigner, membership, port)?;
    server.run().await
}
