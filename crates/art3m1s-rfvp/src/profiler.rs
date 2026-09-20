//! Lightweight rolling profiler for the RFVP host runtime.
//!
//! Emits the same JSON schema as the core `RuntimeProfiler` so hosts can
//! reuse their snapshot parser; fields the RFVP path cannot measure are
//! omitted and default to zero on the host side.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use art3m1s_render::{GpuProfileStats, RenderRegion};

const WINDOW: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Default)]
struct FrameSample {
    at_ms: f64,
    logic_ms: f64,
    render_ms: f64,
    present_ms: f64,
    readback_ms: f64,
    rendered: bool,
    damage_percent: f64,
    draw_list_commands: u64,
    gpu: GpuProfileStats,
}

#[derive(Default)]
pub struct RfvpProfiler {
    enabled: bool,
    session_start: Option<Instant>,
    /// Timings of the in-flight logical frame (step + optional render).
    pending: Option<FrameSample>,
    samples: VecDeque<FrameSample>,
    rendered_frames: u64,
    skipped_frames: u64,
}

impl RfvpProfiler {
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        self.samples.clear();
        self.pending = None;
        self.rendered_frames = 0;
        self.skipped_frames = 0;
        self.session_start = enabled.then(Instant::now);
    }

    fn ensure_pending(&mut self) -> Option<&mut FrameSample> {
        if !self.enabled {
            return None;
        }
        let start = self.session_start?;
        if self.pending.is_none() {
            self.pending = Some(FrameSample {
                at_ms: start.elapsed().as_secs_f64() * 1000.0,
                ..FrameSample::default()
            });
        }
        self.pending.as_mut()
    }

    /// Pushes the in-flight frame into the rolling window, dropping samples
    /// older than the window.
    fn flush_pending(&mut self) {
        let Some(sample) = self.pending.take() else {
            return;
        };
        if sample.rendered {
            self.rendered_frames += 1;
        } else {
            self.skipped_frames += 1;
        }
        let cutoff = sample.at_ms - WINDOW.as_secs_f64() * 1000.0;
        while self
            .samples
            .front()
            .is_some_and(|front| front.at_ms < cutoff)
        {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    pub fn record_step(&mut self, elapsed: Duration) {
        // A pending frame that never reached render (no frame pending from the
        // engine) is still a completed tick.
        if self.pending.is_some() {
            self.flush_pending();
        }
        if let Some(sample) = self.ensure_pending() {
            sample.logic_ms = elapsed.as_secs_f64() * 1000.0;
        }
    }

    pub fn record_render(
        &mut self,
        elapsed: Duration,
        region: Option<&RenderRegion>,
        draw_list_commands: usize,
        stage: (u32, u32),
        gpu: GpuProfileStats,
    ) {
        let Some(sample) = self.ensure_pending() else {
            return;
        };
        sample.render_ms = elapsed.as_secs_f64() * 1000.0;
        sample.draw_list_commands = draw_list_commands as u64;
        sample.gpu = gpu;
        match region {
            None => sample.rendered = false,
            Some(RenderRegion::Full) => {
                sample.rendered = true;
                sample.damage_percent = 100.0;
            }
            Some(RenderRegion::Rect(rectangle)) => {
                sample.rendered = true;
                let stage_area = f64::from(stage.0) * f64::from(stage.1);
                if stage_area > 0.0 {
                    sample.damage_percent =
                        (f64::from(rectangle[2]) * f64::from(rectangle[3])) / stage_area * 100.0;
                }
            }
        }
        self.flush_pending();
    }

    pub fn record_present(&mut self, elapsed: Duration) {
        if let Some(sample) = self.samples.back_mut() {
            sample.present_ms += elapsed.as_secs_f64() * 1000.0;
        }
    }

    pub fn record_readback(&mut self, elapsed: Duration) {
        if let Some(sample) = self.samples.back_mut() {
            sample.readback_ms += elapsed.as_secs_f64() * 1000.0;
        }
    }

    pub fn snapshot_json(&self, stage: (u32, u32)) -> String {
        let session_ms = self
            .session_start
            .map(|start| start.elapsed().as_millis() as u64)
            .unwrap_or(0);
        let window_ms = self
            .samples
            .back()
            .zip(self.samples.front())
            .map(|(back, front)| (back.at_ms - front.at_ms).max(1.0))
            .unwrap_or(0.0);
        let sample_count = self.samples.len();
        let tick_hz = if window_ms > 0.0 {
            sample_count as f64 / (window_ms / 1000.0)
        } else {
            0.0
        };
        let rendered_in_window = self.samples.iter().filter(|sample| sample.rendered).count();
        let rendered_fps = if window_ms > 0.0 {
            rendered_in_window as f64 / (window_ms / 1000.0)
        } else {
            0.0
        };

        let current = self.samples.back().copied().unwrap_or_default();
        let average = average_sample(&self.samples);
        let one_percent = one_percent_sample(&self.samples);

        format!(
            concat!(
                "{{\"enabled\":{enabled},\"session_ms\":{session_ms},",
                "\"sample_window_ms\":{window_ms},\"sample_count\":{sample_count},",
                "\"tick_hz\":{tick_hz:.2},\"rendered_fps\":{rendered_fps:.2},",
                "\"current\":{current},\"average\":{average},\"one_percent\":{one_percent},",
                "\"maximum\":{one_percent},\"damage_percent\":{damage:.1},",
                "\"current_rendered\":{current_rendered},",
                "\"draw_calls\":{draw_calls},\"vertices\":{vertices},",
                "\"texture_binds\":{texture_binds},\"draw_list_commands\":{commands},",
                "\"rendered_frames\":{rendered_frames},\"skipped_frames\":{skipped_frames},",
                "\"uploaded_mib_per_second\":{upload_rate:.3},",
                "\"texture_count\":{texture_count},\"texture_gpu_mib\":{gpu_mib:.2},",
                "\"texture_cpu_mib\":{cpu_mib:.2},",
                "\"render_resolution\":\"{stage_w}x{stage_h}\",",
                "\"output_resolution\":\"{stage_w}x{stage_h}\",",
                "\"upscale_enabled\":{upscale}}}"
            ),
            enabled = self.enabled,
            session_ms = session_ms,
            window_ms = window_ms as u64,
            sample_count = sample_count,
            tick_hz = tick_hz,
            rendered_fps = rendered_fps,
            current = timings_json(&current),
            average = timings_json(&average),
            one_percent = timings_json(&one_percent),
            damage = current.damage_percent,
            current_rendered = current.rendered,
            draw_calls = current.gpu.draw_calls,
            vertices = current.gpu.vertices,
            texture_binds = current.gpu.texture_binds,
            commands = current.draw_list_commands,
            rendered_frames = self.rendered_frames,
            skipped_frames = self.skipped_frames,
            upload_rate = upload_mib_per_second(&self.samples, window_ms),
            texture_count = current.gpu.texture_count,
            gpu_mib = current.gpu.texture_gpu_bytes as f64 / 1048576.0,
            cpu_mib = current.gpu.texture_cpu_bytes as f64 / 1048576.0,
            stage_w = stage.0,
            stage_h = stage.1,
            upscale = current.gpu.upscale_enabled,
        )
    }
}

fn timings_json(sample: &FrameSample) -> String {
    format!(
        concat!(
            "{{\"logic_ms\":{logic:.3},\"frame_build_ms\":{render:.3},",
            "\"texture_upload_ms\":{upload:.3},\"gpu_submit_ms\":{render:.3},",
            "\"present_ms\":{present:.3},\"readback_ms\":{readback:.3}}}"
        ),
        logic = sample.logic_ms,
        render = sample.render_ms,
        upload = sample.gpu.texture_upload_ns as f64 / 1.0e6,
        present = sample.present_ms,
        readback = sample.readback_ms,
    )
}

fn average_sample(samples: &VecDeque<FrameSample>) -> FrameSample {
    if samples.is_empty() {
        return FrameSample::default();
    }
    let mut total = FrameSample::default();
    for sample in samples {
        total.logic_ms += sample.logic_ms;
        total.render_ms += sample.render_ms;
        total.present_ms += sample.present_ms;
        total.readback_ms += sample.readback_ms;
        total.damage_percent += sample.damage_percent;
        total.gpu.texture_upload_ns += sample.gpu.texture_upload_ns;
    }
    let count = samples.len() as f64;
    total.logic_ms /= count;
    total.render_ms /= count;
    total.present_ms /= count;
    total.readback_ms /= count;
    total.damage_percent /= count;
    total.gpu.texture_upload_ns = (total.gpu.texture_upload_ns as f64 / count) as u64;
    total
}

fn one_percent_sample(samples: &VecDeque<FrameSample>) -> FrameSample {
    samples
        .iter()
        .copied()
        .max_by(|left, right| {
            (left.logic_ms + left.render_ms).total_cmp(&(right.logic_ms + right.render_ms))
        })
        .unwrap_or_default()
}

fn upload_mib_per_second(samples: &VecDeque<FrameSample>, window_ms: f64) -> f64 {
    if window_ms <= 0.0 {
        return 0.0;
    }
    let bytes: u64 = samples.iter().map(|sample| sample.gpu.uploaded_bytes).sum();
    (bytes as f64 / 1048576.0) / (window_ms / 1000.0)
}
