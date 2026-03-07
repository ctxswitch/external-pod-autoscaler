use prometheus::{
    HistogramVec, IntCounterVec, opts, register_histogram_vec, register_int_counter_vec,
};
use std::sync::OnceLock;

/// Prometheus metrics for the Webhook service
pub struct Telemetry {
    /// API requests to the external metrics endpoint
    pub api_requests: IntCounterVec,
    /// API request duration in seconds
    pub api_request_duration: HistogramVec,
    /// Cache hits vs misses
    pub cache_hits: IntCounterVec,
}

static METRICS: OnceLock<Telemetry> = OnceLock::new();

impl Telemetry {
    /// Initialize the metrics registry.
    ///
    /// # Panics
    ///
    /// Panics if Prometheus metric registration fails. This runs once via
    /// `OnceLock::get_or_init` at startup; registration failure is unrecoverable.
    pub fn init() -> &'static Telemetry {
        METRICS.get_or_init(|| {
            let api_requests = register_int_counter_vec!(
                opts!(
                    "epa_api_requests_total",
                    "Total number of API requests to external metrics endpoint"
                ),
                &["namespace", "metric", "status"]
            )
            .expect("Failed to register api_requests metric");

            let api_request_duration = register_histogram_vec!(
                "epa_api_request_duration_seconds",
                "Duration of API requests to external metrics endpoint",
                &["namespace", "metric"]
            )
            .expect("Failed to register api_request_duration metric");

            let cache_hits = register_int_counter_vec!(
                opts!(
                    "epa_cache_operations_total",
                    "Cache hits and misses for aggregated metrics"
                ),
                &["namespace", "metric", "result"]
            )
            .expect("Failed to register cache_hits metric");

            Telemetry {
                api_requests,
                api_request_duration,
                cache_hits,
            }
        })
    }

    /// Get the global metrics instance, initializing if needed.
    ///
    /// # Panics
    ///
    /// Panics if Prometheus metric registration fails on first call.
    /// See [`Telemetry::init`] for details.
    pub fn global() -> &'static Telemetry {
        Self::init()
    }
}
