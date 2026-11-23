use crate::render_pipeline::draw::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawMesh, StencilMetadata, TextureId,
    TextureInfo,
};
use art3m1s_emote::{
    EmoteDrawItem, EmoteEvaluationHistory, EmoteEyeControl, EmoteModel, EmoteMotionEvaluator,
    EmotePlayer, EmoteRenderState,
};
use asb_interpreter::EmoteLayerCommand;
use glam::{Affine2, Vec2};
use std::collections::BTreeMap;

pub(crate) struct EmotePlayback {
    pub(crate) model: EmoteModel,
    evaluation_history: EmoteEvaluationHistory,
    generation: u64,
    width: u32,
    height: u32,
    player: EmotePlayer,
    eye_blinks: Vec<EmoteEyeBlink>,
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
    pub(crate) fn new(model: EmoteModel, generation: u64, width: u32, height: u32) -> Self {
        let eye_blinks = model
            .eye_controls()
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, control)| {
                let salt = (index as u32 + 1).wrapping_mul(0x9e37_79b9);
                EmoteEyeBlink::new(control, generation as u32 ^ salt)
            })
            .collect();
        Self {
            model,
            evaluation_history: Default::default(),
            generation,
            width,
            height,
            player: EmotePlayer::default(),
            eye_blinks,
        }
    }

    pub(crate) fn command(&mut self, command: EmoteLayerCommand) {
        match command {
            EmoteLayerCommand::SetScale {
                scale,
                origin_x,
                origin_y,
            } => self.player.set_scale(scale, origin_x, origin_y),
            EmoteLayerCommand::SetCoord { x, y, z, angle } => self.player.set_coord(x, y, z, angle),
            EmoteLayerCommand::SetVariable {
                label,
                value,
                frames,
                easing,
            } => self.player.set_variable(label, value, frames, easing),
            EmoteLayerCommand::PlayTimeline { label, flags } => {
                self.player.play_model_timeline(&self.model, label, flags)
            }
            EmoteLayerCommand::FadeInTimeline {
                label,
                frames,
                easing,
            } => self.player.fade_in_timeline(label, frames, easing),
            EmoteLayerCommand::FadeOutTimeline {
                label,
                frames,
                easing,
            } => self.player.fade_out_timeline(label, frames, easing),
            EmoteLayerCommand::StopTimeline { label } => self.player.stop_timeline(label),
            EmoteLayerCommand::Pass => self.player.pass(),
            EmoteLayerCommand::Step => self.player.step(),
            EmoteLayerCommand::Skip => self.player.skip(),
        }
        self.player.take_commands().for_each(drop);
    }

    #[cfg(feature = "katze-model-host")]
    pub(crate) fn clear_variable(&mut self, label: &str) {
        self.player.clear_variable(label);
    }

    pub(crate) fn advance(&mut self, frames: f32) -> bool {
        let mut changed = self.player.advance_model(&self.model, frames);
        for blink in &mut self.eye_blinks {
            changed |= blink.advance(frames);
        }
        changed
    }

    pub(crate) fn evaluate(&mut self) -> Result<Vec<EmoteDrawItem>, String> {
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

        EmoteMotionEvaluator::new(&self.model)
            .evaluate_base_with_history(&state, &mut self.evaluation_history)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn draw_command(
        &self,
        item: EmoteDrawItem,
        texture_id: TextureId,
        texture_info: TextureInfo,
    ) -> Option<DrawCommand> {
        let native_material = native_emote_material(&item, texture_info);
        let [atlas_x, atlas_y, width, height] = item.atlas_rect;
        if width <= 0.0 || height <= 0.0 {
            return None;
        }

        let transform = self.player.transform();
        let scale = transform.scale[0];
        let coord = transform.coord;
        let model_origin = Vec2::new(self.width as f32 * 0.5, self.height as f32 * 0.5);
        let layer_transform =
            Affine2::from_translation(model_origin + Vec2::new(coord[0], coord[1]))
                * Affine2::from_angle(coord[3].to_radians())
                * Affine2::from_scale(Vec2::splat(scale))
                * Affine2::from_translation(Vec2::new(-transform.scale[1], -transform.scale[2]));
        // The evaluator resolves the complete E-Mote layer chain (including
        // zoom, shear, flips and coordinate-plane inheritance). Keep the
        // public player transform as the outer operation and consume that
        // affine directly instead of rebuilding a translation+angle
        // approximation here.
        let m = item.world_transform;
        let sprite_transform = Affine2::from_cols_array(&[m[0], m[2], m[1], m[3], m[4], m[5]])
            * Affine2::from_translation(Vec2::new(
                -item.origin[0] - item.frame_offset[0],
                -item.origin[1] - item.frame_offset[1],
            ));

        Some(DrawCommand {
            texture: texture_id,
            size: texture_info,
            transform: layer_transform * sprite_transform,
            // E-Mote's corner colors, MODULATE2X and low-nibble blend modes
            // are evaluated by the native-emote shader. Keep opacity in the
            // material alpha so it is applied exactly once for both quad and
            // tessellated mesh paths.
            opacity: 1.0,
            blend: emote_blend(item.blend_mode),
            color: ColorFilter {
                multiply: [1.0, 1.0, 1.0],
                grayscale: false,
                negative: false,
            },
            clip: ClipRect {
                uv_offset: [
                    atlas_x / texture_info.width as f32,
                    atlas_y / texture_info.height as f32,
                ],
                uv_scale: [
                    width / texture_info.width as f32,
                    height / texture_info.height as f32,
                ],
                quad_size: [width, height],
            },
            clip_bounds: Some([0.0, 0.0, self.width as f32, self.height as f32]),
            shader: None,
            mesh: item
                .mesh
                .as_ref()
                .and_then(|mesh| draw_mesh(mesh.blend_points.as_deref(), width, height)),
            stencil: Some(StencilMetadata {
                namespace: self.generation,
                source_label: item.layer_label,
                mask_labels: item.stencil_mask_layers,
                inverted: item.stencil_mask_inverted,
            }),
            native_emote: Some(native_material),
        })
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

fn color_component(color: &[f32], index: usize) -> f32 {
    let value = color.get(index).copied().unwrap_or(255.0);
    if value > 1.0 {
        (value / 255.0).clamp(0.0, 1.0)
    } else {
        value.clamp(0.0, 1.0)
    }
}

fn emote_blend(value: i64) -> BlendMode {
    match value & 0x0f {
        1 => BlendMode::NativeAdd,
        2 | 5 => BlendMode::NativeReverseSubtract,
        3 => BlendMode::NativeMultiply,
        4 => BlendMode::NativeScreen,
        _ => BlendMode::Alpha,
    }
}

fn native_emote_material(
    item: &EmoteDrawItem,
    texture_info: TextureInfo,
) -> crate::render_pipeline::draw::NativeEmoteMaterial {
    // `color` is either four byte channels decoded from a packed scalar or a
    // list of packed 0xRRGGBBAA corner colors. The latter is distinguished by
    // values wider than one byte, matching the independent emoteplayer parser.
    let corners = if item.color.len() >= 4 && item.color.iter().take(4).any(|value| *value > 255.0)
    {
        item.color
            .iter()
            .take(4)
            .map(|value| packed_color(*value as u32, item.opacity))
            .collect::<Vec<_>>()
    } else {
        let rgba = [
            color_component(&item.color, 0),
            color_component(&item.color, 1),
            color_component(&item.color, 2),
            color_component(&item.color, 3) * item.opacity,
        ];
        vec![rgba; 4]
    };
    let mut corner_colors = [
        [1.0, 1.0, 1.0, item.opacity],
        [1.0, 1.0, 1.0, item.opacity],
        [1.0, 1.0, 1.0, item.opacity],
        [1.0, 1.0, 1.0, item.opacity],
    ];
    for (dst, src) in corner_colors.iter_mut().zip(corners.into_iter()) {
        *dst = src;
    }
    crate::render_pipeline::draw::NativeEmoteMaterial {
        corner_colors,
        uv_rect: [
            item.atlas_rect[0] / texture_info.width as f32,
            item.atlas_rect[1] / texture_info.height as f32,
            (item.atlas_rect[0] + item.atlas_rect[2]) / texture_info.width as f32,
            (item.atlas_rect[1] + item.atlas_rect[3]) / texture_info.height as f32,
        ],
        blend_mode: item.blend_mode as u32,
        clip_rect: [-1.0e30, -1.0e30, 1.0e30, 1.0e30],
        wipe: [0.0, 0.0, 0.0],
    }
}

fn packed_color(value: u32, opacity: f32) -> [f32; 4] {
    [
        ((value >> 24) & 0xff) as f32 / 255.0,
        ((value >> 16) & 0xff) as f32 / 255.0,
        ((value >> 8) & 0xff) as f32 / 255.0,
        ((value & 0xff) as f32 / 255.0 * opacity).clamp(0.0, 1.0),
    ]
}

fn draw_mesh(points: Option<&[f32]>, width: f32, height: f32) -> Option<DrawMesh> {
    let points = points?;
    if points.is_empty() || !points.len().is_multiple_of(2) {
        return None;
    }
    let point_count = points.len() / 2;
    let side = (point_count as f32).sqrt() as usize;
    if side < 2 || side * side != point_count {
        return None;
    }

    let vertex = |x: usize, y: usize| {
        let index = (y * side + x) * 2;
        [
            points[index] * width,
            points[index + 1] * height,
            x as f32 / (side - 1) as f32,
            y as f32 / (side - 1) as f32,
        ]
    };
    let mut vertices = Vec::with_capacity((side - 1) * (side - 1) * 6);
    for y in 0..side - 1 {
        for x in 0..side - 1 {
            let top_left = vertex(x, y);
            let top_right = vertex(x + 1, y);
            let bottom_left = vertex(x, y + 1);
            let bottom_right = vertex(x + 1, y + 1);
            vertices.extend_from_slice(&[
                top_left,
                top_right,
                bottom_right,
                top_left,
                bottom_right,
                bottom_left,
            ]);
        }
    }
    Some(DrawMesh {
        vertices: vertices.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_four_by_four_blend_points_into_nine_quads() {
        let mut points = Vec::new();
        for y in 0..4 {
            for x in 0..4 {
                points.push(x as f32 / 3.0);
                points.push(y as f32 / 3.0);
            }
        }
        let mesh = draw_mesh(Some(&points), 300.0, 600.0).unwrap();
        assert_eq!(mesh.vertices.len(), 54);
        assert_eq!(mesh.vertices[0], [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(mesh.vertices[2], [100.0, 200.0, 1.0 / 3.0, 1.0 / 3.0]);
    }

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
}
