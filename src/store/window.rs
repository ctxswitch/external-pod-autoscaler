use super::types::LabeledSample;
use std::collections::VecDeque;
use std::time::Duration;
use std::time::Instant;

/// Sliding window of samples for a single pod/metric combination.
///
/// Maintains a fixed-size FIFO buffer of samples. Old samples are evicted when the
/// buffer is full. Samples can be filtered by evaluation period for aggregation.
#[derive(Debug, Clone)]
pub struct MetricWindow {
    /// FIFO buffer of samples
    pub samples: VecDeque<LabeledSample>,
    /// Maximum number of samples to retain
    pub max_samples: usize,
}

impl MetricWindow {
    /// Creates a new metric window with the specified capacity.
    pub fn new(max_samples: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(max_samples),
            max_samples,
        }
    }

    /// Pushes a new sample into the window, evicting the oldest if at capacity.
    pub fn push(&mut self, sample: LabeledSample) {
        if self.samples.len() >= self.max_samples {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    /// Returns the age of the most recent sample, or `None` if the window is empty.
    ///
    /// Age is computed as the elapsed time since the newest sample's `scraped_at`.
    pub fn newest_sample_age(&self) -> Option<Duration> {
        self.samples.back().map(|s| s.scraped_at.elapsed())
    }

    /// Returns samples within the specified evaluation period from now.
    ///
    /// Filters out samples older than `period` based on their `scraped_at` timestamp.
    pub fn get_samples_in_period(&self, period: Duration) -> Vec<&LabeledSample> {
        let now = Instant::now();
        self.samples
            .iter()
            .filter(|s| now.duration_since(s.scraped_at) <= period)
            .collect()
    }
}
