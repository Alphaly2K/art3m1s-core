use std::collections::BTreeMap;

use crate::{
    EmoteCommand, EmoteDrawItem, EmoteEvaluationHistory, EmoteEyeControl, EmoteModel,
    EmoteMotionEvaluator, EmotePlayer, EmoteRenderState, EmoteTransform, Result,
};

/// Session-owned playback and pose evaluation for one E-Mote model instance.
pub struct EmotePlayback {
    model: EmoteModel,
    evaluation_history: EmoteEvaluationHistory,
    player: EmotePlayer,
    eye_blinks: Vec<EmoteEyeBlink>,
    blink_enabled: bool,
}

struct EmoteEyeBlink {
    control: EmoteEyeControl,
    wait_remaining: f32,
    blink_frame: f32,
    phase: EmoteBlinkPhase,
    random_state: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EmoteBlinkPhase {
    Idle,
    Closing,
    ClosedHold,
    Opening,
}

impl EmotePlayback {
    pub fn new(model: EmoteModel, seed: u32) -> Self {
        let eye_blinks = model
            .eye_controls()
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, control)| {
                let salt = (index as u32 + 1).wrapping_mul(0x9e37_79b9);
                EmoteEyeBlink::new(control, seed ^ salt)
            })
            .collect();
        Self {
            model,
            evaluation_history: Default::default(),
            player: EmotePlayer::default(),
            eye_blinks,
            blink_enabled: true,
        }
    }

    pub fn model(&self) -> &EmoteModel {
        &self.model
    }

    pub fn transform(&self) -> EmoteTransform {
        self.player.transform()
    }

    pub fn command(&mut self, command: EmoteCommand) {
        match command {
            EmoteCommand::SetScale([scale, origin_x, origin_y]) => {
                self.player.set_scale(scale, origin_x, origin_y)
            }
            EmoteCommand::SetCoord([x, y, z, angle]) => self.player.set_coord(x, y, z, angle),
            EmoteCommand::SetVariable {
                label,
                value,
                frames,
                easing,
            } => self.player.set_variable(label, value, frames, easing),
            EmoteCommand::PlayTimeline { label, flags } => {
                self.player.play_model_timeline(&self.model, label, flags)
            }
            EmoteCommand::FadeInTimeline {
                label,
                frames,
                easing,
            } => self.player.fade_in_timeline(label, frames, easing),
            EmoteCommand::FadeOutTimeline {
                label,
                frames,
                easing,
            } => self.player.fade_out_timeline(label, frames, easing),
            EmoteCommand::StopTimeline { label } => self.player.stop_timeline(label),
            EmoteCommand::Pass => self.player.pass(),
            EmoteCommand::Step => self.player.step(),
            EmoteCommand::Skip => self.player.skip(),
        }
        self.player.take_commands().for_each(drop);
    }

    pub fn clear_variable(&mut self, label: &str) {
        self.player.clear_variable(label);
    }

    pub fn set_blink_enabled(&mut self, enabled: bool) -> bool {
        if self.blink_enabled == enabled {
            return false;
        }
        self.blink_enabled = enabled;
        if !enabled {
            for blink in &mut self.eye_blinks {
                blink.cancel();
            }
        }
        !self.eye_blinks.is_empty()
    }

    /// Advances playback by elapsed model frames and reports whether the pose changed.
    pub fn advance(&mut self, frames: f32) -> bool {
        let mut changed = self.player.advance_model(&self.model, frames);
        if self.blink_enabled {
            for blink in &mut self.eye_blinks {
                changed |= blink.advance(frames);
            }
        }
        changed
    }

    pub fn evaluate(&mut self) -> Result<Vec<EmoteDrawItem>> {
        self.evaluate_state(&self.render_state())
    }

    /// Samples the pose before an engine applies any additional variable overrides.
    pub fn render_state(&self) -> EmoteRenderState {
        let mut state = EmoteRenderState {
            // The base motion is the static model graph entry. Timeline
            // playback drives its parameterized layers independently.
            motion_time: 0.0,
            variables: BTreeMap::new(),
        };
        let samples = self.player.active_timeline_samples(&self.model);
        for (timeline, values) in samples.iter().filter(|(state, _)| {
            self.model
                .timelines()
                .get(&state.label)
                .is_some_and(|timeline| !timeline.diff)
        }) {
            for (label, value) in values {
                let current = state.variables.entry(label.clone()).or_insert(0.0);
                *current += (*value - *current) * timeline.weight.clamp(0.0, 1.0);
            }
        }
        for (timeline, values) in samples.iter().filter(|(state, _)| {
            self.model
                .timelines()
                .get(&state.label)
                .is_some_and(|timeline| timeline.diff)
        }) {
            for (label, value) in values {
                *state.variables.entry(label.clone()).or_insert(0.0) +=
                    *value * timeline.weight.clamp(0.0, 1.0);
            }
        }
        for (label, variable) in self.player.variables() {
            state.variables.insert(label.clone(), variable.value);
        }
        for blink in &self.eye_blinks {
            blink.apply(&mut state.variables);
        }

        state
    }

    pub fn evaluate_state(&mut self, state: &EmoteRenderState) -> Result<Vec<EmoteDrawItem>> {
        EmoteMotionEvaluator::new(&self.model)
            .evaluate_base_with_history(state, &mut self.evaluation_history)
    }
}

