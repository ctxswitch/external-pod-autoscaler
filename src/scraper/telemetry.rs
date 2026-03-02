use prometheus::{
    opts, register_histogram_vec, register_int_counter_vec, HistogramVec, IntCounterVec,
};
use std::sync::OnceLock;

/// Prometheus metrics for the Scraper service
pub struct Telemetry {
    /// Scrape duration in seconds
    pub scrape_duration: HistogramVec,
    /// Scrape errors counter
    pub scrape_errors: IntCounterVec,
    /// Number of pods scraped per EPA
    pub pods_scraped: IntCounterVec,
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
            let scrape_duration = register_histogram_vec!(
                "epa_scrape_duration_seconds",
                "Time spent scraping metrics from pods",
                &["epa", "namespace"]
            )
            .expect("Failed to register scrape_duration metric");

            let scrape_errors = register_int_counter_vec!(
                opts!("epa_scrape_errors_total", "Total number of scrape errors"),
                &["epa", "namespace", "error_type"]
            )
            .expect("Failed to register scrape_errors metric");

            let pods_scraped = register_int_counter_vec!(
                opts!(
                    "epa_pods_scraped_total",
                    "Total number of pods scraped per EPA"
                ),
                &["epa", "namespace"]
            )
            .expect("Failed to register pods_scraped metric");

            Telemetry {
                scrape_duration,
                scrape_errors,
                pods_scraped,
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
