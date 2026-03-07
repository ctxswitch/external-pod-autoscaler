use crate::store::MetricType;
use anyhow::{Result, anyhow};
use std::collections::BTreeMap;

/// Parsed Prometheus metric
#[derive(Debug, Clone)]
pub struct PrometheusMetric {
    pub name: String,
    pub value: f64,
    pub labels: BTreeMap<String, String>,
    pub metric_type: MetricType,
}

/// Parse Prometheus text format
/// Example input:
/// ```text
/// # TYPE http_requests_total counter
/// http_requests_total{method="GET",endpoint="/api"} 123
/// http_requests_total{method="POST",endpoint="/api"} 45
/// # TYPE memory_usage_bytes gauge
/// memory_usage_bytes 1048576
/// ```
pub fn parse_prometheus_text(text: &str) -> Result<Vec<PrometheusMetric>> {
    let mut metrics = Vec::new();
    let mut current_type: Option<(String, MetricType)> = None;

    for line in text.lines() {
        let line = line.trim();

        // Skip empty lines
        if line.is_empty() {
            continue;
        }

        // Parse TYPE comments
        if line.starts_with("# TYPE ") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                let metric_name = parts[2];
                let type_str = parts[3];
                let metric_type = match type_str {
                    "counter" => MetricType::Counter,
                    "gauge" => MetricType::Gauge,
                    _ => MetricType::Gauge, // Default to gauge for unknown types
                };
                current_type = Some((metric_name.to_string(), metric_type));
            }
            continue;
        }

        // Skip other comments
        if line.starts_with('#') {
            continue;
        }

        // Parse metric line
        if let Some(metric) = parse_metric_line(line, &current_type)? {
            metrics.push(metric);
        }
    }

    Ok(metrics)
}

/// Parse a single metric line
/// Examples:
/// - `http_requests_total{method="GET",endpoint="/api"} 123`
/// - `memory_usage_bytes 1048576`
fn parse_metric_line(
    line: &str,
    current_type: &Option<(String, MetricType)>,
) -> Result<Option<PrometheusMetric>> {
    // Split on whitespace to separate metric name+labels from value
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 2 {
        return Ok(None); // Not a valid metric line
    }

    let metric_part = parts[0];
    let value_str = parts[1];

    // Parse value
    let value = value_str
        .parse::<f64>()
        .map_err(|_| anyhow!("Invalid metric value: {}", value_str))?;

    // Parse metric name and labels
    let (name, labels) = if metric_part.contains('{') {
        // Has labels: http_requests_total{method="GET",endpoint="/api"}
        let open_brace = metric_part
            .find('{')
            .ok_or_else(|| anyhow!("Invalid metric format"))?;
        let close_brace = metric_part
            .rfind('}')
            .ok_or_else(|| anyhow!("Invalid metric format"))?;

        let name = &metric_part[..open_brace];
        let labels_str = &metric_part[open_brace + 1..close_brace];

        let labels = parse_labels(labels_str)?;

        (name.to_string(), labels)
    } else {
        // No labels: memory_usage_bytes
        (metric_part.to_string(), BTreeMap::new())
    };

    // Determine metric type
    let metric_type = current_type
        .as_ref()
        .filter(|(type_name, _)| type_name == &name)
        .map(|(_, mt)| *mt)
        .unwrap_or(MetricType::Gauge); // Default to gauge if no type info

    Ok(Some(PrometheusMetric {
        name,
        value,
        labels,
        metric_type,
    }))
}

/// Parse Prometheus labels
/// Example: `method="GET",endpoint="/api"`
pub(crate) fn parse_labels(labels_str: &str) -> Result<BTreeMap<String, String>> {
    let mut labels = BTreeMap::new();

    if labels_str.is_empty() {
        return Ok(labels);
    }

    for pair in labels_str.split(',') {
        let pair = pair.trim();
        if let Some((key, value)) = pair.split_once('=') {
            let key = key.trim().to_string();
            // Remove quotes from value
            let value = value.trim().trim_matches('"').to_string();
            labels.insert(key, value);
        }
    }

    Ok(labels)
}