impl EmoteEyeBlink {
    fn new(control: EmoteEyeControl, seed: u32) -> Self {
        let mut blink = Self {
            control,
            wait_remaining: 0.0,
            blink_frame: 0.0,
            phase: EmoteBlinkPhase::Idle,
            random_state: seed.max(1),
        };
        blink.blink_frame = blink.control.begin_frame;
        blink.schedule_next();
        blink
    }

    fn advance(&mut self, mut frames: f32) -> bool {
        let was_active = !matches!(self.phase, EmoteBlinkPhase::Idle);
        frames = frames.max(0.0);
        // The native EPEyeControl step uses a 40% close, 20% closed hold,
        // and 40% open split (2.5x the nominal speed in each moving phase).
        // Continue through phase boundaries in one call so a long host frame
        // cannot leave the eye stuck half closed.
        for _ in 0..8 {
            if frames <= 0.0 {
                break;
            }
            match self.phase {
                EmoteBlinkPhase::Idle => {
                    if !self.control.blink_enabled
                        || (self.blink_frame - self.control.begin_frame).abs() > f32::EPSILON
                    {
                        break;
                    }
                    if frames < self.wait_remaining {
                        self.wait_remaining -= frames;
                        frames = 0.0;
                    } else {
                        frames -= self.wait_remaining;
                        self.wait_remaining = 0.0;
                        self.phase = EmoteBlinkPhase::Closing;
                    }
                }
                EmoteBlinkPhase::Closing => {
                    let span = (self.control.end_frame - self.control.begin_frame).max(0.0);
                    let speed = span * 2.5 / self.control.blink_frame_count.max(f32::EPSILON);
                    let remaining = ((self.control.end_frame - self.blink_frame) / speed).max(0.0);
                    // 余量比较留出 float 余量：跨步恰好落在边界时必须走完成分支，
                    // 否则 blink_frame 会残留 1e-6 级误差并推迟一个宿主帧才进下一阶段。
                    if frames < remaining && remaining - frames > 1.0e-4 {
                        self.blink_frame += speed * frames;
                        frames = 0.0;
                    } else {
                        self.blink_frame = self.control.end_frame;
                        frames -= remaining;
                        self.wait_remaining = self.control.blink_frame_count / 5.0;
                        self.phase = EmoteBlinkPhase::ClosedHold;
                    }
                }
                EmoteBlinkPhase::ClosedHold => {
                    if frames < self.wait_remaining {
                        self.wait_remaining -= frames;
                        frames = 0.0;
                    } else {
                        frames -= self.wait_remaining;
                        self.wait_remaining = 0.0;
                        self.phase = EmoteBlinkPhase::Opening;
                    }
                }
                EmoteBlinkPhase::Opening => {
                    let span = (self.control.end_frame - self.control.begin_frame).max(0.0);
                    let speed = span * 2.5 / self.control.blink_frame_count.max(f32::EPSILON);
                    let remaining =
                        ((self.blink_frame - self.control.begin_frame) / speed).max(0.0);
                    if frames < remaining && remaining - frames > 1.0e-4 {
                        self.blink_frame -= speed * frames;
                        frames = 0.0;
                    } else {
                        self.blink_frame = self.control.begin_frame;
                        frames -= remaining;
                        self.phase = EmoteBlinkPhase::Idle;
                        self.schedule_next();
                    }
                }
            }
        }
        was_active || !matches!(self.phase, EmoteBlinkPhase::Idle)
    }

