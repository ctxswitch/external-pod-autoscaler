use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq)]
pub enum ConditionStatus {
    True,
    False,
    Unknown,
}

/// Metric is a custom resource for collecting and aggregating metrics from pods
#[derive(CustomResource, Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "ctx.sh",
    version = "v1beta1",
    kind = "Metric",
    namespaced,
    status = "MetricStatus",
    shortname = "metric",
    printcolumn = r#"{"name":"Targets", "type":"integer", "jsonPath":".status.totalTargets"}"#,
    printcolumn = r#"{"name":"Scraped", "type":"integer", "jsonPath":".status.scrapedTargets"}"#,
    printcolumn = r#"{"name":"Last Scrape", "type":"string", "jsonPath":".status.lastScrapeTime"}"#,
    printcolumn = r#"{"name":"Age", "type":"date", "jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct MetricSpec {
    /// Selector for finding scrape targets
    pub selector: Selector,

    // Target configuration
    pub target: MetricTarget,
}

/// Metric target configuration
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MetricTarget {
    /// Port to scrape metrics from
    #[serde(default = "default_port")]
    #[schemars(default = "default_port")]
    pub port: i32,

    /// HTTP path to scrape metrics from (e.g., "/metrics")
    #[serde(default = "default_path")]
    #[schemars(default = "default_path")]
    pub path: String,

    /// Scrape interval in seconds
    #[serde(default = "default_scrape_interval_seconds")]
    #[schemars(default = "default_scrape_interval_seconds")]
    pub interval_seconds: i32,

    /// Scrape timeout in seconds
    #[serde(default = "default_scrape_timeout_seconds")]
    #[schemars(default = "default_scrape_timeout_seconds")]
    pub timeout_seconds: i32,

    /// Prometheus metric name to scrape
    pub metric_name: String,

    /// Type of metric value (gauge or counter)
    #[serde(default)]
    #[schemars(default = "MetricValueType::default")]
    pub value_type: MetricValueType,

    /// Window duration
    #[serde(default = "default_window_duration_seconds")]
    #[schemars(default = "default_window_duration_seconds")]
    pub window_duration_seconds: i32,

    /// Aggregation to compute - defaults to Avg for Gauge, Rate for Counter
    pub aggregation: AggregationType,
}

impl<'de> Deserialize<'de> for MetricTarget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct MetricTargetHelper {
            #[serde(default = "default_port")]
            port: i32,
            #[serde(default = "default_path")]
            path: String,
            #[serde(default = "default_scrape_interval_seconds")]
            interval_seconds: i32,
            #[serde(default = "default_scrape_timeout_seconds")]
            timeout_seconds: i32,
            metric_name: String,
            #[serde(default)]
            value_type: MetricValueType,
            #[serde(default = "default_window_duration_seconds")]
            window_duration_seconds: i32,
            aggregation: Option<AggregationType>,
        }

        let helper = MetricTargetHelper::deserialize(deserializer)?;

        // Apply conditional default for aggregation based on value_type
        let aggregation = helper.aggregation.unwrap_or_else(|| {
            match helper.value_type {
                MetricValueType::Gauge => AggregationType::Avg,
                MetricValueType::Counter => AggregationType::Rate,
            }
        });

        Ok(MetricTarget {
            port: helper.port,
            path: helper.path,
            interval_seconds: helper.interval_seconds,
            timeout_seconds: helper.timeout_seconds,
            metric_name: helper.metric_name,
            value_type: helper.value_type,
            window_duration_seconds: helper.window_duration_seconds,
            aggregation,
        })
    }
}

/// Type of metric value
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum MetricValueType {
    #[default]
    Gauge,
    Counter,
}

/// Aggregation type for metrics
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AggregationType {
    Avg,
    Sum,
    Min,
    Max,
    Rate,
}

/// Selector for finding scrape targets
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Selector {
    /// Simple key-value label matches (AND'd together)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_labels: Option<BTreeMap<String, String>>,

    /// Expression-based label requirements (AND'd together)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_expressions: Option<Vec<SelectorRequirement>>,
}

/// Selector requirement for match expressions
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SelectorRequirement {
    /// Label key to match against
    pub key: String,

    /// Operator for matching (In, NotIn, Exists, DoesNotExist)
    pub operator: SelectorOperator,

    /// Values to match (required for In/NotIn, ignored for Exists/DoesNotExist)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
}

/// Selector operator
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq)]
pub enum SelectorOperator {
    /// Key must equal one of the values
    In,
    /// Key must not equal any of the values
    NotIn,
    /// Key must exist (value doesn't matter)
    Exists,
    /// Key must not exist
    DoesNotExist,
}

/// Status of the Metric resource
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct MetricStatus {
    /// Total number of scrape targets (pods)
    #[serde(default)]
    pub total_targets: i32,

    /// Number of successfully scraped targets
    #[serde(default)]
    pub scraped_targets: i32,

    /// Last successful scrape timestamp
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_scrape_time: Option<String>,
}



fn default_port() -> i32 { 8080 }

fn default_path() -> String { "/metrics".to_string() }

fn default_scrape_interval_seconds() -> i32 {
    30 // Scrape every 30 seconds by default
}

fn default_scrape_timeout_seconds() -> i32 {
    10 // 10 second timeout for scrapes
}

fn default_window_duration_seconds() -> i32 {
    120 // 120 second window by default.
}
