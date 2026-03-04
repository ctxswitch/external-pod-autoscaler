use crate::apis::ctx_sh::v1beta1::ExternalPodAutoscaler;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tracing::{info, warn};

/// AdmissionReview request wrapper
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionReview {
    pub api_version: String,
    pub kind: String,
    pub request: Option<AdmissionRequest>,
    pub response: Option<AdmissionResponse>,
}

/// AdmissionRequest
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionRequest {
    pub uid: String,
    pub kind: GroupVersionKind,
    pub resource: GroupVersionResource,
    pub operation: String,
    pub object: Option<Value>,
    pub old_object: Option<Value>,
}

/// GroupVersionKind
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupVersionKind {
    pub group: String,
    pub version: String,
    pub kind: String,
}

/// GroupVersionResource
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupVersionResource {
    pub group: String,
    pub version: String,
    pub resource: String,
}

/// AdmissionResponse
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionResponse {
    pub uid: String,
    pub allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<StatusDetails>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch_type: Option<String>,
}

/// Status details for denied requests
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StatusDetails {
    pub code: u16,
    pub message: String,
}

/// Validating webhook handler for EPA
pub async fn validate_epa(
    State(_app_state): State<Arc<crate::webhook::server::AppState>>,
    Json(review): Json<AdmissionReview>,
) -> Result<Json<AdmissionReview>, AdmissionError> {
    let request = review.request.ok_or_else(|| {
        AdmissionError::BadRequest("Missing request in AdmissionReview".to_string())
    })?;

    info!(
        uid = %request.uid,
        operation = %request.operation,
        "Received validation request for EPA"
    );

    // Parse the EPA object
    let epa: ExternalPodAutoscaler = match &request.object {
        Some(obj) => serde_json::from_value(obj.clone()).map_err(|e| {
            AdmissionError::BadRequest(format!("Failed to parse EPA object: {}", e))
        })?,
        None => {
            return Ok(Json(AdmissionReview {
                api_version: "admission.k8s.io/v1".to_string(),
                kind: "AdmissionReview".to_string(),
                request: None,
                response: Some(AdmissionResponse {
                    uid: request.uid,
                    allowed: true,
                    status: None,
                    patch: None,
                    patch_type: None,
                }),
            }));
        }
    };

    // Validate the EPA
    if let Err(msg) = validate_epa_spec(&epa) {
        warn!(
            uid = %request.uid,
            error = %msg,
            "EPA validation failed"
        );

        return Ok(Json(AdmissionReview {
            api_version: "admission.k8s.io/v1".to_string(),
            kind: "AdmissionReview".to_string(),
            request: None,
            response: Some(AdmissionResponse {
                uid: request.uid,
                allowed: false,
                status: Some(StatusDetails {
                    code: 400,
                    message: msg,
                }),
                patch: None,
                patch_type: None,
            }),
        }));
    }

    info!(uid = %request.uid, "EPA validation passed");

    Ok(Json(AdmissionReview {
        api_version: "admission.k8s.io/v1".to_string(),
        kind: "AdmissionReview".to_string(),
        request: None,
        response: Some(AdmissionResponse {
            uid: request.uid,
            allowed: true,
            status: None,
            patch: None,
            patch_type: None,
        }),
    }))
}

