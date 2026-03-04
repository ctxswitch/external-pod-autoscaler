use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// External metric value (Kubernetes format)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalMetricValue {
    #[serde(rename = "metricName")]
    pub metric_name: String,

    #[serde(rename = "metricLabels")]
    pub metric_labels: BTreeMap<String, String>,

    pub value: String,

    pub timestamp: k8s_openapi::apimachinery::pkg::apis::meta::v1::Time,
}

/// External metric value list (Kubernetes format)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalMetricValueList {
    #[serde(rename = "apiVersion")]
    pub api_version: String,

    pub kind: String,

    pub items: Vec<ExternalMetricValue>,
}

impl ExternalMetricValueList {
    /// Creates a new list with the given metric values.
    pub fn new(items: Vec<ExternalMetricValue>) -> Self {
        Self {
            api_version: "external.metrics.k8s.io/v1beta1".to_string(),
            kind: "ExternalMetricValueList".to_string(),
            items,
        }
    }

    /// Creates an empty list with no metric values.
    pub fn empty() -> Self {
        Self::new(Vec::new())
    }
}
