//! Runtime-side media contracts.
//!
//! Decoding and media timing belong to the runtime. Audio device output and
//! final presentation remain host-owned. This crate intentionally contains no
//! platform decoder, audio sink, or FFI bindings yet; those are supplied by
//! later backend implementations behind these contracts.

use std::collections::VecDeque;
use std::time::Duration;

#[cfg(feature = "ffmpeg")]
pub mod ffmpeg;

/// Monotonic media clock driven by the host's consumed audio position.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct MediaTime {
    micros: u64,
}

impl MediaTime {
    pub const ZERO: Self = Self { micros: 0 };

    pub const fn from_micros(micros: u64) -> Self {
        Self { micros }
    }

    pub const fn from_millis(millis: u64) -> Self {
        Self {
            micros: millis.saturating_mul(1_000),
        }
    }

    pub const fn micros(self) -> u64 {
        self.micros
    }

    pub const fn millis(self) -> u64 {
        self.micros / 1_000
    }

    pub const fn saturating_add(self, duration: Duration) -> Self {
        Self {
            micros: self.micros.saturating_add(duration.as_micros() as u64),
        }
    }

    pub const fn saturating_sub(self, duration: Duration) -> Self {
        Self {
            micros: self.micros.saturating_sub(duration.as_micros() as u64),
        }
    }

    pub const fn saturating_add_micros(self, micros: u64) -> Self {
        Self {
            micros: self.micros.saturating_add(micros),
        }
    }
}

/// Pixel layout produced by a runtime video decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoPixelFormat {
    /// Platform-native hardware frame. The handle is meaningful only to the
    /// active runtime backend and never crosses the Dart ABI.
    Native,
    /// Planar YUV 4:2:0. Software decode should normally remain in this
    /// format and be converted during GPU sampling.
    Yuv420p,
    /// Semi-planar NV12. Used by platform decoders and some software paths.
    Nv12,
    /// Tightly packed, top-left origin RGBA8.
    ///
    /// This is a fallback format, not the production default: converting every
    /// frame on the CPU adds a full-frame copy and loses hardware zero-copy.
    Rgba8,
}

pub const MAX_VIDEO_PLANES: usize = 3;

/// One plane inside a tightly packed CPU video frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoPlane {
    /// Byte offset from the start of the frame buffer.
    pub offset: usize,
    /// Bytes between consecutive rows inside the packed buffer.
    pub stride: usize,
    pub width: u32,
    pub height: u32,
}

/// One decoded video frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodedVideoFrame {
    pub pts: MediaTime,
    pub duration: Duration,
    pub width: u32,
    pub height: u32,
    /// Legacy single-stride view. For planar formats this is the luma stride.
    pub stride: usize,
    pub format: VideoPixelFormat,
    pub plane_count: u8,
    pub planes: [VideoPlane; MAX_VIDEO_PLANES],
}

/// PCM layout emitted to the host-owned audio output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioLayout {
    pub sample_rate: u32,
    pub channels: u16,
}

/// One decoded PCM block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodedAudioBlock {
    pub pts: MediaTime,
    pub frames: u32,
    pub layout: AudioLayout,
}

/// Random-access source used by runtime decoders.
///
/// Implementations must be safe to call from a decoder thread. The file host
/// implementation resolves logical paths against the active directory/PFS
/// mount, so media never needs to be materialized before decoding.
pub trait MediaSource: Send + Sync {
    fn len(&self) -> Result<u64, String>;

    fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, String>;
}

/// Bounded frame queue that always drops the oldest frame.
#[derive(Debug)]
pub struct FrameQueue<T> {
    items: VecDeque<T>,
    capacity: usize,
    dropped: u64,
}

impl<T> FrameQueue<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            items: VecDeque::with_capacity(capacity.max(1)),
            capacity: capacity.max(1),
            dropped: 0,
        }
    }

    pub fn push(&mut self, value: T) -> Option<T> {
        let dropped = if self.items.len() == self.capacity {
            self.dropped = self.dropped.saturating_add(1);
            self.items.pop_front()
        } else {
            None
        };
        self.items.push_back(value);
        dropped
    }

    pub fn pop(&mut self) -> Option<T> {
        self.items.pop_front()
    }

    pub fn front(&self) -> Option<&T> {
        self.items.front()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_time_uses_saturating_arithmetic() {
        let start = MediaTime::from_millis(10);
        assert_eq!(start.saturating_add(Duration::from_millis(5)).millis(), 15);
        assert_eq!(
            MediaTime::ZERO.saturating_sub(Duration::from_millis(1)),
            MediaTime::ZERO
        );
        assert_eq!(
            MediaTime::from_micros(u64::MAX)
                .saturating_add_micros(1)
                .micros(),
            u64::MAX
        );
    }

    #[test]
    fn frame_queue_drops_oldest_and_keeps_order() {
        let mut queue = FrameQueue::new(2);
        assert_eq!(queue.push(1), None);
        assert_eq!(queue.push(2), None);
        assert_eq!(queue.push(3), Some(1));
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.dropped(), 1);
        assert_eq!(queue.pop(), Some(2));
        assert_eq!(queue.pop(), Some(3));
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn queues_never_allow_zero_capacity() {
        let mut queue = FrameQueue::new(0);
        queue.push(1);
        queue.push(2);
        assert_eq!(queue.pop(), Some(2));
    }

    #[test]
    fn media_source_is_object_safe() {
        struct Source(Vec<u8>);

        impl MediaSource for Source {
            fn len(&self) -> Result<u64, String> {
                Ok(self.0.len() as u64)
            }

            fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, String> {
                let offset = usize::try_from(offset).map_err(|error| error.to_string())?;
                if offset >= self.0.len() {
                    return Ok(0);
                }
                let count = output.len().min(self.0.len() - offset);
                output[..count].copy_from_slice(&self.0[offset..offset + count]);
                Ok(count)
            }
        }

        let source: Box<dyn MediaSource> = Box::new(Source(vec![1, 2, 3]));
        let mut output = [0u8; 2];
        assert_eq!(source.len().unwrap(), 3);
        assert_eq!(source.read_at(1, &mut output).unwrap(), 2);
        assert_eq!(output, [2, 3]);
    }
}
