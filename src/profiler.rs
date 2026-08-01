use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_millis(500);
const QUEUE_CAPACITY: usize = 256;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FrameProfile {
    pub enabled: bool,
    started: Option<Instant>,
    pub ffi_call_ns: u64,
    pub logic_ns: u64,
    pub input_ns: u64,
    pub interpreter_ns: u64,
    pub events_ns: u64,
    pub emote_ns: u64,
    pub audio_media_ns: u64,
    pub compositor_ns: u64,
    pub text_ns: u64,
    pub frame_build_ns: u64,
    pub transition_capture_ns: u64,
    pub gpu_submit_ns: u64,
    pub present_ns: u64,
    pub readback_ns: u64,
    pub host_ffi_ns: u64,
    pub host_ffi_calls: u64,
    pub host_ffi_bytes: u64,
    pub rendered: bool,
    pub damage_pixels: u64,
    pub stage_pixels: u64,
    pub draw_calls: u64,
    pub texture_count: u64,
    pub texture_gpu_bytes: u64,
    pub texture_cpu_bytes: u64,
    pub emote_layers: u64,
    pub emote_source_bytes: u64,
}

impl FrameProfile {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            started: enabled.then(Instant::now),
            ..Self::default()
        }
    }

    pub(crate) fn mark(&self) -> Option<Instant> {
        self.enabled.then(Instant::now)
    }

    pub(crate) fn elapsed(start: Option<Instant>) -> u64 {
        start
            .map(|value| value.elapsed().as_nanos().min(u64::MAX as u128) as u64)
            .unwrap_or(0)
    }

    pub(crate) fn finish(&mut self) {
        self.ffi_call_ns = Self::elapsed(self.started);
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ProfileTimings {
    pub ffi_call_ms: f64,
    pub logic_ms: f64,
    pub input_ms: f64,
    pub interpreter_ms: f64,
    pub events_ms: f64,
    pub emote_ms: f64,
    pub audio_media_ms: f64,
    pub compositor_ms: f64,
    pub text_ms: f64,
    pub frame_build_ms: f64,
    pub transition_capture_ms: f64,
    pub gpu_submit_ms: f64,
    pub present_ms: f64,
    pub readback_ms: f64,
    pub host_ffi_ms: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ProfilerSnapshot {
    pub enabled: bool,
    pub window_ms: u64,
    pub tick_hz: f64,
    pub rendered_fps: f64,
    pub average: ProfileTimings,
    pub maximum: ProfileTimings,
    pub damage_percent: f64,
    pub draw_calls: f64,
    pub host_ffi_calls_per_second: f64,
    pub host_ffi_mib_per_second: f64,
    pub texture_count: u64,
    pub texture_gpu_mib: f64,
    pub texture_cpu_mib: f64,
    pub emote_layers: u64,
    pub emote_source_mib: f64,
    pub dropped_samples: u64,
}

enum Message {
    Frame(FrameProfile),
    Reset(bool),
}

pub struct RuntimeProfiler {
    enabled: Arc<AtomicBool>,
    dropped: Arc<AtomicU64>,
    snapshot: Arc<RwLock<ProfilerSnapshot>>,
    sender: Option<mpsc::SyncSender<Message>>,
    worker: Option<JoinHandle<()>>,
}

impl RuntimeProfiler {
    pub fn new() -> Self {
        let enabled = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicU64::new(0));
        let snapshot = Arc::new(RwLock::new(ProfilerSnapshot::default()));
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let worker_snapshot = Arc::clone(&snapshot);
        let worker_dropped = Arc::clone(&dropped);
        let worker = std::thread::Builder::new()
            .name("art3m1s-profiler".into())
            .spawn(move || aggregate(receiver, worker_snapshot, worker_dropped))
            .ok();
        Self {
            enabled,
            dropped,
            snapshot,
            sender: Some(sender),
            worker,
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
        self.dropped.store(0, Ordering::Relaxed);
        crate::ffi::set_profile_io_enabled(enabled);
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(Message::Reset(enabled));
        }
        if let Ok(mut snapshot) = self.snapshot.write() {
            snapshot.enabled = enabled;
            if !enabled {
                *snapshot = ProfilerSnapshot::default();
            }
        }
    }

    pub(crate) fn begin_frame(&self) -> FrameProfile {
        FrameProfile::new(self.enabled.load(Ordering::Relaxed))
    }

    pub(crate) fn submit(&self, frame: FrameProfile) {
        if !frame.enabled {
            return;
        }
        if self
            .sender
            .as_ref()
            .is_none_or(|sender| sender.try_send(Message::Frame(frame)).is_err())
        {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn snapshot_json(&self) -> String {
        let snapshot = self
            .snapshot
            .read()
            .map(|value| value.clone())
            .unwrap_or_default();
        serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".into())
    }
}

impl Drop for RuntimeProfiler {
    fn drop(&mut self) {
        self.enabled.store(false, Ordering::Relaxed);
        crate::ffi::set_profile_io_enabled(false);
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Default)]
struct WindowAccumulator {
    frames: u64,
    rendered: u64,
    sums: [u128; 15],
    maxima: [u64; 15],
    damage_pixels: u128,
    stage_pixels: u128,
    draw_calls: u128,
    host_ffi_calls: u128,
    host_ffi_bytes: u128,
    latest: FrameProfile,
}

impl WindowAccumulator {
    fn push(&mut self, frame: FrameProfile) {
        let values = timing_values(&frame);
        self.frames += 1;
        self.rendered += u64::from(frame.rendered);
        for (index, value) in values.into_iter().enumerate() {
            self.sums[index] += value as u128;
            self.maxima[index] = self.maxima[index].max(value);
        }
        if frame.rendered {
            self.damage_pixels += frame.damage_pixels as u128;
            self.stage_pixels += frame.stage_pixels as u128;
            self.draw_calls += frame.draw_calls as u128;
        }
        self.host_ffi_calls += frame.host_ffi_calls as u128;
        self.host_ffi_bytes += frame.host_ffi_bytes as u128;
        self.latest = frame;
    }

    fn snapshot(&self, elapsed: Duration, enabled: bool, dropped: u64) -> ProfilerSnapshot {
        let seconds = elapsed.as_secs_f64().max(0.001);
        let frame_divisor = self.frames.max(1) as f64;
        let render_divisor = self.rendered.max(1) as f64;
        ProfilerSnapshot {
            enabled,
            window_ms: elapsed.as_millis().min(u64::MAX as u128) as u64,
            tick_hz: self.frames as f64 / seconds,
            rendered_fps: self.rendered as f64 / seconds,
            average: timings_from_values(self.sums.map(|value| value as f64 / frame_divisor)),
            maximum: timings_from_values(self.maxima.map(|value| value as f64)),
            damage_percent: if self.stage_pixels == 0 {
                0.0
            } else {
                self.damage_pixels as f64 * 100.0 / self.stage_pixels as f64
            },
            draw_calls: self.draw_calls as f64 / render_divisor,
            host_ffi_calls_per_second: self.host_ffi_calls as f64 / seconds,
            host_ffi_mib_per_second: self.host_ffi_bytes as f64 / (1024.0 * 1024.0) / seconds,
            texture_count: self.latest.texture_count,
            texture_gpu_mib: mib(self.latest.texture_gpu_bytes),
            texture_cpu_mib: mib(self.latest.texture_cpu_bytes),
            emote_layers: self.latest.emote_layers,
            emote_source_mib: mib(self.latest.emote_source_bytes),
            dropped_samples: dropped,
        }
    }
}

fn aggregate(
    receiver: mpsc::Receiver<Message>,
    snapshot: Arc<RwLock<ProfilerSnapshot>>,
    dropped: Arc<AtomicU64>,
) {
    let mut enabled = false;
    let mut session_started = Instant::now();
    let mut refresh_started = Instant::now();
    let mut accumulator = WindowAccumulator::default();
    loop {
        if !enabled {
            match receiver.recv() {
                Ok(Message::Reset(value)) => {
                    enabled = value;
                    session_started = Instant::now();
                    refresh_started = session_started;
                    accumulator = WindowAccumulator::default();
                    continue;
                }
                Ok(Message::Frame(_)) => continue,
                Err(_) => break,
            }
        }
        let timeout = WINDOW.saturating_sub(refresh_started.elapsed());
        match receiver.recv_timeout(timeout) {
            Ok(Message::Frame(frame)) if enabled => accumulator.push(frame),
            Ok(Message::Frame(_)) => {}
            Ok(Message::Reset(value)) => {
                enabled = value;
                session_started = Instant::now();
                refresh_started = session_started;
                accumulator = WindowAccumulator::default();
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if refresh_started.elapsed() >= WINDOW {
            let value = accumulator.snapshot(
                session_started.elapsed(),
                enabled,
                dropped.load(Ordering::Relaxed),
            );
            if let Ok(mut output) = snapshot.write() {
                *output = value;
            }
            // Only the publication clock rolls forward. The accumulator is
            // intentionally session-long so average values converge and peak
            // values remain visible until profiling is explicitly restarted.
            refresh_started = Instant::now();
        }
    }
}

fn timing_values(frame: &FrameProfile) -> [u64; 15] {
    [
        frame.ffi_call_ns,
        frame.logic_ns,
        frame.input_ns,
        frame.interpreter_ns,
        frame.events_ns,
        frame.emote_ns,
        frame.audio_media_ns,
        frame.compositor_ns,
        frame.text_ns,
        frame.frame_build_ns,
        frame.transition_capture_ns,
        frame.gpu_submit_ns,
        frame.present_ns,
        frame.readback_ns,
        frame.host_ffi_ns,
    ]
}

fn timings_from_values(values: [f64; 15]) -> ProfileTimings {
    let ms = |value: f64| value / 1_000_000.0;
    ProfileTimings {
        ffi_call_ms: ms(values[0]),
        logic_ms: ms(values[1]),
        input_ms: ms(values[2]),
        interpreter_ms: ms(values[3]),
        events_ms: ms(values[4]),
        emote_ms: ms(values[5]),
        audio_media_ms: ms(values[6]),
        compositor_ms: ms(values[7]),
        text_ms: ms(values[8]),
        frame_build_ms: ms(values[9]),
        transition_capture_ms: ms(values[10]),
        gpu_submit_ms: ms(values[11]),
        present_ms: ms(values[12]),
        readback_ms: ms(values[13]),
        host_ffi_ms: ms(values[14]),
    }
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulator_keeps_interpreter_separate_from_host_ffi() {
        let mut accumulator = WindowAccumulator::default();
        accumulator.push(FrameProfile {
            enabled: true,
            interpreter_ns: 2_000_000,
            host_ffi_ns: 500_000,
            host_ffi_calls: 2,
            rendered: true,
            damage_pixels: 25,
            stage_pixels: 100,
            ..FrameProfile::default()
        });
        let snapshot = accumulator.snapshot(Duration::from_secs(1), true, 0);
        assert_eq!(snapshot.average.interpreter_ms, 2.0);
        assert_eq!(snapshot.average.host_ffi_ms, 0.5);
        assert_eq!(snapshot.host_ffi_calls_per_second, 2.0);
        assert_eq!(snapshot.damage_percent, 25.0);
    }

    #[test]
    fn accumulator_keeps_session_average_and_peak() {
        let mut accumulator = WindowAccumulator::default();
        accumulator.push(FrameProfile {
            enabled: true,
            interpreter_ns: 1_000_000,
            ..FrameProfile::default()
        });
        accumulator.push(FrameProfile {
            enabled: true,
            interpreter_ns: 5_000_000,
            ..FrameProfile::default()
        });

        let snapshot = accumulator.snapshot(Duration::from_secs(2), true, 0);
        assert_eq!(snapshot.average.interpreter_ms, 3.0);
        assert_eq!(snapshot.maximum.interpreter_ms, 5.0);
        assert_eq!(snapshot.window_ms, 2_000);
    }
}
