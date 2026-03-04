use super::{aggregation::aggregate_metric, telemetry::Telemetry, types::*};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, instrument, warn};

/// Handler for discovery endpoint
pub async fn api_resource_list() -> Json<serde_json::Value> {
    info!("Received API discovery request");

    Json(serde_json::json!({
        "kind": "APIResourceList",
        "apiVersion": "v1",
        "groupVersion": "external.metrics.k8s.io/v1beta1",
        "resources": [
            {
                "name": "*",
                "singularName": "",
                "namespaced": true,
                "kind": "ExternalMetricValueList",
                "verbs": ["get"]
            }
        ]
    }))
}

/// Handler for GET /apis/external.metrics.k8s.io/v1beta1/namespaces/{namespace}/{metric_name}
#[instrument(skip(app_state), fields(namespace = %namespace, metric_name = %metric_name))]
pub async fn get_external_metric(
    State(app_state): State<Arc<crate::webhook::server::AppState>>,
    Path((namespace, metric_name)): Path<(String, String)>,
) -> Result<Json<ExternalMetricValueList>, ApiError> {
    let start = std::time::Instant::now();
    let telemetry = Telemetry::global();

    info!(
        namespace = %namespace,
        metric_name = %metric_name,
        "Received external metrics request"
    );

    // Parse metric name: {epa-name}-{epa-namespace}-{metric-name}
    let (epa_name, epa_namespace, actual_metric_name) =
        parse_external_metric_name(&namespace, &metric_name)?;

    info!(
        epa_name = %epa_name,
        epa_namespace = %epa_namespace,
        metric = %actual_metric_name,
        "Parsed external metric name"
    );

    let epa_key = format!("{}/{}", epa_namespace, epa_name);
    if !app_state
        .epa_ownership
        .should_serve_epa(&epa_namespace, &epa_name)
        .await
    {
        let forwarded: Option<ExternalMetricValueList> = 'forward: {
            // Get the owner replica ID
            let owner_id = match app_state
                .epa_ownership
                .get_epa_owner(&epa_namespace, &epa_name)
                .await
            {
                Some(id) => id,
                None => {
                    // No active replicas (shouldn't happen, but be defensive)
                    warn!(epa_key = %epa_key, "No active replicas to handle request");
                    telemetry
                        .api_requests
                        .with_label_values(&[&epa_namespace, &actual_metric_name, "no_replicas"])
                        .inc();
                    break 'forward None;
                }
            };

            // If the hash winner is us but we can't serve yet (evaluation window
            // not filled), forward to the previous owner (the draining replica)
            // instead so that metrics remain available during the transition.
            //
            // Maximum two hops: a non-owner forwards to the active hash winner,
            // which may itself forward once to the draining predecessor. Cycles
            // are impossible: when we are the hash winner, we only forward if
            // get_previous_epa_owner returns a *different* replica (the
            // prev != owner_id guard below filters the case where no draining
            // replica exists and the all-replica HRW winner is still us).
            // Non-owner replicas forward to the active hash winner, never back
            // to themselves.
            let (forward_target, is_draining_forward) = if owner_id
                == app_state.membership.my_replica_id()
            {
                match app_state
                    .epa_ownership
                    .get_previous_epa_owner(&epa_namespace, &epa_name)
                    .await
                {
                    Some(prev) if prev != owner_id => {
                        info!(
                            epa_key = %epa_key,
                            current_owner = %owner_id,
                            previous_owner = %prev,
                            "We are the new hash winner but not serving yet; forwarding to draining replica"
                        );
                        (prev, true)
                    }
                    _ => {
                        warn!(
                            epa_key = %epa_key,
                            owner_id = %owner_id,
                            "We are the hash winner but not serving yet and no draining replica available"
                        );
                        break 'forward None;
                    }
                }
            } else {
                (owner_id.clone(), false)
            };

            // Get the forward target's address
            let owner_address = match app_state
                .membership
                .get_replica_address(&forward_target)
                .await
            {
                Ok(addr) => addr,
                Err(e) => {
                    // Forward target replica is down or lease expired
                    warn!(
                        epa_key = %epa_key,
                        forward_target = %forward_target,
                        error = %e,
                        "Forward target replica unavailable"
                    );

                    telemetry
                        .api_requests
                        .with_label_values(&[&epa_namespace, &actual_metric_name, "owner_down"])
                        .inc();

                    break 'forward None;
                }
            };

            // Forward the HTTP request to the target replica
            let forward_url = format!(
                "https://{}/apis/external.metrics.k8s.io/v1beta1/namespaces/{}/{}",
                owner_address, namespace, metric_name
            );

            info!(
                epa_key = %epa_key,
                forward_target = %forward_target,
                forward_url = %forward_url,
                is_draining_forward = is_draining_forward,
                "Forwarding to {} replica",
                if is_draining_forward { "draining" } else { "owner" }
            );

            let forward_label = if is_draining_forward {
                "forwarded_to_draining"
            } else {
                "forwarded"
            };

            match app_state.forward_client.get(&forward_url).send().await {
                Ok(response) if response.status().is_success() => {
                    // Parse the response as ExternalMetricValueList
                    match response.json::<ExternalMetricValueList>().await {
                        Ok(metrics) => {
                            info!(
                                epa_key = %epa_key,
                                forward_target = %forward_target,
                                "Successfully forwarded request"
                            );

                            telemetry
                                .api_requests
                                .with_label_values(&[
                                    &epa_namespace,
                                    &actual_metric_name,
                                    forward_label,
                                ])
                                .inc();

                            break 'forward Some(metrics);
                        }
                        Err(e) => {
                            warn!(
                                epa_key = %epa_key,
                                forward_target = %forward_target,
                                error = %e,
                                "Failed to parse forwarded response"
                            );

                            telemetry
                                .api_requests
                                .with_label_values(&[
                                    &epa_namespace,
                                    &actual_metric_name,
                                    "forward_failed",
                                ])
                                .inc();

                            break 'forward None;
                        }
                    }
                }
                Ok(response) => {
                    warn!(
                        epa_key = %epa_key,
                        forward_target = %forward_target,
                        status = %response.status(),
                        "Forward request returned non-success status"
                    );

                    telemetry
                        .api_requests
                        .with_label_values(&[&epa_namespace, &actual_metric_name, "forward_failed"])
                        .inc();

                    break 'forward None;
                }
                Err(e) => {
                    warn!(
                        epa_key = %epa_key,
                        forward_target = %forward_target,
                        error = %e,
                        "Forward request failed"
                    );

                    telemetry
                        .api_requests
                        .with_label_values(&[&epa_namespace, &actual_metric_name, "forward_failed"])
                        .inc();

                    break 'forward None;
                }
            }
        };

        if let Some(metrics) = forwarded {
            return Ok(Json(metrics));
        }

        // Forward failed — during ownership transitions, the new owner may not
        // be serving yet. If we have local data, fall through to local processing.
        let windows =
            app_state
                .metrics_store
                .get_windows(&epa_namespace, &epa_name, &actual_metric_name);
        if windows.is_empty() {
            return Err(ApiError::MetricUnavailable(format!(
                "metric {} unavailable during ownership transition",
                actual_metric_name
            )));
        }

        info!(
            epa_key = %epa_key,
            "Forward unavailable, serving from local store during transition"
        );
        // Fall through to local processing
    }

    // This replica owns this EPA - proceed with local processing
    info!(epa_key = %epa_key, "Processing request locally");

    // Check cache first
    if let Some(cached_value) =
        app_state
            .metrics_store
            .get_cached(&epa_namespace, &epa_name, &actual_metric_name)
    {
        info!(
            epa = %epa_name,
            namespace = %epa_namespace,
            metric = %actual_metric_name,
            value = cached_value,
            "Returning cached aggregated value"
        );

        telemetry
            .cache_hits
            .with_label_values(&[&epa_namespace, &actual_metric_name, "hit"])
            .inc();

        telemetry
            .api_requests
            .with_label_values(&[&epa_namespace, &actual_metric_name, "success"])
            .inc();

        // Safely convert f64 to i64 with overflow protection
        let value_i64 = if cached_value >= i64::MAX as f64 {
            i64::MAX
        } else if cached_value <= i64::MIN as f64 {
            i64::MIN
        } else {
            cached_value as i64
        };

        let metric_value = ExternalMetricValue {
            metric_name: metric_name.clone(),
            metric_labels: BTreeMap::new(),
            value: format!("{}", value_i64),
            timestamp: k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                k8s_openapi::jiff::Timestamp::now(),
            ),
        };

        telemetry
            .api_request_duration
            .with_label_values(&[&epa_namespace, &actual_metric_name])
            .observe(start.elapsed().as_secs_f64());

        return Ok(Json(ExternalMetricValueList::new(vec![metric_value])));
    }

    // Cache miss - compute aggregation
    telemetry
        .cache_hits
        .with_label_values(&[&epa_namespace, &actual_metric_name, "miss"])
        .inc();

    // Get windows from store
    let windows =
        app_state
            .metrics_store
            .get_windows(&epa_namespace, &epa_name, &actual_metric_name);

    if windows.is_empty() {
        warn!(
            epa = %epa_name,
            namespace = %epa_namespace,
            metric = %actual_metric_name,
            "No metric windows found"
        );

        telemetry
            .api_requests
            .with_label_values(&[&epa_namespace, &actual_metric_name, "not_found"])
            .inc();

        return Err(ApiError::MetricUnavailable(format!(
            "no metric data available for {}",
            actual_metric_name
        )));
    }

    // Get aggregation configuration from store (set by scraper when EPA is processed)
    let config =
        app_state
            .metrics_store
            .get_metric_config(&epa_namespace, &epa_name, &actual_metric_name);

    // Aggregate across all pods using configured aggregation type and evaluation period
    let aggregated_value =
        aggregate_metric(&windows, &config.aggregation_type, config.evaluation_period).await;

    info!(
        epa = %epa_name,
        namespace = %epa_namespace,
        metric = %actual_metric_name,
        value = aggregated_value,
        pod_count = windows.len(),
        "Computed aggregated value from {} pods",
        windows.len()
    );

    // Cache the result (10s TTL)
    app_state.metrics_store.cache_result(
        &epa_namespace,
        &epa_name,
        &actual_metric_name,
        aggregated_value,
        Duration::from_secs(10),
    );

    // Record success
    telemetry
        .api_requests
        .with_label_values(&[&epa_namespace, &actual_metric_name, "success"])
        .inc();

    telemetry
        .api_request_duration
        .with_label_values(&[&epa_namespace, &actual_metric_name])
        .observe(start.elapsed().as_secs_f64());

    // Safely convert f64 to i64 with overflow protection
    let value_i64 = if aggregated_value >= i64::MAX as f64 {
        i64::MAX
    } else if aggregated_value <= i64::MIN as f64 {
        i64::MIN
    } else {
        aggregated_value as i64
    };

    let metric_value = ExternalMetricValue {
        metric_name: metric_name.clone(),
        metric_labels: BTreeMap::new(),
        value: format!("{}", value_i64),
        timestamp: k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
            k8s_openapi::jiff::Timestamp::now(),
        ),
    };

    Ok(Json(ExternalMetricValueList::new(vec![metric_value])))
}

