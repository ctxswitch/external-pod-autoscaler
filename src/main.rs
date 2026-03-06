mod apis;
mod controller;
mod membership;
mod scraper;
mod store;
mod webhook;

use anyhow::{Context, Result};
use kube::Client;
use membership::manager::MembershipManager;
use membership::ownership::EpaOwnership;
use std::sync::Arc;
use std::time::Duration;
use tokio::signal::unix::{signal, SignalKind};
use tracing::{info, warn};
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

    // Register lease and discover peers before starting the controller.
    // This ensures is_epa_owner() has a populated active set when the
    // initial informer sync triggers reconciliation.
    membership
        .register_and_renew()
        .await
        .context("Failed to register initial membership lease")?;
    membership
        .update_active_replicas()
        .await
        .context("Failed to populate initial active replicas")?;

    let epa_ownership = Arc::new(EpaOwnership::new(membership.clone()));

    info!(
        replica_id = %membership.my_replica_id(),
        "Initialized distributed scraping components"
    );

    // Create shared metrics store
    let metrics_store = store::MetricsStore::new();

    // Create scraper service with EPA ownership for distributed scraping
    let scraper_service =
        scraper::ScraperService::new(metrics_store.clone(), client.clone(), epa_ownership.clone());
    let scraper_tx = scraper_service.update_sender();

    // Start cache cleanup background task (runs every 60 seconds)
    let _cleanup_handle = metrics_store
        .clone()
        .spawn_cache_cleanup_task(std::time::Duration::from_secs(60));
    info!("Started cache cleanup background task");

    // Parse drain duration from environment (default: 120 seconds)
    let drain_secs = match std::env::var("DRAIN_DURATION") {
        Ok(val) => val
            .parse::<u64>()
            .context("DRAIN_DURATION must be a non-negative integer")?,
        Err(std::env::VarError::NotPresent) => 120,
        Err(std::env::VarError::NotUnicode(v)) => {
            anyhow::bail!("DRAIN_DURATION contains invalid UTF-8: {:?}", v)
        }
    };
    let drain_duration = Duration::from_secs(drain_secs);

    // Register signal handlers before spawning tasks
    let mut sigterm =
        signal(SignalKind::terminate()).context("Failed to register SIGTERM handler")?;
    let mut sigint =
        signal(SignalKind::interrupt()).context("Failed to register SIGINT handler")?;

    // Spawn all components as tasks so they survive into the drain sequence
    let mut controller_handle = tokio::spawn(controller::run_all(
        metrics_store.clone(),
        scraper_tx,
        epa_ownership.clone(),
    ));
    let mut scraper_handle = tokio::spawn(scraper_service.run());
    let mut webhook_handle = tokio::spawn(run_webhook_server(
        metrics_store,
        epa_ownership,
        membership.clone(),
        webhook_port,
    ));
    let mut membership_handle = tokio::spawn(membership.clone().run());

    // Run until a component exits, a signal arrives, or drain completes
    tokio::select! {
        result = &mut controller_handle => {
            result.context("Controller task panicked")??;
            info!("Controllers stopped");
        },
        result = &mut scraper_handle => {
            result.context("Scraper task panicked")??;
            info!("Scraper service stopped");
        },
        result = &mut webhook_handle => {
            result.context("Webhook task panicked")??;
            info!("Webhook server stopped");
        },
        result = &mut membership_handle => {
            result.context("Membership task panicked")??;
            info!("Membership manager stopped");
        },
        _ = sigterm.recv() => {
            info!("Received SIGTERM, starting drain sequence");
            membership.mark_draining().await;
            info!(drain_duration_secs = drain_duration.as_secs(), "Draining...");
            // Allow the drain to be interrupted by a second signal
            tokio::select! {
                _ = tokio::time::sleep(drain_duration) => {},
                _ = sigterm.recv() => { info!("Second SIGTERM received, aborting drain"); },
                _ = sigint.recv() => { info!("SIGINT during drain, aborting drain"); },
            }
            // Stop the renewal loop and wait for it to finish so it cannot
            // re-create the lease after deletion.
            membership_handle.abort();
            match (&mut membership_handle).await {
                Ok(Err(e)) => warn!(error = %e, "Membership task returned an error before abort"),
                Err(e) if !e.is_cancelled() => warn!(error = %e, "Membership task panicked"),
                _ => {}
            }
            if let Err(e) = membership.delete_lease().await {
                warn!(error = %e, "Failed to delete lease during shutdown");
            }
            info!("Drain complete, shutting down");
        },
        _ = sigint.recv() => {
            // SIGINT is used in development. Skip the drain sleep but still
            // remove the lease so peers do not wait for its TTL to expire.
            membership_handle.abort();
            match (&mut membership_handle).await {
                Ok(Err(e)) => warn!(error = %e, "Membership task returned an error before abort"),
                Err(e) if !e.is_cancelled() => warn!(error = %e, "Membership task panicked"),
                _ => {}
            }
            if let Err(e) = membership.delete_lease().await {
                warn!(error = %e, "Failed to delete lease on SIGINT");
            }
            info!("Received SIGINT, shutting down immediately");
        },
    }

    // Abort remaining tasks so they are not detached on runtime shutdown
    controller_handle.abort();
    scraper_handle.abort();
    webhook_handle.abort();
    membership_handle.abort();

    Ok(())
}

/// Run the webhook server (external metrics API + admission webhooks)
async fn run_webhook_server(
    metrics_store: store::MetricsStore,
    epa_ownership: Arc<EpaOwnership>,
    membership: Arc<MembershipManager>,
    port: u16,
) -> Result<()> {
    let server = webhook::WebhookServer::new(metrics_store, epa_ownership, membership, port)?;
    server.run().await
}
