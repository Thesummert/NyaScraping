//! Bounded sample storage, independent of acquisition and rendering.
use crate::{Error, Result};
use std::{collections::VecDeque, num::NonZeroUsize, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// Monotonic time relative to the start of acquisition.
    pub elapsed: Duration,
    pub value: f64,
}

pub struct SampleBuffer {
    capacity: NonZeroUsize,
    samples: VecDeque<Sample>,
}

impl SampleBuffer {
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            capacity,
            samples: VecDeque::new(),
        }
    }

    pub fn push(&mut self, sample: Sample) -> Result<()> {
        if !sample.value.is_finite() {
            return Err(Error("sample must be finite".into()));
        }
        if self
            .samples
            .back()
            .is_some_and(|last| sample.elapsed < last.elapsed)
        {
            return Err(Error("sample timestamp moved backwards".into()));
        }
        if self.samples.len() == self.capacity.get() {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
        Ok(())
    }

    pub fn samples(&self) -> impl Iterator<Item = &Sample> {
        self.samples.iter()
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_latest_samples_and_rejects_invalid_data_without_evicting() {
        let mut buffer = SampleBuffer::new(NonZeroUsize::new(2).unwrap());
        for n in 0..3 {
            buffer
                .push(Sample {
                    elapsed: Duration::from_secs(n),
                    value: n as f64,
                })
                .unwrap();
        }
        assert_eq!(
            buffer.samples().map(|s| s.value).collect::<Vec<_>>(),
            [1.0, 2.0]
        );
        assert!(buffer
            .push(Sample {
                elapsed: Duration::ZERO,
                value: 0.0
            })
            .is_err());
        assert!(buffer
            .push(Sample {
                elapsed: Duration::from_secs(3),
                value: f64::NAN
            })
            .is_err());
        assert_eq!(buffer.samples().count(), 2);
        buffer.clear();
        assert_eq!(buffer.samples().count(), 0);
    }
}
