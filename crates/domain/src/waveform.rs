//! Bounded sample storage, independent of acquisition and rendering.
use crate::{Error, Result};
use std::{collections::VecDeque, num::NonZeroUsize, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalarType {
    Signed,
    Unsigned,
    Float,
    Boolean,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryChannel {
    pub address: u64,
    pub byte_size: usize,
    pub scalar_type: ScalarType,
    pub little_endian: bool,
}

impl MemoryChannel {
    pub fn validate(&self) -> Result<()> {
        let valid_size = match self.scalar_type {
            ScalarType::Float => matches!(self.byte_size, 4 | 8),
            _ => matches!(self.byte_size, 1 | 2 | 4 | 8),
        };
        if !valid_size || self.address.checked_add(self.byte_size as u64).is_none() {
            return Err(Error("不支持的标量大小或地址溢出".into()));
        }
        Ok(())
    }

    pub fn decode(&self, bytes: &[u8]) -> Result<f64> {
        self.validate()?;
        if bytes.len() != self.byte_size {
            return Err(Error("内存读取长度与变量大小不符".into()));
        }
        let mut raw = [0u8; 8];
        if self.little_endian {
            raw[..bytes.len()].copy_from_slice(bytes);
        } else {
            for (out, input) in raw.iter_mut().zip(bytes.iter().rev()) {
                *out = *input;
            }
        }
        let bits = u64::from_le_bytes(raw);
        Ok(match self.scalar_type {
            ScalarType::Float if self.byte_size == 4 => f32::from_bits(bits as u32) as f64,
            ScalarType::Float => f64::from_bits(bits),
            ScalarType::Signed => {
                let shift = 64 - self.byte_size * 8;
                ((bits << shift) as i64 >> shift) as f64
            }
            ScalarType::Unsigned => bits as f64,
            ScalarType::Boolean => (bits != 0) as u8 as f64,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadBlock {
    pub address: u64,
    pub byte_size: usize,
    pub little_endian: bool,
}

impl ReadBlock {
    /// Largest naturally aligned transfer up to 32 bits; never overread a variable.
    pub fn width_bytes(&self) -> usize {
        [4, 2, 1]
            .into_iter()
            .find(|width| {
                self.address.is_multiple_of(*width as u64) && self.byte_size.is_multiple_of(*width)
            })
            .unwrap()
    }
}

pub struct ReadPlan {
    pub blocks: Vec<ReadBlock>,
    /// (block index, byte offset), in original channel order.
    pub locations: Vec<(usize, usize)>,
}

impl ReadPlan {
    pub fn new(channels: &[MemoryChannel]) -> Result<Self> {
        for channel in channels {
            channel.validate()?;
        }
        let mut order: Vec<_> = channels.iter().enumerate().collect();
        order.sort_by_key(|(_, channel)| channel.address);
        let mut plan = Self {
            blocks: Vec::new(),
            locations: vec![(0, 0); channels.len()],
        };
        for (index, channel) in order {
            let end = channel.address + channel.byte_size as u64;
            let merge = plan.blocks.last().is_some_and(|block| {
                let block_end = block.address + block.byte_size as u64;
                channel.little_endian == block.little_endian
                    && channel.address <= block_end
                    && end - block.address <= 1024
            });
            if merge {
                let block = plan.blocks.last_mut().unwrap();
                block.byte_size = block.byte_size.max((end - block.address) as usize);
            } else {
                plan.blocks.push(ReadBlock {
                    address: channel.address,
                    byte_size: channel.byte_size,
                    little_endian: channel.little_endian,
                });
            }
            let block = plan.blocks.last().unwrap();
            plan.locations[index] = (
                plan.blocks.len() - 1,
                (channel.address - block.address) as usize,
            );
        }
        Ok(plan)
    }
}

/// Implementations must read running-target memory without halt/resume or target code.
pub trait LiveMemoryReader {
    fn read_blocks(&mut self, blocks: &[ReadBlock]) -> Result<Vec<Vec<u8>>>;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// Monotonic time relative to the start of acquisition.
    pub elapsed: Duration,
    pub value: f64,
}

pub struct SampleBuffer {
    capacity: NonZeroUsize,
    samples: VecDeque<Sample>,
    evicted: u64,
}

impl SampleBuffer {
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            capacity,
            samples: VecDeque::with_capacity(capacity.get()),
            evicted: 0,
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
            self.evicted += 1;
        }
        self.samples.push_back(sample);
        Ok(())
    }

    pub fn samples(&self) -> impl Iterator<Item = &Sample> {
        self.samples.iter()
    }

    pub fn clear(&mut self) {
        self.samples.clear();
        self.evicted = 0;
    }

    pub fn latest(&self) -> Option<&Sample> {
        self.samples.back()
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn evicted(&self) -> u64 {
        self.evicted
    }

    /// Per-pixel min/max preserves narrow spikes with bounded rendering cost.
    pub fn envelope(
        &self,
        end: Duration,
        window: Duration,
        width: usize,
    ) -> Vec<Option<(f64, f64)>> {
        let mut bins: Vec<Option<(f64, f64)>> = vec![None; width];
        if width == 0 || window.is_zero() {
            return bins;
        }
        let start = end.as_secs_f64() - window.as_secs_f64();
        for sample in self.samples.iter().rev() {
            let t = sample.elapsed.as_secs_f64();
            if t < start {
                break;
            }
            if sample.elapsed > end {
                continue;
            }
            let x = (((t - start) / window.as_secs_f64() * width as f64) as usize).min(width - 1);
            bins[x] = Some(match bins[x] {
                Some((min, max)) => (min.min(sample.value), max.max(sample.value)),
                None => (sample.value, sample.value),
            });
        }
        bins
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_signed_float_and_big_endian_without_guessing_type_names() {
        let mut channel = MemoryChannel {
            address: 0x20000000,
            byte_size: 2,
            scalar_type: ScalarType::Signed,
            little_endian: true,
        };
        assert_eq!(channel.decode(&[0xfe, 0xff]).unwrap(), -2.0);
        channel.little_endian = false;
        assert_eq!(channel.decode(&[0xff, 0xfe]).unwrap(), -2.0);
        channel.scalar_type = ScalarType::Float;
        channel.byte_size = 4;
        assert_eq!(channel.decode(&1.5f32.to_be_bytes()).unwrap(), 1.5);
        assert!(channel.decode(&[0]).is_err());
        channel.byte_size = 16;
        assert!(channel.validate().is_err());
    }

    #[test]
    fn merges_only_selected_overlapping_or_adjacent_memory_and_preserves_order() {
        let channel = |address| MemoryChannel {
            address,
            byte_size: 4,
            scalar_type: ScalarType::Unsigned,
            little_endian: true,
        };
        let plan = ReadPlan::new(&[
            channel(0x20000004),
            channel(0x20000000),
            channel(0x20000004),
            channel(0x20000010),
        ])
        .unwrap();
        assert_eq!(plan.blocks.len(), 2);
        assert_eq!(plan.blocks[0].byte_size, 8);
        assert_eq!(plan.blocks[0].width_bytes(), 4);
        assert_eq!(plan.locations, [(0, 4), (0, 0), (0, 4), (1, 0)]);
        assert!(ReadPlan::new(&[channel(u64::MAX)]).is_err());
    }

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

    #[test]
    fn pixel_envelope_preserves_spikes_and_excludes_samples_outside_window() {
        let mut buffer = SampleBuffer::new(NonZeroUsize::new(10).unwrap());
        for (millis, value) in [
            (0, -999.0),
            (1000, 1.0),
            (1100, 500.0),
            (1200, -30.0),
            (1900, 2.0),
            (2000, 3.0),
            (3000, 999.0),
        ] {
            buffer
                .push(Sample {
                    elapsed: Duration::from_millis(millis),
                    value,
                })
                .unwrap();
        }
        assert_eq!(
            buffer.envelope(Duration::from_secs(2), Duration::from_secs(1), 2),
            [Some((-30.0, 500.0)), Some((2.0, 3.0))]
        );
        assert!(buffer
            .envelope(Duration::from_secs(2), Duration::from_secs(1), 0)
            .is_empty());
        assert_eq!(
            buffer.envelope(Duration::from_secs(2), Duration::ZERO, 1),
            [None]
        );
    }
}
