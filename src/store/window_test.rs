use super::types::MetricType;
use super::window::MetricWindow;
use crate::store::types::LabeledSample;
use std::time::Instant;

#[test]
fn test_metric_window_push() {
    let mut window = MetricWindow::new(3);
    assert_eq!(window.samples.len(), 0);

    let sample = LabeledSample {
        value: 100.0,
        scraped_at: Instant::now(),
        success: true,
        metric_type: MetricType::Gauge,
    };

    window.push(sample.clone());
    assert_eq!(window.samples.len(), 1);

    window.push(sample.clone());
    window.push(sample.clone());
    assert_eq!(window.samples.len(), 3);

    // Fourth push should evict first
    window.push(sample.clone());
    assert_eq!(window.samples.len(), 3);
}
