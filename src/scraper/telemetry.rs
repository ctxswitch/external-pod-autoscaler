use prometheus::{
    HistogramVec, IntCounterVec, opts, register_histogram_vec, register_int_counter_vec,
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
    /// Histogram of channel send latency per EPA, labeled `[epa, namespace]`.
    pub enqueue_wait: HistogramVec,
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

            let enqueue_wait = register_histogram_vec!(
                "epa_scrape_enqueue_wait_seconds",
                "Time blocked waiting to enqueue a scrape job (non-zero only under backpressure)",
                &["epa", "namespace"],
                vec![
                    0.000_1, 0.001, 0.005, 0.010, 0.025, 0.050, 0.100, 0.250, 0.500, 1.0
                ]
            )
            .expect("Failed to register enqueue_wait metric");

            Telemetry {
                scrape_duration,
                scrape_errors,
                pods_scraped,
                enqueue_wait,
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
