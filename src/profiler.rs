use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const PUBLISH_INTERVAL: Duration = Duration::from_millis(500);
const SAMPLE_WINDOW: Duration = Duration::from_secs(10);
const QUEUE_CAPACITY: usize = 256;
const MAX_WINDOW_SAMPLES: usize = 4096;
const TIMING_COUNT: usize = 24;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FrameProfile {
    pub enabled: bool,
    started: Option<Instant>,
    pub ffi_call_ns: u64,
    pub logic_ns: u64,
    pub input_ns: u64,
    pub interpreter_ns: u64,
    pub events_ns: u64,
    pub event_runtime_ns: u64,
    pub event_media_ns: u64,
    pub event_text_ns: u64,
    pub event_transition_ns: u64,
    pub event_compositor_ns: u64,
    pub event_layer_sync_ns: u64,
    pub emote_ns: u64,
    pub audio_media_ns: u64,
    pub compositor_ns: u64,
    pub text_ns: u64,
    pub frame_build_ns: u64,
    pub damage_compute_ns: u64,
    pub transition_capture_ns: u64,
    pub texture_upload_ns: u64,
    pub video_upload_ns: u64,
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
    pub vertices: u64,
    pub texture_binds: u64,
    pub draw_list_commands: u64,
    pub uploaded_bytes: u64,
    pub video_uploaded_bytes: u64,
    pub video_uploaded_frames: u64,
    pub dynamic_mesh_uploaded_bytes: u64,
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
    pub event_runtime_ms: f64,
    pub event_media_ms: f64,
    pub event_text_ms: f64,
    pub event_transition_ms: f64,
    pub event_compositor_ms: f64,
    pub event_layer_sync_ms: f64,
    pub emote_ms: f64,
    pub audio_media_ms: f64,
    pub compositor_ms: f64,
    pub text_ms: f64,
    pub frame_build_ms: f64,
    pub damage_compute_ms: f64,
    pub transition_capture_ms: f64,
    pub texture_upload_ms: f64,
    pub video_upload_ms: f64,
    pub gpu_submit_ms: f64,
    pub present_ms: f64,
    pub readback_ms: f64,
    pub host_ffi_ms: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ProfilerSnapshot {
    pub enabled: bool,
    /// Kept for older hosts; equivalent to `session_ms`.
    pub window_ms: u64,
    pub session_ms: u64,
    pub sample_window_ms: u64,
    pub sample_count: u64,
    pub tick_hz: f64,
    pub rendered_fps: f64,
    pub current: ProfileTimings,
    pub average: ProfileTimings,
    pub one_percent: ProfileTimings,
    /// Compatibility alias for old hosts. It now contains `one_percent`.
    pub maximum: ProfileTimings,
    pub damage_percent: f64,
    pub current_rendered: bool,
    pub draw_calls: u64,
    pub vertices: u64,
    pub texture_binds: u64,
    pub draw_list_commands: u64,
    pub rendered_frames: u64,
    pub skipped_frames: u64,
    pub host_ffi_calls_per_second: f64,
    pub host_ffi_mib_per_second: f64,
    pub uploaded_mib_per_second: f64,
    pub video_uploaded_mib_per_second: f64,
    pub video_uploaded_frames_per_second: f64,
    pub dynamic_mesh_uploaded_mib_per_second: f64,
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
            // Toggling is a cold UI path. Delivering the reset reliably is
            // more important than avoiding a short wait behind queued frames.
            let _ = sender.send(Message::Reset(enabled));
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

#[derive(Clone, Copy)]
struct TimedFrame {
    captured_at: Instant,
    profile: FrameProfile,
}

#[derive(Default)]
struct RollingWindow {
    samples: VecDeque<TimedFrame>,
}

impl RollingWindow {
    fn push(&mut self, frame: FrameProfile) {
        self.push_at(Instant::now(), frame);
    }

    fn push_at(&mut self, captured_at: Instant, frame: FrameProfile) {
        self.samples.push_back(TimedFrame {
            captured_at,
            profile: frame,
        });
        self.trim(captured_at);
    }

    fn trim(&mut self, now: Instant) {
        while self
            .samples
            .front()
            .is_some_and(|sample| now.saturating_duration_since(sample.captured_at) > SAMPLE_WINDOW)
        {
            self.samples.pop_front();
        }
        while self.samples.len() > MAX_WINDOW_SAMPLES {
            self.samples.pop_front();
        }
    }

    fn snapshot(
        &mut self,
        now: Instant,
        session_elapsed: Duration,
        enabled: bool,
        dropped: u64,
    ) -> ProfilerSnapshot {
        self.trim(now);
        let profiles = self
            .samples
            .iter()
            .map(|sample| sample.profile)
            .collect::<Vec<_>>();
        let latest = profiles.last().copied().unwrap_or_default();
        let frame_count = profiles.len() as u64;
        let rendered_frames = profiles.iter().filter(|frame| frame.rendered).count() as u64;
        let sample_elapsed = session_elapsed.min(SAMPLE_WINDOW);
        let seconds = sample_elapsed.as_secs_f64().max(0.001);
        let sums = profiles
            .iter()
            .fold([0u128; TIMING_COUNT], |mut sums, frame| {
                for (index, value) in timing_values(frame).into_iter().enumerate() {
                    sums[index] += value as u128;
                }
                sums
            });
        let divisor = frame_count.max(1) as f64;
        let average_values = sums.map(|value| value as f64 / divisor);
        let one_percent_values = std::array::from_fn(|index| {
            slowest_one_percent_average(profiles.iter().map(|frame| timing_values(frame)[index]))
        });
        let current = timings_from_values(timing_values(&latest).map(|value| value as f64));
        let average = timings_from_values(average_values);
        let one_percent = timings_from_values(one_percent_values);
        let sum_counter = |read: fn(&FrameProfile) -> u64| -> u128 {
            profiles.iter().map(|frame| read(frame) as u128).sum()
        };
        let session_ms = duration_ms(session_elapsed);
        ProfilerSnapshot {
            enabled,
            window_ms: session_ms,
            session_ms,
            sample_window_ms: duration_ms(sample_elapsed),
            sample_count: frame_count,
            tick_hz: frame_count as f64 / seconds,
            rendered_fps: rendered_frames as f64 / seconds,
            current,
            average,
            one_percent: one_percent.clone(),
            maximum: one_percent,
            damage_percent: if latest.stage_pixels == 0 {
                0.0
            } else {
                latest.damage_pixels as f64 * 100.0 / latest.stage_pixels as f64
            },
            current_rendered: latest.rendered,
            draw_calls: latest.draw_calls,
            vertices: latest.vertices,
            texture_binds: latest.texture_binds,
            draw_list_commands: latest.draw_list_commands,
            rendered_frames,
            skipped_frames: frame_count.saturating_sub(rendered_frames),
            host_ffi_calls_per_second: sum_counter(|frame| frame.host_ffi_calls) as f64 / seconds,
            host_ffi_mib_per_second: bytes_per_second_to_mib(
                sum_counter(|frame| frame.host_ffi_bytes),
                seconds,
            ),
            uploaded_mib_per_second: bytes_per_second_to_mib(
                sum_counter(|frame| frame.uploaded_bytes),
                seconds,
            ),
            video_uploaded_mib_per_second: bytes_per_second_to_mib(
                sum_counter(|frame| frame.video_uploaded_bytes),
                seconds,
            ),
            video_uploaded_frames_per_second: sum_counter(|frame| frame.video_uploaded_frames)
                as f64
                / seconds,
            dynamic_mesh_uploaded_mib_per_second: bytes_per_second_to_mib(
                sum_counter(|frame| frame.dynamic_mesh_uploaded_bytes),
                seconds,
            ),
            texture_count: latest.texture_count,
            texture_gpu_mib: mib(latest.texture_gpu_bytes),
            texture_cpu_mib: mib(latest.texture_cpu_bytes),
            emote_layers: latest.emote_layers,
            emote_source_mib: mib(latest.emote_source_bytes),
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
    let mut window = RollingWindow::default();
    loop {
        if !enabled {
            match receiver.recv() {
                Ok(Message::Reset(value)) => {
                    enabled = value;
                    session_started = Instant::now();
                    refresh_started = session_started;
                    window = RollingWindow::default();
                    continue;
                }
                Ok(Message::Frame(_)) => continue,
                Err(_) => break,
            }
        }
        let timeout = PUBLISH_INTERVAL.saturating_sub(refresh_started.elapsed());
        match receiver.recv_timeout(timeout) {
            Ok(Message::Frame(frame)) if enabled => window.push(frame),
            Ok(Message::Frame(_)) => {}
            Ok(Message::Reset(value)) => {
                enabled = value;
                session_started = Instant::now();
                refresh_started = session_started;
                window = RollingWindow::default();
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if refresh_started.elapsed() >= PUBLISH_INTERVAL {
            let now = Instant::now();
            let value = window.snapshot(
                now,
                now.saturating_duration_since(session_started),
                enabled,
                dropped.load(Ordering::Relaxed),
            );
            if let Ok(mut output) = snapshot.write() {
                *output = value;
            }
            refresh_started = now;
        }
    }
}

fn timing_values(frame: &FrameProfile) -> [u64; TIMING_COUNT] {
    [
        frame.ffi_call_ns,
        frame.logic_ns,
        frame.input_ns,
        frame.interpreter_ns,
        frame.events_ns,
        frame.event_runtime_ns,
        frame.event_media_ns,
        frame.event_text_ns,
        frame.event_transition_ns,
        frame.event_compositor_ns,
        frame.event_layer_sync_ns,
        frame.emote_ns,
        frame.audio_media_ns,
        frame.compositor_ns,
        frame.text_ns,
        frame.frame_build_ns,
        frame.damage_compute_ns,
        frame.transition_capture_ns,
        frame.texture_upload_ns,
        frame.video_upload_ns,
        frame.gpu_submit_ns,
        frame.present_ns,
        frame.readback_ns,
        frame.host_ffi_ns,
    ]
}

fn timings_from_values(values: [f64; TIMING_COUNT]) -> ProfileTimings {
    let ms = |value: f64| value / 1_000_000.0;
    ProfileTimings {
        ffi_call_ms: ms(values[0]),
        logic_ms: ms(values[1]),
        input_ms: ms(values[2]),
        interpreter_ms: ms(values[3]),
        events_ms: ms(values[4]),
        event_runtime_ms: ms(values[5]),
        event_media_ms: ms(values[6]),
        event_text_ms: ms(values[7]),
        event_transition_ms: ms(values[8]),
        event_compositor_ms: ms(values[9]),
        event_layer_sync_ms: ms(values[10]),
        emote_ms: ms(values[11]),
        audio_media_ms: ms(values[12]),
        compositor_ms: ms(values[13]),
        text_ms: ms(values[14]),
        frame_build_ms: ms(values[15]),
        damage_compute_ms: ms(values[16]),
        transition_capture_ms: ms(values[17]),
        texture_upload_ms: ms(values[18]),
        video_upload_ms: ms(values[19]),
        gpu_submit_ms: ms(values[20]),
        present_ms: ms(values[21]),
        readback_ms: ms(values[22]),
        host_ffi_ms: ms(values[23]),
    }
}

fn slowest_one_percent_average(values: impl Iterator<Item = u64>) -> f64 {
    let mut values = values.collect::<Vec<_>>();
    if values.is_empty() {
        return 0.0;
    }
    values.sort_unstable_by(|left, right| right.cmp(left));
    let tail = values.len().div_ceil(100).max(1);
    values[..tail]
        .iter()
        .map(|value| *value as u128)
        .sum::<u128>() as f64
        / tail as f64
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

fn bytes_per_second_to_mib(bytes: u128, seconds: f64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0) / seconds
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolling_window_keeps_current_state_separate_from_timing_averages() {
        let now = Instant::now();
        let mut window = RollingWindow::default();
        window.push_at(
            now - Duration::from_secs(1),
            FrameProfile {
                enabled: true,
                interpreter_ns: 2_000_000,
                host_ffi_ns: 500_000,
                host_ffi_calls: 2,
                rendered: true,
                damage_pixels: 100,
                stage_pixels: 100,
                draw_calls: 10,
                ..FrameProfile::default()
            },
        );
        window.push_at(
            now,
            FrameProfile {
                enabled: true,
                interpreter_ns: 4_000_000,
                damage_pixels: 25,
                stage_pixels: 100,
                draw_calls: 3,
                ..FrameProfile::default()
            },
        );
        let snapshot = window.snapshot(now, Duration::from_secs(1), true, 0);
        assert_eq!(snapshot.current.interpreter_ms, 4.0);
        assert_eq!(snapshot.average.interpreter_ms, 3.0);
        assert_eq!(snapshot.average.host_ffi_ms, 0.25);
        assert_eq!(snapshot.host_ffi_calls_per_second, 2.0);
        assert_eq!(snapshot.damage_percent, 25.0);
        assert_eq!(snapshot.draw_calls, 3);
    }

    #[test]
    fn slowest_one_percent_averages_the_tail_instead_of_one_peak() {
        let values = (0..200).map(|index| match index {
            198 => 10,
            199 => 20,
            _ => 1,
        });
        assert_eq!(slowest_one_percent_average(values), 15.0);
    }

    #[test]
    fn event_dispatch_breakdown_is_preserved_in_snapshots() {
        let timings = timings_from_values(
            timing_values(&FrameProfile {
                events_ns: 28_000_000,
                event_runtime_ns: 1_000_000,
                event_media_ns: 2_000_000,
                event_text_ns: 3_000_000,
                event_transition_ns: 4_000_000,
                event_compositor_ns: 8_000_000,
                event_layer_sync_ns: 10_000_000,
                ..FrameProfile::default()
            })
            .map(|value| value as f64),
        );

        assert_eq!(timings.events_ms, 28.0);
        assert_eq!(timings.event_runtime_ms, 1.0);
        assert_eq!(timings.event_media_ms, 2.0);
        assert_eq!(timings.event_text_ms, 3.0);
        assert_eq!(timings.event_transition_ms, 4.0);
        assert_eq!(timings.event_compositor_ms, 8.0);
        assert_eq!(timings.event_layer_sync_ms, 10.0);
    }

    #[test]
    fn rolling_window_evicts_startup_spikes() {
        let now = Instant::now();
        let mut window = RollingWindow::default();
        window.push_at(
            now - SAMPLE_WINDOW - Duration::from_millis(1),
            FrameProfile {
                enabled: true,
                interpreter_ns: 100_000_000,
                ..FrameProfile::default()
            },
        );
        window.push_at(
            now,
            FrameProfile {
                enabled: true,
                interpreter_ns: 1_000_000,
                ..FrameProfile::default()
            },
        );

        let snapshot = window.snapshot(now, Duration::from_secs(30), true, 0);
        assert_eq!(snapshot.sample_window_ms, 10_000);
        assert_eq!(snapshot.sample_count, 1);
        assert_eq!(snapshot.average.interpreter_ms, 1.0);
        assert_eq!(snapshot.one_percent.interpreter_ms, 1.0);
    }
}