/// Mutating webhook handler for EPA
pub async fn mutate_epa(
    State(_app_state): State<Arc<crate::webhook::server::AppState>>,
    Json(review): Json<AdmissionReview>,
) -> Result<Json<AdmissionReview>, AdmissionError> {
    let request = review.request.ok_or_else(|| {
        AdmissionError::BadRequest("Missing request in AdmissionReview".to_string())
    })?;

    info!(
        uid = %request.uid,
        operation = %request.operation,
        "Received mutation request for EPA"
    );

    // Get the original object
    let original_obj = match &request.object {
        Some(obj) => obj.clone(),
        None => {
            return Ok(Json(AdmissionReview {
                api_version: "admission.k8s.io/v1".to_string(),
                kind: "AdmissionReview".to_string(),
                request: None,
                response: Some(AdmissionResponse {
                    uid: request.uid,
                    allowed: true,
                    status: None,
                    patch: None,
                    patch_type: None,
                }),
            }));
        }
    };

    // Parse the EPA object
    let mut epa: ExternalPodAutoscaler = serde_json::from_value(original_obj.clone())
        .map_err(|e| AdmissionError::BadRequest(format!("Failed to parse EPA object: {}", e)))?;

    // Apply defaults directly to the EPA
    apply_defaults(&mut epa);

    // Serialize the modified EPA
    let modified_obj = serde_json::to_value(&epa)
        .map_err(|e| AdmissionError::InternalError(format!("Failed to serialize EPA: {}", e)))?;

    // Generate patch from original to modified
    let patch = generate_patch(&original_obj, &modified_obj);

    let has_patch = patch.is_some();
    let patch_type = if has_patch {
        Some("JSONPatch".to_string())
    } else {
        None
    };

    info!(
        uid = %request.uid,
        has_patch = has_patch,
        "EPA mutation completed"
    );

    Ok(Json(AdmissionReview {
        api_version: "admission.k8s.io/v1".to_string(),
        kind: "AdmissionReview".to_string(),
        request: None,
        response: Some(AdmissionResponse {
            uid: request.uid,
            allowed: true,
            status: None,
            patch,
            patch_type,
        }),
    }))
}

/// Validate EPA spec
fn validate_epa_spec(epa: &ExternalPodAutoscaler) -> Result<(), String> {
    // Validate replica counts
    if epa.spec.min_replicas < 1 {
        return Err("minReplicas must be at least 1".to_string());
    }

    if epa.spec.max_replicas < epa.spec.min_replicas {
        return Err(format!(
            "maxReplicas ({}) must be greater than or equal to minReplicas ({})",
            epa.spec.max_replicas, epa.spec.min_replicas
        ));
    }

    // Validate scaleTargetRef
    if epa.spec.scale_target_ref.name.is_empty() {
        return Err("scaleTargetRef.name must not be empty".to_string());
    }

    if epa.spec.scale_target_ref.kind.is_empty() {
        return Err("scaleTargetRef.kind must not be empty".to_string());
    }

    // Validate metrics
    if epa.spec.metrics.is_empty() {
        return Err("At least one metric must be specified".to_string());
    }

    for (i, metric) in epa.spec.metrics.iter().enumerate() {
        if metric.metric_name.is_empty() {
            return Err(format!("metrics[{}].metricName must not be empty", i));
        }

        if metric.target_value.is_empty() {
            return Err(format!("metrics[{}].targetValue must not be empty", i));
        }

        // Validate target value is a valid quantity
        if let Err(e) = metric.target_value.parse::<f64>() {
            return Err(format!(
                "metrics[{}].targetValue must be a valid number: {}",
                i, e
            ));
        }
    }

    // Validate scrape config
    if epa.spec.scrape.port < 1 || epa.spec.scrape.port > 65535 {
        return Err(format!(
            "scrape.port must be between 1 and 65535, got {}",
            epa.spec.scrape.port
        ));
    }

    if epa.spec.scrape.scheme != "http" && epa.spec.scrape.scheme != "https" {
        return Err(format!(
            "scrape.scheme must be 'http' or 'https', got '{}'",
            epa.spec.scrape.scheme
        ));
    }

    // Validate scrape path to prevent path traversal attacks
    validate_scrape_path(&epa.spec.scrape.path)?;

    // Validate duration strings
    validate_duration(&epa.spec.scrape.interval, "scrape.interval")?;
    validate_duration(&epa.spec.scrape.timeout, "scrape.timeout")?;
    validate_duration(
        &epa.spec.scrape.evaluation_period,
        "scrape.evaluationPeriod",
    )?;

    Ok(())
}

