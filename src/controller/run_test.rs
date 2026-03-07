use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use k8s_openapi::kube_aggregator::pkg::apis::apiregistration::v1::APIService;
use kube::Api;
use kube::CustomResourceExt;
use kube_fake_client::ClientBuilder;

use crate::apis::ctx_sh::v1beta1::ExternalPodAutoscaler;
use crate::controller::run::{install_crd, register_api_service};

#[tokio::test]
async fn install_crd_creates_when_missing() -> anyhow::Result<()> {
    let client = ClientBuilder::new().build().await?;

    install_crd(&client).await?;

    let crd_api: Api<CustomResourceDefinition> = Api::all(client);
    let crd_name = ExternalPodAutoscaler::crd()
        .metadata
        .name
        .ok_or_else(|| anyhow::anyhow!("CRD metadata missing name"))?;

    let found = crd_api.get(&crd_name).await?;
    assert_eq!(found.metadata.name.as_deref(), Some("epas.ctx.sh"),);

    Ok(())
}

#[tokio::test]
async fn install_crd_updates_when_exists() -> anyhow::Result<()> {
    let mut existing_crd = ExternalPodAutoscaler::crd();
    existing_crd.metadata.resource_version = Some("42".to_string());

    let client = ClientBuilder::new()
        .with_object(existing_crd)
        .build()
        .await?;

    install_crd(&client).await?;

    let crd_api: Api<CustomResourceDefinition> = Api::all(client);
    let crd_name = ExternalPodAutoscaler::crd()
        .metadata
        .name
        .ok_or_else(|| anyhow::anyhow!("CRD metadata missing name"))?;

    let found = crd_api.get(&crd_name).await?;
    assert_eq!(found.metadata.name.as_deref(), Some("epas.ctx.sh"),);

    Ok(())
}

#[tokio::test]
async fn register_api_service_creates() -> anyhow::Result<()> {
    let client = ClientBuilder::new().build().await?;

    register_api_service(&client).await?;

    let api_service_api: Api<APIService> = Api::all(client);
    let api_service_name = "v1beta1.external.metrics.k8s.io";

    let found = api_service_api.get(api_service_name).await?;
    assert_eq!(found.metadata.name.as_deref(), Some(api_service_name),);

    let spec = found
        .spec
        .ok_or_else(|| anyhow::anyhow!("APIService spec should be present"))?;
    assert_eq!(spec.group.as_deref(), Some("external.metrics.k8s.io"));
    assert_eq!(spec.version.as_deref(), Some("v1beta1"));

    Ok(())
}

#[tokio::test]
async fn register_api_service_updates() -> anyhow::Result<()> {
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use k8s_openapi::kube_aggregator::pkg::apis::apiregistration::v1::{
        APIServiceSpec, ServiceReference,
    };

    let existing_api_service = APIService {
        metadata: ObjectMeta {
            name: Some("v1beta1.external.metrics.k8s.io".to_string()),
            resource_version: Some("7".to_string()),
            ..Default::default()
        },
        spec: Some(APIServiceSpec {
            group: Some("external.metrics.k8s.io".to_string()),
            version: Some("v1beta1".to_string()),
            group_priority_minimum: 100,
            version_priority: 100,
            service: Some(ServiceReference {
                namespace: Some("epa-system".to_string()),
                name: Some("epa-webhook".to_string()),
                port: Some(443),
            }),
            insecure_skip_tls_verify: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    };

    let client = ClientBuilder::new()
        .with_object(existing_api_service)
        .build()
        .await?;

    register_api_service(&client).await?;

    let api_service_api: Api<APIService> = Api::all(client);
    let found = api_service_api
        .get("v1beta1.external.metrics.k8s.io")
        .await?;
    assert_eq!(
        found.metadata.name.as_deref(),
        Some("v1beta1.external.metrics.k8s.io"),
    );

    Ok(())
}
