pub mod metric;

use crate::controller::metric::controller::Controller as MetricController;

use anyhow::Result;
use kube::Client;
use std::sync::Arc;
use tracing::{info, error};

pub async fn run_all() -> Result<()> {
    // We need to initialize and pass a metrics store

    // TODO: Setup webhooks
    // TODO: Set up prometheus collection/endpoint

    info!("Starting all controllers");

    tokio::select! {
        result = run_metrics_controller() => {
            result?;
            info!("Stopped metrics controller");
        }
    }

    Ok(())
}

async fn run_metrics_controller() -> Result<()> {
    let client = Client::try_default().await?;

    // TODO: Initialize the telemetry
    let context = Arc::new(metric::Context::new(client));
    let controller = Arc::new(MetricController::new());

    controller.run(context).await?;

    Ok(())
}