/// Validate scrape path to prevent path injection/traversal attacks
fn validate_scrape_path(path: &str) -> Result<(), String> {
    // Path must start with /
    if !path.starts_with('/') {
        return Err(format!("scrape.path must start with '/', got '{}'", path));
    }

    // Prevent path traversal with ..
    if path.contains("..") {
        return Err(format!(
            "scrape.path must not contain '..' (path traversal attempt): '{}'",
            path
        ));
    }

    // Prevent multiple consecutive slashes (path normalization bypass)
    if path.contains("//") {
        return Err(format!(
            "scrape.path must not contain consecutive slashes: '{}'",
            path
        ));
    }

    // Prevent null bytes
    if path.contains('\0') {
        return Err("scrape.path must not contain null bytes".to_string());
    }

    // Path should be reasonable length (prevent DoS via extremely long paths)
    if path.len() > 256 {
        return Err(format!(
            "scrape.path too long (max 256 characters), got {} characters",
            path.len()
        ));
    }

    Ok(())
}

/// Validate duration string format
fn validate_duration(duration: &str, field: &str) -> Result<(), String> {
    let duration = duration.trim();
    if duration.is_empty() {
        return Err(format!("{} must not be empty", field));
    }

    let (num_str, unit) = if let Some(stripped) = duration.strip_suffix("ms") {
        (stripped, "ms")
    } else if duration.len() > 1 {
        (
            &duration[..duration.len() - 1],
            &duration[duration.len() - 1..],
        )
    } else {
        return Err(format!("{} has invalid format: {}", field, duration));
    };

    if num_str.parse::<u64>().is_err() {
        return Err(format!("{} has invalid number: {}", field, duration));
    }

    if !matches!(unit, "ms" | "s" | "m" | "h") {
        return Err(format!(
            "{} has invalid unit (must be ms, s, m, or h): {}",
            field, duration
        ));
    }

    Ok(())
}

/// Apply default values to EPA (modifies in place)
fn apply_defaults(epa: &mut ExternalPodAutoscaler) {
    use crate::apis::ctx_sh::v1beta1::TargetRef;

    // Default minReplicas to 1 if 0
    if epa.spec.min_replicas == 0 {
        epa.spec.min_replicas = 1;
    }

    // Set scrapeTargetRef to scaleTargetRef if not specified
    if epa.spec.scrape_target_ref.is_none() {
        epa.spec.scrape_target_ref = Some(TargetRef {
            api_version: epa.spec.scale_target_ref.api_version.clone(),
            kind: epa.spec.scale_target_ref.kind.clone(),
            name: epa.spec.scale_target_ref.name.clone(),
        });
    }

    // Set scaleTargetRef.apiVersion default if empty
    if epa.spec.scale_target_ref.api_version.is_empty() {
        epa.spec.scale_target_ref.api_version = "apps/v1".to_string();
    }

    // Set scrape config defaults
    if epa.spec.scrape.port == 0 {
        epa.spec.scrape.port = 8080;
    }

    if epa.spec.scrape.path.is_empty() {
        epa.spec.scrape.path = "/metrics".to_string();
    }

    if epa.spec.scrape.interval.is_empty() {
        epa.spec.scrape.interval = "15s".to_string();
    }

    if epa.spec.scrape.timeout.is_empty() {
        epa.spec.scrape.timeout = "1s".to_string();
    }

    if epa.spec.scrape.scheme.is_empty() {
        epa.spec.scrape.scheme = "http".to_string();
    }

    if epa.spec.scrape.evaluation_period.is_empty() {
        epa.spec.scrape.evaluation_period = "60s".to_string();
    }

    // Note: MetricTargetType and AggregationType already have #[default] attributes
    // so serde will handle their defaults during deserialization
}

/// Generate JSON patch from original to modified object
fn generate_patch(original: &Value, modified: &Value) -> Option<String> {
    // Use json_patch to diff the two objects
    let patch = json_patch::diff(original, modified);

    // If patch is empty, no changes needed
    if patch.0.is_empty() {
        return None;
    }

    // Serialize and encode as base64
    let patch_json = serde_json::to_string(&patch).ok()?;
    Some(base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        patch_json.as_bytes(),
    ))
}

/// Admission error type
#[derive(Debug)]
pub enum AdmissionError {
    BadRequest(String),
    InternalError(String),
}

impl IntoResponse for AdmissionError {
    fn into_response(self) -> Response {
        match self {
            AdmissionError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
            AdmissionError::InternalError(msg) => {
                (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
            }
        }
    }
}
