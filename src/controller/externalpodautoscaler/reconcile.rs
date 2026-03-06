use crate::apis::ctx_sh::v1beta1::ExternalPodAutoscaler;
use crate::controller::externalpodautoscaler::observer::{ObservedState, StateObserver};
use crate::controller::externalpodautoscaler::telemetry::Telemetry;
use crate::membership::ownership::EpaOwnership;
use crate::scraper::EpaUpdate;
use crate::store::MetricsStore;
use k8s_openapi::api::autoscaling::v2::{
    CrossVersionObjectReference, HorizontalPodAutoscaler, HorizontalPodAutoscalerSpec,
    MetricIdentifier, MetricSpec as HpaMetricSpec, MetricTarget,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::runtime::controller::Action;
use kube::{api::Patch, api::PatchParams, Api};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{error, info, instrument, warn};

const FIELD_MANAGER: &str = "external-pod-autoscaler";

#[derive(Debug, Error)]
pub enum Error {
    #[error("Kubernetes API error: {0}")]
    Kube(#[from] kube::Error),

    #[error("Observation error: {0}")]
    Observation(#[from] anyhow::Error),

    #[error("HPA management error: {0}")]
    Hpa(String),
}

/// Reconciler for ExternalPodAutoscaler resources.
///
/// Combines the reconciliation logic and shared state needed for EPA processing.
/// Passed as the kube-runtime context via `Arc<Reconciler>`.
pub struct Reconciler {
    client: kube::Client,
    scraper_tx: mpsc::Sender<EpaUpdate>,
    metrics_store: MetricsStore,
    epa_ownership: Arc<EpaOwnership>,
}

/// Extract the name and namespace from an EPA's metadata, returning an error if either is missing.
fn epa_name_namespace(epa: &ExternalPodAutoscaler) -> Result<(&str, &str), Error> {
    let name = epa
        .metadata
        .name
        .as_deref()
        .ok_or_else(|| Error::Observation(anyhow::anyhow!("EPA missing metadata.name")))?;
    let namespace = epa
        .metadata
        .namespace
        .as_deref()
        .ok_or_else(|| Error::Observation(anyhow::anyhow!("EPA missing metadata.namespace")))?;
    Ok((name, namespace))
}

impl Reconciler {
    pub fn new(
        client: kube::Client,
        scraper_tx: mpsc::Sender<EpaUpdate>,
        metrics_store: MetricsStore,
        epa_ownership: Arc<EpaOwnership>,
    ) -> Self {
        Self {
            client,
            scraper_tx,
            metrics_store,
            epa_ownership,
        }
    }

    // Reconciler is both the handler (&self) and the kube-runtime context (ctx).
    // They point to the same Arc allocation; we use &self and ignore the redundant ctx.
    #[instrument(skip(self, epa, _ctx), fields(epa = %epa.metadata.name.as_deref().unwrap_or("unknown")))]
    pub async fn reconcile(
        &self,
        epa: Arc<ExternalPodAutoscaler>,
        _ctx: Arc<Reconciler>,
    ) -> Result<Action, Error> {
        let start = std::time::Instant::now();
        let (name_ref, namespace_ref) = epa_name_namespace(&epa)?;
        let name = name_ref.to_string();
        let namespace = namespace_ref.to_string();

        info!("Reconciling ExternalPodAutoscaler {}/{}", namespace, name);

        let telemetry = Telemetry::global();

        // Observe current state
        let observer = StateObserver::new(self.client.clone(), namespace.clone(), name.clone());
        let mut observed = ObservedState::new();
        observer.observe(&mut observed).await?;

        // If the EPA doesn't exist, nothing to do
        if !observed.epa_exists() {
            info!(
                "ExternalPodAutoscaler {}/{} not found, skipping",
                namespace, name
            );
            return Ok(Action::await_change());
        }

        let epa_ref = observed.epa().ok_or_else(|| {
            Error::Observation(anyhow::anyhow!(
                "EPA not found after observation confirmed existence"
            ))
        })?;

        let is_owner = self.epa_ownership.is_epa_owner(&namespace, &name).await;

        if observed.is_deleting() {
            if is_owner {
                info!(
                    "ExternalPodAutoscaler {}/{} is being deleted",
                    namespace, name
                );

                if let Err(e) = self
                    .scraper_tx
                    .send(EpaUpdate::Delete {
                        namespace: namespace.clone(),
                        name: name.clone(),
                    })
                    .await
                {
                    warn!(
                        epa = %name,
                        namespace = %namespace,
                        error = %e,
                        "Failed to notify scraper of EPA deletion"
                    );
                }

                self.metrics_store.remove_epa_windows(&namespace, &name);
            }

            return Ok(Action::await_change());
        }

        if is_owner {
            let hpa_result = self.reconcile_hpa(epa_ref).await;

            match hpa_result {
                Ok(()) => {
                    self.patch_epa_status(
                        &name,
                        &namespace,
                        true,
                        "HpaSynced",
                        "HPA successfully reconciled",
                    )
                    .await;
                }
                Err(e) => {
                    self.patch_epa_status(&name, &namespace, false, "HpaError", &e.to_string())
                        .await;
                    return Err(e);
                }
            }
        } else {
            info!(
                epa = %name,
                namespace = %namespace,
                "Skipping HPA management (not owner)"
            );
        }

        if let Err(e) = self
            .scraper_tx
            .send(EpaUpdate::Upsert((*epa_ref).clone()))
            .await
        {
            error!(
                epa = %name,
                namespace = %namespace,
                error = %e,
                "Failed to notify scraper service"
            );
        }

        // Record metrics
        telemetry
            .reconcile_duration
            .with_label_values(&[&name, &namespace])
            .observe(start.elapsed().as_secs_f64());

        Ok(Action::await_change())
    }

    /// Error policy called by the kube-runtime controller on reconcile failures.
    ///
    /// Logs the error and schedules a requeue after 60 seconds to allow transient
    /// issues to resolve before the next attempt.
    pub fn error_policy(
        &self,
        epa: Arc<ExternalPodAutoscaler>,
        error: &Error,
        _ctx: Arc<Reconciler>,
    ) -> Action {
        let name = epa.metadata.name.as_deref().unwrap_or("unknown");
        let namespace = epa.metadata.namespace.as_deref().unwrap_or("unknown");
        let error_type = match error {
            Error::Kube(_) => "kube",
            Error::Observation(_) => "observation",
            Error::Hpa(_) => "hpa",
        };
        Telemetry::global()
            .reconcile_errors
            .with_label_values(&[name, namespace, error_type])
            .inc();
        error!("ExternalPodAutoscaler reconciliation error: {:?}", error);
        Action::requeue(Duration::from_secs(60))
    }

    /// Patches the EPA status sub-resource with HPA sync details and a Ready condition.
    ///
    /// Replica counts (currentReplicas/desiredReplicas) are intentionally not tracked
    /// here -- the HPA owns that state and operators can query it directly. The EPA
    /// status focuses on what only the controller knows: managed HPA identity and
    /// reconciliation health.
    ///
    /// Failures are treated as non-fatal: a warning is logged and the status patch error
    /// is swallowed so that the parent reconcile result is not affected.
    ///
    /// # Arguments
    ///
    /// * `name` - Name of the EPA (and the managed HPA, which share the same name)
    /// * `namespace` - Namespace the EPA lives in
    /// * `ready` - Whether reconciliation succeeded (`true`) or encountered an HPA error (`false`)
    /// * `reason` - Short CamelCase reason string for the `Ready` condition
    /// * `message` - Human-readable message for the `Ready` condition
    async fn patch_epa_status(
        &self,
        name: &str,
        namespace: &str,
        ready: bool,
        reason: &str,
        message: &str,
    ) {
        let now = k8s_openapi::jiff::Timestamp::now().to_string();
        let condition_status = if ready { "True" } else { "False" };

        // Preserve lastTransitionTime when the condition state hasn't changed.
        // Only update the timestamp on actual True<->False transitions.
        let api: Api<ExternalPodAutoscaler> = Api::namespaced(self.client.clone(), namespace);

        let transition_time = match api.get(name).await {
            Ok(existing) => {
                let existing_ready = existing
                    .status
                    .as_ref()
                    .map(|s| &s.conditions)
                    .and_then(|conds| conds.iter().find(|c| c.type_ == "Ready"));

                match existing_ready {
                    Some(c) if c.status == condition_status => {
                        // State unchanged -- preserve existing transition time
                        c.last_transition_time.clone()
                    }
                    _ => now.clone(),
                }
            }
            Err(e) => {
                warn!(
                    epa = %name,
                    namespace = %namespace,
                    error = %e,
                    "Failed to read EPA for condition check -- using current time"
                );
                now.clone()
            }
        };

        let patch = serde_json::json!({
            "status": {
                "managedHpa": {
                    "name": name,
                    "lastSyncTime": now,
                },
                "conditions": [
                    {
                        "type": "Ready",
                        "status": condition_status,
                        "lastTransitionTime": transition_time,
                        "reason": reason,
                        "message": message,
                    }
                ]
            }
        });

        let params = PatchParams::default();

        if let Err(e) = api.patch_status(name, &params, &Patch::Merge(&patch)).await {
            warn!(
                epa = %name,
                namespace = %namespace,
                error = %e,
                "Failed to patch EPA status -- continuing without status update"
            );
        }
    }

    /// Reconcile the managed HPA via Server-Side Apply.
    ///
    /// If the HPA does not yet exist, SSA creates it. Otherwise it updates only
    /// the fields owned by our field manager.
    async fn reconcile_hpa(&self, epa: &ExternalPodAutoscaler) -> Result<(), Error> {
        let (name, namespace) = epa_name_namespace(epa)?;

        let hpa_api: Api<HorizontalPodAutoscaler> = Api::namespaced(self.client.clone(), namespace);
        let desired_hpa = Self::build_hpa_spec(epa)?;
        let telemetry = Telemetry::global();

        info!("Applying HPA {}/{}", namespace, name);

        match hpa_api
            .patch(
                name,
                &PatchParams::apply(FIELD_MANAGER).force(),
                &Patch::Apply(&desired_hpa),
            )
            .await
        {
            Ok(_) => {
                telemetry
                    .hpa_operations
                    .with_label_values(&[name, namespace, "apply"])
                    .inc();
                info!("Successfully applied HPA {}/{}", namespace, name);
            }
            Err(e) => {
                error!("Failed to apply HPA {}/{}: {}", namespace, name, e);
                return Err(Error::Hpa(format!("Failed to apply HPA: {e}")));
            }
        }

        Ok(())
    }

    /// Build HPA spec from EPA
    fn build_hpa_spec(epa: &ExternalPodAutoscaler) -> Result<HorizontalPodAutoscaler, Error> {
        let (name, namespace) = epa_name_namespace(epa)?;

        // Build owner reference for automatic cleanup
        // UID is required for garbage collection to work properly
        let uid = epa
            .metadata
            .uid
            .as_ref()
            .ok_or_else(|| {
                Error::Hpa(format!(
                    "ExternalPodAutoscaler {namespace}/{name} missing required UID for owner reference"
                ))
            })?
            .clone();

        let owner_ref = OwnerReference {
            api_version: "ctx.sh/v1beta1".to_string(),
            kind: "ExternalPodAutoscaler".to_string(),
            name: name.to_string(),
            uid,
            controller: Some(true),
            block_owner_deletion: Some(true),
        };

        // Build metric specs for HPA
        let hpa_metrics: Vec<HpaMetricSpec> = epa
            .spec
            .metrics
            .iter()
            .map(|metric_spec| {
                // External metric name: {epa-name}-{epa-namespace}-{metric-name}
                let external_metric_name =
                    format!("{name}-{namespace}-{}", metric_spec.metric_name);

                HpaMetricSpec {
                    external: Some(k8s_openapi::api::autoscaling::v2::ExternalMetricSource {
                        metric: MetricIdentifier {
                            name: external_metric_name,
                            selector: None, // Label selector handled in our API
                        },
                        target: MetricTarget {
                            average_value: if matches!(
                                metric_spec.type_,
                                crate::apis::ctx_sh::v1beta1::MetricTargetType::AverageValue
                            ) {
                                Some(k8s_openapi::apimachinery::pkg::api::resource::Quantity(
                                    metric_spec.target_value.clone(),
                                ))
                            } else {
                                None
                            },
                            value: if matches!(
                                metric_spec.type_,
                                crate::apis::ctx_sh::v1beta1::MetricTargetType::Value
                            ) {
                                Some(k8s_openapi::apimachinery::pkg::api::resource::Quantity(
                                    metric_spec.target_value.clone(),
                                ))
                            } else {
                                None
                            },
                            type_: if matches!(
                                metric_spec.type_,
                                crate::apis::ctx_sh::v1beta1::MetricTargetType::AverageValue
                            ) {
                                "AverageValue".to_string()
                            } else {
                                "Value".to_string()
                            },
                            ..Default::default()
                        },
                    }),
                    type_: "External".to_string(),
                    ..Default::default()
                }
            })
            .collect();

        let hpa = HorizontalPodAutoscaler {
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                name: Some(name.to_string()),
                namespace: Some(namespace.to_string()),
                owner_references: Some(vec![owner_ref]),
                ..Default::default()
            },
            spec: Some(HorizontalPodAutoscalerSpec {
                scale_target_ref: CrossVersionObjectReference {
                    api_version: Some(epa.spec.scale_target_ref.api_version.clone()),
                    kind: epa.spec.scale_target_ref.kind.clone(),
                    name: epa.spec.scale_target_ref.name.clone(),
                },
                min_replicas: Some(epa.spec.min_replicas),
                max_replicas: epa.spec.max_replicas,
                metrics: Some(hpa_metrics),
                behavior: epa.spec.behavior.as_ref().map(|b| {
                    k8s_openapi::api::autoscaling::v2::HorizontalPodAutoscalerBehavior {
                        scale_up: b.scale_up.as_ref().map(|su| {
                            k8s_openapi::api::autoscaling::v2::HPAScalingRules {
                                stabilization_window_seconds: su.stabilization_window_seconds,
                                policies: su.policies.as_ref().map(|policies| {
                                    policies
                                        .iter()
                                        .map(|p| {
                                            k8s_openapi::api::autoscaling::v2::HPAScalingPolicy {
                                                type_: p.type_.clone(),
                                                value: p.value,
                                                period_seconds: p.period_seconds,
                                            }
                                        })
                                        .collect()
                                }),
                                select_policy: su.select_policy.clone(),
                                // Not yet exposed in the EPA CRD spec (k8s 1.31 alpha feature).
                                tolerance: None,
                            }
                        }),
                        scale_down: b.scale_down.as_ref().map(|sd| {
                            k8s_openapi::api::autoscaling::v2::HPAScalingRules {
                                stabilization_window_seconds: sd.stabilization_window_seconds,
                                policies: sd.policies.as_ref().map(|policies| {
                                    policies
                                        .iter()
                                        .map(|p| {
                                            k8s_openapi::api::autoscaling::v2::HPAScalingPolicy {
                                                type_: p.type_.clone(),
                                                value: p.value,
                                                period_seconds: p.period_seconds,
                                            }
                                        })
                                        .collect()
                                }),
                                select_policy: sd.select_policy.clone(),
                                // Not yet exposed in the EPA CRD spec (k8s 1.31 alpha feature).
                                tolerance: None,
                            }
                        }),
                    }
                }),
            }),
            ..Default::default()
        };

        Ok(hpa)
    }
}
