use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// ExternalPodAutoscaler is a custom resource for managing HPAs with Prometheus-based external metrics
#[derive(CustomResource, Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "ctx.sh",
    version = "v1beta1",
    kind = "ExternalPodAutoscaler",
    namespaced,
    status = "ExternalPodAutoscalerStatus",
    shortname = "epa",
    printcolumn = r#"{"name":"Scale Target", "type":"string", "jsonPath":".spec.scaleTargetRef.name"}"#,
    printcolumn = r#"{"name":"Min", "type":"integer", "jsonPath":".spec.minReplicas"}"#,
    printcolumn = r#"{"name":"Max", "type":"integer", "jsonPath":".spec.maxReplicas"}"#,
    printcolumn = r#"{"name":"Scraped", "type":"integer", "jsonPath":".status.scrapedReplicas"}"#,
    printcolumn = r#"{"name":"Errors", "type":"integer", "jsonPath":".status.scrapeErrors"}"#,
    printcolumn = r#"{"name":"Age", "type":"date", "jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPodAutoscalerSpec {
    /// Minimum number of replicas
    #[serde(default = "default_min_replicas")]
    pub min_replicas: i32,

    /// Maximum number of replicas
    pub max_replicas: i32,

    /// Scrape configuration
    pub scrape: ScrapeConfig,

    /// Reference to the resource to scrape metrics from (optional, defaults to scaleTargetRef)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scrape_target_ref: Option<TargetRef>,

    /// Reference to the resource to scale
    pub scale_target_ref: TargetRef,

    /// Metrics to scrape and use for scaling
    pub metrics: Vec<MetricSpec>,

    /// HPA behavior configuration (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behavior: Option<ScalingBehavior>,
}

/// Scrape configuration
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScrapeConfig {
    /// Port to scrape metrics from
    #[serde(default = "default_port")]
    pub port: i32,

    /// HTTP path to scrape metrics from
    #[serde(default = "default_path")]
    pub path: String,

    /// Scrape interval (e.g., "15s", "1m")
    #[serde(default = "default_interval")]
    pub interval: String,

    /// Scrape timeout per target (e.g., "1s")
    #[serde(default = "default_timeout")]
    pub timeout: String,

    /// Scheme (http or https)
    #[serde(default = "default_scheme")]
    pub scheme: String,

    /// TLS configuration (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls: Option<TlsConfig>,

    /// Evaluation period for metric aggregation (e.g., "5m")
    #[serde(default = "default_evaluation_period")]
    pub evaluation_period: String,

    /// Default aggregation type for metrics
    #[serde(default)]
    pub aggregation_type: AggregationType,
}

/// TLS configuration for HTTPS scraping
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TlsConfig {
    /// Skip TLS certificate verification
    #[serde(default)]
    pub insecure_skip_verify: bool,
}

/// Reference to a Kubernetes resource
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TargetRef {
    /// API version of the referent
    #[serde(default = "default_api_version")]
    pub api_version: String,

    /// Kind of the referent (e.g., Deployment, StatefulSet)
    pub kind: String,

    /// Name of the referent
    pub name: String,
}

/// Metric specification
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MetricSpec {
    /// Prometheus metric name to scrape
    /// Will be exposed as external metric: {crd-name}-{crd-namespace}-{metric-name}
    pub metric_name: String,

    /// HPA metric target type
    #[serde(rename = "type", default)]
    pub type_: MetricTargetType,

    /// Target value for scaling
    pub target_value: String,

    /// Aggregation type override (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aggregation_type: Option<AggregationType>,

    /// Evaluation period override (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluation_period: Option<String>,

    /// Label selector for filtering metric samples (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_selector: Option<LabelSelector>,
}

/// HPA metric target type
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
pub enum MetricTargetType {
    /// Divide metric value by current replica count
    #[default]
    AverageValue,
    /// Use metric value directly
    Value,
}

/// Aggregation type for per-pod metric values over evaluation window
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum AggregationType {
    #[default]
    Avg,
    Max,
    Min,
    Median,
    Last,
}

/// Label selector for filtering metric samples
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabelSelector {
    /// Simple key-value label matches (AND'd together)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_labels: Option<BTreeMap<String, String>>,

    /// Expression-based label requirements (AND'd together)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_expressions: Option<Vec<LabelSelectorRequirement>>,
}

/// Label selector requirement
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabelSelectorRequirement {
    /// Label key
    pub key: String,

    /// Operator (In, NotIn, Exists, DoesNotExist)
    pub operator: LabelSelectorOperator,

    /// Values (required for In/NotIn)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
}

/// Label selector operator
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq)]
pub enum LabelSelectorOperator {
    In,
    NotIn,
    Exists,
    DoesNotExist,
}

/// HPA scaling behavior configuration
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScalingBehavior {
    /// Scale up behavior
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale_up: Option<ScalingRules>,

    /// Scale down behavior
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale_down: Option<ScalingRules>,
}

/// Scaling rules
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScalingRules {
    /// Stabilization window in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stabilization_window_seconds: Option<i32>,

    /// Scaling policies
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policies: Option<Vec<ScalingPolicy>>,

    /// Policy selection (Min, Max, Disabled)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub select_policy: Option<String>,
}

/// Scaling policy
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScalingPolicy {
    /// Type (Pods or Percent)
    #[serde(rename = "type")]
    pub type_: String,

    /// Value
    pub value: i32,

    /// Period in seconds
    pub period_seconds: i32,
}

/// Status of the ExternalPodAutoscaler resource
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPodAutoscalerStatus {
    /// Managed HPA status
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed_hpa: Option<ManagedHpaStatus>,

    /// Scraped replica count
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scraped_replicas: Option<i32>,

    /// Scrape error count
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scrape_errors: Option<i32>,

    /// Conditions
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
}

/// Managed HPA status
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ManagedHpaStatus {
    /// Name of the managed HPA
    pub name: String,

    /// Last sync time
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sync_time: Option<String>,
}

/// Condition
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    /// Condition type
    #[serde(rename = "type")]
    pub type_: String,

    /// Condition status
    pub status: String,

    /// Last transition time
    pub last_transition_time: String,

    /// Reason
    pub reason: String,

    /// Message
    pub message: String,
}

// Default functions
fn default_min_replicas() -> i32 {
    1
}

fn default_port() -> i32 {
    8080
}

fn default_path() -> String {
    "/metrics".to_string()
}

fn default_interval() -> String {
    "15s".to_string()
}

fn default_timeout() -> String {
    "1s".to_string()
}

fn default_scheme() -> String {
    "http".to_string()
}

fn default_evaluation_period() -> String {
    "60s".to_string()
}

fn default_api_version() -> String {
    "apps/v1".to_string()
}