    fn cancel(&mut self) {
        if self.phase != EmoteBlinkPhase::Idle {
            self.blink_frame = self.control.begin_frame;
            self.phase = EmoteBlinkPhase::Idle;
            self.schedule_next();
        }
    }

    fn apply(&self, variables: &mut BTreeMap<String, f32>) {
        if matches!(self.phase, EmoteBlinkPhase::Idle) {
            return;
        }
        let base = variables.get(&self.control.label).copied().unwrap_or(0.0);
        let span = self.control.end_frame - self.control.begin_frame;
        if base >= self.control.begin_frame
            && base <= self.control.end_frame
            && span.abs() > f32::EPSILON
        {
            let amount = ((self.blink_frame - self.control.begin_frame) / span).clamp(0.0, 1.0);
            variables.insert(
                self.control.label.clone(),
                base + (self.control.end_frame - base) * amount,
            );
        }
    }

    fn schedule_next(&mut self) {
        self.random_state = self
            .random_state
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        let min = self.control.blink_interval_min.max(0.0);
        let max = self.control.blink_interval_max.max(min);
        let unit = self.random_state as f32 / u32::MAX as f32;
        self.wait_remaining = min + (max - min) * unit;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_eye_control_closes_and_reopens_the_eye() {
        let control = EmoteEyeControl {
            label: "face_eye_open".into(),
            enabled: true,
            blink_enabled: true,
            blink_frame_count: 16.0,
            blink_interval_min: 0.0,
            blink_interval_max: 0.0,
            begin_frame: 0.0,
            end_frame: 10.0,
            edges: vec![[-10.0, 20.0]],
            nodes: vec![vec![10.0, 30.0, 32.0, 34.0, 36.0, 38.0, 40.0, 0.0]],
        };
        let mut blink = EmoteEyeBlink::new(control, 1);
        blink.advance(8.0);
        let mut variables = BTreeMap::from([("face_eye_open".to_string(), 0.0)]);
        blink.apply(&mut variables);
        assert_eq!(variables["face_eye_open"], 10.0);

        blink.advance(8.0);
        let mut variables = BTreeMap::from([("face_eye_open".to_string(), 0.0)]);
        blink.apply(&mut variables);
        assert_eq!(variables["face_eye_open"], 0.0);
    }

    #[test]
    fn automatic_eye_control_hits_exact_closed_value_with_fractional_steps() {
        let control = EmoteEyeControl {
            label: "face_eye_open".into(),
            enabled: true,
            blink_enabled: true,
            blink_frame_count: 16.0,
            blink_interval_min: 0.0,
            blink_interval_max: 0.0,
            begin_frame: 0.0,
            end_frame: 10.0,
            edges: vec![[-10.0, 20.0]],
            nodes: vec![vec![10.0]],
        };
        let mut blink = EmoteEyeBlink::new(control, 1);
        let mut consecutive_closed = 0;
        let mut max_consecutive_closed = 0;
        for _ in 0..20 {
            blink.advance(0.96);
            let mut variables = BTreeMap::from([("face_eye_open".to_string(), 0.0)]);
            blink.apply(&mut variables);
            if variables["face_eye_open"] == 10.0 {
                consecutive_closed += 1;
                max_consecutive_closed = max_consecutive_closed.max(consecutive_closed);
            } else {
                consecutive_closed = 0;
            }
        }
        assert!(
            max_consecutive_closed >= 2,
            "fractional frame steps must render the fully closed eye for more than one frame"
        );
    }

    #[test]
    fn cancelling_a_blink_restores_the_selected_eye_value() {
        let control = EmoteEyeControl {
            label: "eye".into(),
            enabled: true,
            blink_enabled: true,
            blink_frame_count: 16.,
            blink_interval_min: 30.,
            blink_interval_max: 30.,
            begin_frame: 0.,
            end_frame: 10.,
            edges: Vec::new(),
            nodes: Vec::new(),
        };
        let mut blink = EmoteEyeBlink::new(control, 1);
        blink.advance(38.);
        let mut variables = BTreeMap::from([("eye".into(), 3.)]);
        blink.apply(&mut variables);
        assert_eq!(variables["eye"], 10.);

        blink.cancel();
        blink.advance(8.);
        let mut variables = BTreeMap::from([("eye".into(), 3.)]);
        blink.apply(&mut variables);
        assert_eq!(variables["eye"], 3.);
    }
}