/// Parse external metric name: {epa-name}-{epa-namespace}-{metric-name}
/// The namespace parameter from the URL is the namespace where the HPA is (same as EPA).
///
/// Prometheus metric names follow the pattern `[a-zA-Z_:][a-zA-Z0-9_:]*` and never contain
/// hyphens, while Kubernetes resource names and namespaces use hyphens. This means the metric
/// name portion is everything after the last hyphen. We then use the known URL namespace to
/// extract the EPA name from the remaining prefix.
pub(crate) fn parse_external_metric_name(
    url_namespace: &str,
    metric_name: &str,
) -> Result<(String, String, String), ApiError> {
    // Find the last hyphen — everything after it is the Prometheus metric name
    // (which cannot contain hyphens per the Prometheus data model)
    let last_hyphen = metric_name.rfind('-').ok_or_else(|| {
        warn!(
            metric_name = %metric_name,
            "Invalid external metric name format: no hyphens found"
        );
        ApiError::BadRequest(format!(
            "Invalid external metric name format: {}",
            metric_name
        ))
    })?;

    let prefix = &metric_name[..last_hyphen]; // {epa-name}-{namespace}
    let actual_metric = &metric_name[last_hyphen + 1..];

    // The prefix must end with -{namespace}
    let ns_suffix = format!("-{}", url_namespace);
    if !prefix.ends_with(&ns_suffix) {
        warn!(
            metric_name = %metric_name,
            url_namespace = %url_namespace,
            prefix = %prefix,
            "External metric name does not contain expected namespace"
        );
        return Err(ApiError::BadRequest(format!(
            "Invalid external metric name format: {}",
            metric_name
        )));
    }

    let epa_name = &prefix[..prefix.len() - ns_suffix.len()];

    if epa_name.is_empty() || actual_metric.is_empty() {
        warn!(
            metric_name = %metric_name,
            "Invalid external metric name format: empty EPA name or metric name"
        );
        return Err(ApiError::BadRequest(format!(
            "Invalid external metric name format: {}",
            metric_name
        )));
    }

    Ok((
        epa_name.to_string(),
        url_namespace.to_string(),
        actual_metric.to_string(),
    ))
}

/// API error type
#[derive(Debug)]
pub enum ApiError {
    #[allow(dead_code)]
    NotFound,
    BadRequest(String),
    #[allow(dead_code)]
    InternalError(String),
    MetricUnavailable(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::NotFound => (StatusCode::NOT_FOUND, "Metric not found").into_response(),
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
            ApiError::InternalError(msg) => {
                (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
            }
            ApiError::MetricUnavailable(msg) => {
                (StatusCode::UNPROCESSABLE_ENTITY, msg).into_response()
            }
        }
    }
}
