//! Device output owns only PCM. The thread-confined VM is mixed by runtime_tick.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

const SOURCE_RATE: u32 = 48_000;
const CAPACITY: usize = 4_800;

struct Queue {
    frames: VecDeque<[f32; 2]>,
    phase: f64,
    gain: f32,
    suspended: bool,
}

impl Queue {
    fn new() -> Self {
        Self {
            frames: VecDeque::with_capacity(CAPACITY),
            phase: 0.0,
            gain: 1.0,
            suspended: false,
        }
    }

    fn next(&mut self, rate: u32) -> [f32; 2] {
        if self.suspended {
            return [0.0; 2];
        }
        let Some(first) = self.frames.front().copied() else {
            self.phase = 0.0;
            return [0.0; 2];
        };
        let second = self.frames.get(1).copied().unwrap_or(first);
        let t = self.phase as f32;
        let result = std::array::from_fn(|i| (first[i] + (second[i] - first[i]) * t) * self.gain);
        self.phase += f64::from(SOURCE_RATE) / f64::from(rate);
        while self.phase >= 1.0 {
            self.frames.pop_front();
            self.phase -= 1.0;
        }
        result
    }
}

pub(super) struct Output {
    _stream: cpal::Stream,
    queue: Arc<Mutex<Queue>>,
    failed: Arc<AtomicBool>,
}

impl Output {
    pub(super) fn start() -> Result<Self, String> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("No audio output device")?;
        let preferred = device
            .supported_output_configs()
            .ok()
            .and_then(|mut configs| {
                configs.find(|c| {
                    c.channels() == 2
                        && c.min_sample_rate().0 <= SOURCE_RATE
                        && c.max_sample_rate().0 >= SOURCE_RATE
                        && matches!(
                            c.sample_format(),
                            cpal::SampleFormat::F32
                                | cpal::SampleFormat::I16
                                | cpal::SampleFormat::U16
                        )
                })
            });
        let supported = match preferred {
            Some(config) => config.with_sample_rate(cpal::SampleRate(SOURCE_RATE)),
            None => device.default_output_config().map_err(|e| e.to_string())?,
        };
        let config: cpal::StreamConfig = supported.clone().into();
        if config.channels == 0 || config.sample_rate.0 == 0 {
            return Err("Invalid audio format".into());
        }
        let queue = Arc::new(Mutex::new(Queue::new()));
        let failed = Arc::new(AtomicBool::new(false));
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => {
                build::<f32>(&device, &config, queue.clone(), failed.clone())
            }
            cpal::SampleFormat::I16 => {
                build::<i16>(&device, &config, queue.clone(), failed.clone())
            }
            cpal::SampleFormat::U16 => {
                build::<u16>(&device, &config, queue.clone(), failed.clone())
            }
            format => return Err(format!("Unsupported device audio format: {format:?}")),
        }
        .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(Self {
            _stream: stream,
            queue,
            failed,
        })
    }

    pub(super) fn capacity(&self) -> usize {
        self.queue
            .lock()
            .map_or(0, |q| CAPACITY.saturating_sub(q.frames.len()))
    }

    pub(super) fn append(&self, samples: &[f32]) {
        if let Ok(mut queue) = self.queue.lock() {
            let available = CAPACITY.saturating_sub(queue.frames.len());
            for frame in samples.chunks_exact(2).take(available) {
                queue.frames.push_back(std::array::from_fn(|i| {
                    if frame[i].is_finite() {
                        frame[i].clamp(-1.0, 1.0)
                    } else {
                        0.0
                    }
                }));
            }
        }
    }

    pub(super) fn suspend(&self, value: bool) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.suspended = value;
        }
    }

    pub(super) fn volume(&self, value: f32) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.gain = value.clamp(0.0, 1.0);
        }
    }

    pub(super) fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
}

fn build<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    queue: Arc<Mutex<Queue>>,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    let channels = usize::from(config.channels);
    let rate = config.sample_rate.0;
    device.build_output_stream(
        config,
        move |output: &mut [T], _| {
            let Ok(mut queue) = queue.lock() else {
                output.fill(T::from_sample(0.0));
                return;
            };
            for frame in output.chunks_mut(channels) {
                let stereo = queue.next(rate);
                for (index, sample) in frame.iter_mut().enumerate() {
                    let value = if channels == 1 {
                        (stereo[0] + stereo[1]) * 0.5
                    } else {
                        stereo.get(index).copied().unwrap_or(0.0)
                    };
                    *sample = T::from_sample(value);
                }
            }
        },
        move |_| {
            failed.store(true, Ordering::Relaxed);
        },
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspension_keeps_samples_and_underflow_produces_silence() {
        let mut queue = Queue::new();
        queue.frames.push_back([0.25, -0.25]);
        queue.suspended = true;
        assert_eq!(queue.next(48_000), [0.0; 2]);
        assert_eq!(queue.frames.len(), 1);
        queue.suspended = false;
        queue.gain = 0.5;
        assert_eq!(queue.next(48_000), [0.125, -0.125]);
        assert_eq!(queue.next(48_000), [0.0; 2]);
    }

    #[test]
    fn device_rate_conversion_preserves_stereo_and_source_clock() {
        let mut queue = Queue::new();
        queue.frames.extend([[0.0, 1.0], [1.0, 0.0], [0.0, 1.0]]);
        assert_eq!(queue.next(96_000), [0.0, 1.0]);
        assert_eq!(queue.next(96_000), [0.5, 0.5]);
        assert_eq!(queue.next(96_000), [1.0, 0.0]);
        assert_eq!(queue.frames.len(), 2);
    }
}
