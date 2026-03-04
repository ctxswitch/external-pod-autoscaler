use super::types::MetricType;
use super::window::MetricWindow;
use crate::store::types::LabeledSample;
use std::time::Duration;
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

#[test]
fn is_window_sufficient_empty() {
    let window = MetricWindow::new(10);
    assert!(!window.is_window_sufficient(Duration::from_secs(60), 1));
}

#[test]
fn is_window_sufficient_below_threshold() {
    let mut window = MetricWindow::new(10);
    let sample = LabeledSample {
        value: 1.0,
        scraped_at: Instant::now(),
        success: true,
        metric_type: MetricType::Gauge,
    };

    window.push(sample.clone());
    window.push(sample);

    assert!(!window.is_window_sufficient(Duration::from_secs(60), 3));
}

#[test]
fn is_window_sufficient_at_threshold() {
    let mut window = MetricWindow::new(10);
    let sample = LabeledSample {
        value: 1.0,
        scraped_at: Instant::now(),
        success: true,
        metric_type: MetricType::Gauge,
    };

    window.push(sample.clone());
    window.push(sample.clone());
    window.push(sample);

    assert!(window.is_window_sufficient(Duration::from_secs(60), 3));
}

#[test]
fn is_window_sufficient_expired_samples() {
    let now = Instant::now();
    let mut window = MetricWindow::new(10);
    let expired_sample = LabeledSample {
        value: 1.0,
        scraped_at: now - Duration::from_secs(120),
        success: true,
        metric_type: MetricType::Gauge,
    };

    window.push(expired_sample.clone());
    window.push(expired_sample.clone());
    window.push(expired_sample);

    assert!(!window.is_window_sufficient(Duration::from_secs(60), 1));
}

#[test]
fn is_window_sufficient_mixed_samples() {
    let now = Instant::now();
    let mut window = MetricWindow::new(10);
    let expired = LabeledSample {
        value: 1.0,
        scraped_at: now - Duration::from_secs(120),
        success: true,
        metric_type: MetricType::Gauge,
    };
    let fresh = LabeledSample {
        value: 2.0,
        scraped_at: now,
        success: true,
        metric_type: MetricType::Gauge,
    };

    window.push(expired.clone());
    window.push(expired);
    window.push(fresh);

    assert!(!window.is_window_sufficient(Duration::from_secs(60), 2));
    assert!(window.is_window_sufficient(Duration::from_secs(60), 1));
}
