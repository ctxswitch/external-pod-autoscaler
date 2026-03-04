mod admission;
mod metrics;
mod server;

pub use server::WebhookServer;
// Exported for the e2e integration-test binary; unused in the main binary
// target, which causes a false-positive unused_imports warning.
#[allow(unused_imports)]
pub use metrics::ExternalMetricValueList;
