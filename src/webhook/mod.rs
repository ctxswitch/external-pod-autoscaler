mod admission;
mod aggregation;
mod handlers;
mod server;
mod telemetry;
mod types;

#[cfg(test)]
mod admission_test;
#[cfg(test)]
mod aggregation_test;
#[cfg(test)]
mod handlers_test;

pub use server::WebhookServer;
// Exported for the e2e integration-test binary; unused in the main binary
// target, which causes a false-positive unused_imports warning.
#[allow(unused_imports)]
pub use types::ExternalMetricValueList;
