use prometheus::{
    HistogramVec, IntCounterVec, opts, register_histogram_vec, register_int_counter_vec,
};
use std::sync::OnceLock;

/// Prometheus metrics for the ExternalPodAutoscaler controller
pub struct Telemetry {
    /// Reconciliation duration in seconds
    pub reconcile_duration: HistogramVec,
    /// Reconciliation errors counter
    pub reconcile_errors: IntCounterVec,
    /// HPA operations counter (create, update, delete)
    pub hpa_operations: IntCounterVec,
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
            let reconcile_duration = register_histogram_vec!(
                "epa_reconcile_duration_seconds",
                "Time spent reconciling ExternalPodAutoscaler resources",
                &["epa", "namespace"]
            )
            .expect("Failed to register reconcile_duration metric");

            let reconcile_errors = register_int_counter_vec!(
                opts!(
                    "epa_reconcile_errors_total",
                    "Total number of reconciliation errors"
                ),
                &["epa", "namespace", "error_type"]
            )
            .expect("Failed to register reconcile_errors metric");

            let hpa_operations = register_int_counter_vec!(
                opts!(
                    "epa_hpa_operations_total",
                    "Total number of HPA operations (create/update/delete)"
                ),
                &["epa", "namespace", "operation"]
            )
            .expect("Failed to register hpa_operations metric");

            Telemetry {
                reconcile_duration,
                reconcile_errors,
                hpa_operations,
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
