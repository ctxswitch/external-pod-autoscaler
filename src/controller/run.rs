use super::externalpodautoscaler;
use crate::apis::ctx_sh::v1beta1::ExternalPodAutoscaler;
use crate::membership::ownership::EpaOwnership;
use anyhow::Result;
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use k8s_openapi::kube_aggregator::pkg::apis::apiregistration::v1::{
    APIService, APIServiceSpec, ServiceReference,
};
use kube::{api::PostParams, Api, Client, CustomResourceExt};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// Run all controllers concurrently
pub async fn run_all(
    metrics_store: crate::store::MetricsStore,
    scraper_tx: mpsc::Sender<crate::scraper::EpaUpdate>,
    epa_ownership: Arc<EpaOwnership>,
) -> Result<()> {
    info!("Starting all controllers");

    // Run ExternalPodAutoscaler controller
    run_epa_controller(metrics_store, scraper_tx, epa_ownership).await?;

    Ok(())
}

/// Run the ExternalPodAutoscaler controller
async fn run_epa_controller(
    metrics_store: crate::store::MetricsStore,
    scraper_tx: mpsc::Sender<crate::scraper::EpaUpdate>,
    epa_ownership: Arc<EpaOwnership>,
) -> Result<()> {
    let client = Client::try_default().await?;

    // Install CRD before starting controller
    install_crd(&client).await?;

    // Register APIService for external metrics
    register_api_service(&client).await?;

    // Initialize telemetry
    externalpodautoscaler::telemetry::Telemetry::init();

    info!("Starting ExternalPodAutoscaler controller");

    let controller =
        externalpodautoscaler::Controller::new(client, scraper_tx, metrics_store, epa_ownership);
    controller.run().await?;

    Ok(())
}

/// Install or update the ExternalPodAutoscaler CRD
pub(crate) async fn install_crd(client: &Client) -> Result<()> {
    let crd_api: Api<CustomResourceDefinition> = Api::all(client.clone());
    let crd = ExternalPodAutoscaler::crd();
    let crd_name = crd
        .metadata
        .name
        .clone()
        .ok_or_else(|| anyhow::anyhow!("CRD metadata missing name field"))?;

    info!("Installing CRD: {}", crd_name);

    match crd_api.get(&crd_name).await {
        Ok(existing) => {
            // CRD exists, update it
            info!("CRD already exists, updating: {}", crd_name);

            // Preserve resourceVersion for update
            let mut updated_crd = crd;
            updated_crd.metadata.resource_version = existing.metadata.resource_version;

            match crd_api
                .replace(&crd_name, &PostParams::default(), &updated_crd)
                .await
            {
                Ok(_) => info!("Successfully updated CRD: {}", crd_name),
                Err(e) => {
                    warn!("Failed to update CRD (may not be necessary): {}", e);
                }
            }
        }
        Err(_) => {
            // CRD doesn't exist, create it
            info!("CRD does not exist, creating: {}", crd_name);
            crd_api.create(&PostParams::default(), &crd).await?;
            info!("Successfully created CRD: {}", crd_name);
        }
    }

    Ok(())
}

/// Register APIService for external metrics API
pub(crate) async fn register_api_service(client: &Client) -> Result<()> {
    let api_service_api: Api<APIService> = Api::all(client.clone());
    let api_service_name = "v1beta1.external.metrics.k8s.io";

    info!("Registering APIService: {}", api_service_name);

    // Get service namespace and name from environment or use defaults
    let service_namespace =
        std::env::var("SERVICE_NAMESPACE").unwrap_or_else(|_| "epa-system".to_string());
    let service_name = std::env::var("SERVICE_NAME").unwrap_or_else(|_| "epa-webhook".to_string());

    let api_service = APIService {
        metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
            name: Some(api_service_name.to_string()),
            ..Default::default()
        },
        spec: Some(APIServiceSpec {
            service: Some(ServiceReference {
                name: Some(service_name.clone()),
                namespace: Some(service_namespace.clone()),
                port: Some(443),
            }),
            group: Some("external.metrics.k8s.io".to_string()),
            version: Some("v1beta1".to_string()),
            insecure_skip_tls_verify: Some(false),
            group_priority_minimum: 100,
            version_priority: 100,
            ca_bundle: None, // Will be injected by cert-manager
        }),
        status: None,
    };

    match api_service_api.get(api_service_name).await {
        Ok(existing) => {
            // APIService exists, update it
            info!("APIService already exists, updating: {}", api_service_name);

            let mut updated = api_service;
            updated.metadata.resource_version = existing.metadata.resource_version;

            match api_service_api
                .replace(api_service_name, &PostParams::default(), &updated)
                .await
            {
                Ok(_) => info!("Successfully updated APIService: {}", api_service_name),
                Err(e) => {
                    warn!("Failed to update APIService (may not be necessary): {}", e);
                }
            }
        }
        Err(_) => {
            // APIService doesn't exist, create it
            info!("APIService does not exist, creating: {}", api_service_name);
            api_service_api
                .create(&PostParams::default(), &api_service)
                .await?;
            info!("Successfully created APIService: {}", api_service_name);
        }
    }

    Ok(())
}
