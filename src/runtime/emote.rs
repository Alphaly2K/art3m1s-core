use art3m1s_emote::{
    EmoteDrawItem, EmoteEyeControl, EmoteModel, EmoteMotionEvaluator, EmotePlayer,
    EmoteRenderState, PsbDocument,
};
use asb_interpreter::EmoteLayerCommand;
use glam::{Affine2, Vec2};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use super::CoreRuntime;
use crate::render_pipeline::draw::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawMesh, StencilMetadata, TextureId,
    TextureInfo, TextureProvider,
};

pub(super) type SharedEmoteState = Arc<Mutex<EmoteState>>;

#[derive(Default)]
pub(super) struct EmoteState {
    layers: BTreeMap<String, LayerSlots>,
    next_generation: u64,
}

#[derive(Default)]
struct LayerSlots {
    active: Option<EmoteInstance>,
    pending: Option<EmoteInstance>,
    attach_to_scene: bool,
}

struct EmoteInstance {
    generation: u64,
    width: u32,
    height: u32,
    model: EmoteModel,
    player: EmotePlayer,
    eye_blinks: Vec<EmoteEyeBlink>,
    textures: BTreeMap<String, EmoteTextureState>,
    #[cfg(any(
        target_os = "android",
        target_os = "ios",
        all(target_os = "macos", target_arch = "aarch64")
    ))]
    astc_encoder: Option<crate::mobile_astc::AstcEncoder>,
}

struct EmoteEyeBlink {
    control: EmoteEyeControl,
    wait_remaining: f32,
    blink_position: Option<f32>,
    random_state: u32,
}

struct EmoteTextureState {
    name: String,
    width: u32,
    height: u32,
    gpu: Option<(TextureId, TextureInfo)>,
}

impl EmoteState {
    pub fn create_layer(
        &mut self,
        id: &str,
        files: Vec<(String, Vec<u8>)>,
        width: u32,
        height: u32,
    ) -> Result<bool, String> {
        if files.len() != 1 {
            return Err(format!(
                "E-Mote layer {id} requires exactly one embedded-texture PSB, got {}",
                files.len()
            ));
        }
        let (path, bytes) = files
            .into_iter()
            .next()
            .ok_or_else(|| format!("E-Mote layer {id} has no model file"))?;
        let document = PsbDocument::from_bytes(bytes)
            .map_err(|error| format!("failed to parse E-Mote model {path}: {error}"))?;
        let model = EmoteModel::from_document(document)
            .map_err(|error| format!("failed to load E-Mote model {path}: {error}"))?;

        self.next_generation = self.next_generation.wrapping_add(1);
        let generation = self.next_generation;
        let mut textures = BTreeMap::new();
        for (texture_id, texture) in model.atlas().textures() {
            textures.insert(
                texture_id.clone(),
                EmoteTextureState {
                    name: format!(":emote/{generation}/{texture_id}"),
                    width: texture.width,
                    height: texture.height,
                    gpu: None,
                },
            );
        }

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
        let instance = EmoteInstance {
            generation,
            width,
            height,
            model,
            player: EmotePlayer::default(),
            eye_blinks,
            textures,
            #[cfg(any(
                target_os = "android",
                target_os = "ios",
                all(target_os = "macos", target_arch = "aarch64")
            ))]
            astc_encoder: None,
        };
        let slots = self.layers.entry(id.to_string()).or_default();
        slots.attach_to_scene = true;
        if slots.active.is_none() {
            slots.active = Some(instance);
            Ok(false)
        } else {
            slots.pending = Some(instance);
            Ok(true)
        }
    }

    pub fn get_layer(&mut self, id: &str, next: bool) -> Option<bool> {
        let slots = self.layers.get_mut(id)?;
        if next && slots.pending.is_some() {
            slots.active = slots.pending.take();
        }
        if slots.active.is_some() {
            Some(false)
        } else if slots.pending.is_some() {
            Some(true)
        } else {
            None
        }
    }

    pub fn command(
        &mut self,
        id: &str,
        next: bool,
        command: EmoteLayerCommand,
    ) -> Result<(), String> {
        let slots = self
            .layers
            .get_mut(id)
            .ok_or_else(|| format!("unknown E-Mote layer {id}"))?;
        let instance = if next {
            slots.pending.as_mut()
        } else {
            slots.active.as_mut()
        }
        .ok_or_else(|| {
            format!(
                "E-Mote layer {id} has no {} instance",
                if next { "pending" } else { "active" }
            )
        })?;

        match command {
            EmoteLayerCommand::SetScale {
                scale,
                origin_x,
                origin_y,
            } => instance.player.set_scale(scale, origin_x, origin_y),
            EmoteLayerCommand::SetCoord { x, y, z, angle } => {
                instance.player.set_coord(x, y, z, angle)
            }
            EmoteLayerCommand::SetVariable {
                label,
                value,
                frames,
                easing,
            } => instance.player.set_variable(label, value, frames, easing),
            EmoteLayerCommand::PlayTimeline { label, flags } => instance
                .player
                .play_model_timeline(&instance.model, label, flags),
            EmoteLayerCommand::FadeInTimeline {
                label,
                frames,
                easing,
            } => instance.player.fade_in_timeline(label, frames, easing),
            EmoteLayerCommand::FadeOutTimeline {
                label,
                frames,
                easing,
            } => instance.player.fade_out_timeline(label, frames, easing),
            EmoteLayerCommand::StopTimeline { label } => instance.player.stop_timeline(label),
            EmoteLayerCommand::Pass => instance.player.pass(),
            EmoteLayerCommand::Step => instance.player.step(),
            EmoteLayerCommand::Skip => instance.player.skip(),
        }
        instance.player.take_commands().for_each(drop);
        Ok(())
    }

    pub fn advance(&mut self, delta_ms: u64) {
        let frames = delta_ms as f32 * 60.0 / 1000.0;
        for slots in self.layers.values_mut() {
            for instance in [&mut slots.active, &mut slots.pending]
                .into_iter()
                .flatten()
            {
                instance.advance(frames);
            }
        }
    }

    pub fn take_scene_attachments(&mut self) -> Vec<String> {
        self.layers
            .iter_mut()
            .filter_map(|(id, slots)| {
                std::mem::take(&mut slots.attach_to_scene).then(|| id.clone())
            })
            .collect()
    }

    pub fn retain_scene_layers(&mut self, scene_ids: &HashSet<String>) {
        self.layers.retain(|id, _| scene_ids.contains(id));
    }

    pub fn clear(&mut self) {
        self.layers.clear();
    }

    pub fn build_commands(
        &mut self,
        provider: &mut dyn TextureProvider,
    ) -> (HashMap<String, Vec<DrawCommand>>, HashSet<String>) {
        let mut commands = HashMap::new();
        let mut retained = HashSet::new();
        for (layer_id, slots) in &mut self.layers {
            let Some(instance) = slots.pending.as_mut().or(slots.active.as_mut()) else {
                continue;
            };
            match instance.build_commands(provider, &mut retained) {
                Ok(draws) if !draws.is_empty() => {
                    commands.insert(layer_id.clone(), draws);
                }
                Ok(_) => {}
                Err(error) => {
                    crate::core_debug!("[E-Mote] layer {layer_id} render failed: {error}");
                }
            }
        }
        (commands, retained)
    }
}

impl EmoteInstance {
    fn build_commands(
        &mut self,
        provider: &mut dyn TextureProvider,
        retained: &mut HashSet<String>,
    ) -> Result<Vec<DrawCommand>, String> {
        for (texture_id, texture) in &mut self.textures {
            retained.insert(texture.name.clone());
            if texture.gpu.is_none() {
                let document = self.model.source_document().ok_or_else(|| {
                    format!("E-Mote texture {texture_id} was evicted after source release")
                })?;
                let compressed = self
                    .model
                    .atlas()
                    .compressed_texture(document, texture_id)
                    .map_err(|error| {
                        format!("failed to read E-Mote texture {texture_id}: {error}")
                    })?;
                texture.gpu = provider.upload_dxt5_render_only(
                    &texture.name,
                    texture.width,
                    texture.height,
                    compressed,
                );

                #[cfg(any(
                    target_os = "android",
                    target_os = "ios",
                    all(target_os = "macos", target_arch = "aarch64")
                ))]
                let mut decoded_rgba = None;
                #[cfg(any(
                    target_os = "android",
                    target_os = "ios",
                    all(target_os = "macos", target_arch = "aarch64")
                ))]
                if texture.gpu.is_none() && provider.supports_astc_4x4() {
                    let cache_path = astc_cache_path(compressed, texture.width, texture.height);
                    if let Ok(cached) = crate::ffi::request_file(&cache_path)
                        && crate::mobile_astc::astc_4x4_len(texture.width, texture.height)
                            == Some(cached.len())
                    {
                        texture.gpu = provider.upload_astc_4x4_render_only(
                            &texture.name,
                            texture.width,
                            texture.height,
                            &cached,
                        );
                    }
                    if texture.gpu.is_none() {
                        let rgba = self
                            .model
                            .atlas()
                            .decode_texture_rgba8(document, texture_id)
                            .map_err(|error| {
                                format!("failed to decode E-Mote texture {texture_id}: {error}")
                            })?;
                        if self.astc_encoder.is_none() {
                            match crate::mobile_astc::AstcEncoder::new() {
                                Ok(encoder) => self.astc_encoder = Some(encoder),
                                Err(error) => {
                                    crate::core_warn!("[E-Mote] ASTC encoder unavailable: {error}");
                                }
                            }
                        }
                        if let Some(encoder) = self.astc_encoder.as_mut() {
                            match encoder.encode_rgba8(texture.width, texture.height, &rgba) {
                                Ok(astc) => {
                                    if let Err(error) =
                                        crate::ffi::request_write(&cache_path, &astc)
                                    {
                                        crate::core_debug!(
                                            "[E-Mote] ASTC cache write failed {cache_path}: {error}"
                                        );
                                    }
                                    texture.gpu = provider.upload_astc_4x4_render_only(
                                        &texture.name,
                                        texture.width,
                                        texture.height,
                                        &astc,
                                    );
                                }
                                Err(error) => {
                                    crate::core_warn!(
                                        "[E-Mote] ASTC encode failed for {texture_id}: {error}"
                                    );
                                }
                            }
                        }
                        decoded_rgba = Some(rgba);
                    }
                }
                if texture.gpu.is_none() {
                    #[cfg(any(
                        target_os = "android",
                        target_os = "ios",
                        all(target_os = "macos", target_arch = "aarch64")
                    ))]
                    let rgba = if let Some(rgba) = decoded_rgba.take() {
                        rgba
                    } else {
                        self.model
                            .atlas()
                            .decode_texture_rgba8(document, texture_id)
                            .map_err(|error| {
                                format!("failed to decode E-Mote texture {texture_id}: {error}")
                            })?
                    };
                    #[cfg(not(any(
                        target_os = "android",
                        target_os = "ios",
                        all(target_os = "macos", target_arch = "aarch64")
                    )))]
                    let rgba = self
                        .model
                        .atlas()
                        .decode_texture_rgba8(document, texture_id)
                        .map_err(|error| {
                            format!("failed to decode E-Mote texture {texture_id}: {error}")
                        })?;
                    texture.gpu = provider.upload_rgba_render_only(
                        &texture.name,
                        texture.width,
                        texture.height,
                        &rgba,
                    );
                }
            }
        }
        if self.textures.values().all(|texture| texture.gpu.is_some()) {
            #[cfg(any(
                target_os = "android",
                target_os = "ios",
                all(target_os = "macos", target_arch = "aarch64")
            ))]
            {
                self.astc_encoder = None;
            }
            let released = self.model.release_source_document();
            if released != 0 {
                crate::core_info!(
                    "[E-Mote] released {:.1} MiB source document after GPU upload",
                    released as f64 / (1024.0 * 1024.0)
                );
            }
        }

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

        let items = EmoteMotionEvaluator::new(&self.model)
            .evaluate_base(&state)
            .map_err(|error| error.to_string())?;
        Ok(items
            .into_iter()
            .filter_map(|item| self.draw_command(item))
            .collect())
    }

    fn draw_command(&self, item: EmoteDrawItem) -> Option<DrawCommand> {
        let texture = self.textures.get(&item.texture_id)?;
        let (texture_id, texture_info) = texture.gpu?;
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
        let sprite_transform =
            Affine2::from_translation(Vec2::new(item.translation[0], item.translation[1]))
                * Affine2::from_angle(item.angle.to_radians())
                * Affine2::from_translation(Vec2::new(-item.origin[0], -item.origin[1]));

        let alpha = color_component(&item.color, 3);
        Some(DrawCommand {
            texture: texture_id,
            size: texture_info,
            transform: layer_transform * sprite_transform,
            opacity: item.opacity * alpha,
            blend: emote_blend(item.blend_mode),
            color: ColorFilter {
                multiply: [
                    color_component(&item.color, 0),
                    color_component(&item.color, 1),
                    color_component(&item.color, 2),
                ],
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
            }),
        })
    }
}

#[cfg(any(
    target_os = "android",
    target_os = "ios",
    all(target_os = "macos", target_arch = "aarch64")
))]
fn astc_cache_path(compressed: &[u8], width: u32, height: u32) -> String {
    let hash = compressed
        .iter()
        .fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x1000_0000_01b3)
        });
    format!("cache/emote/astc4x4/{hash:016x}-{width}x{height}.bin")
}

impl EmoteInstance {
    fn advance(&mut self, frames: f32) {
        self.player.advance(frames);
        for blink in &mut self.eye_blinks {
            blink.advance(frames);
        }
    }
}

impl EmoteEyeBlink {
    fn new(control: EmoteEyeControl, seed: u32) -> Self {
        let mut blink = Self {
            control,
            wait_remaining: 0.0,
            blink_position: None,
            random_state: seed.max(1),
        };
        blink.schedule_next();
        blink
    }

    fn advance(&mut self, mut frames: f32) {
        frames = frames.max(0.0);
        while frames > 0.0 {
            if let Some(position) = self.blink_position {
                let remaining = (self.control.blink_frame_count - position).max(0.0);
                if frames < remaining {
                    let next = position + frames;
                    let closed = self.control.blink_frame_count * 0.5;
                    // Runtime frame deltas rarely land exactly on the blink midpoint
                    // (for example 0.96 E-Mote frames at 60 Hz). The model only hides
                    // eye/white layers at the exact closed value, so preserve that peak
                    // for one rendered frame when a step crosses it.
                    self.blink_position = Some(if position < closed && next > closed {
                        closed
                    } else {
                        next
                    });
                    break;
                }
                frames -= remaining;
                self.blink_position = None;
                self.schedule_next();
            } else if frames < self.wait_remaining {
                self.wait_remaining -= frames;
                break;
            } else {
                frames -= self.wait_remaining;
                self.wait_remaining = 0.0;
                self.blink_position = Some(0.0);
            }
        }
    }

    fn apply(&self, variables: &mut BTreeMap<String, f32>) {
        let Some(position) = self.blink_position else {
            return;
        };
        let base = variables.get(&self.control.label).copied().unwrap_or(0.0);
        let phase = (position / self.control.blink_frame_count).clamp(0.0, 1.0);
        if let Some(value) = self.control.blink_value(base, phase) {
            variables.insert(self.control.label.clone(), value);
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
    match value {
        1 => BlendMode::Add,
        2 => BlendMode::Multiply,
        3 => BlendMode::Screen,
        _ => BlendMode::Alpha,
    }
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
    Some(DrawMesh { vertices })
}

impl CoreRuntime {
    pub(super) fn sync_emote_scene(&mut self) {
        let attachments = self.emote.lock().unwrap().take_scene_attachments();
        for id in attachments {
            self.compositor.ensure_layer(&id);
        }
        let scene_ids = self
            .compositor
            .scene()
            .iter_ids()
            .into_iter()
            .collect::<HashSet<_>>();
        self.emote.lock().unwrap().retain_scene_layers(&scene_ids);
    }

    pub(super) fn build_emote_commands(
        &mut self,
    ) -> (HashMap<String, Vec<DrawCommand>>, HashSet<String>) {
        self.emote
            .lock()
            .unwrap()
            .build_commands(&mut self.texture_provider)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use art3m1s_emote::{EmoteEyeControl, EmoteMotionEvaluator, EmoteRenderState};

    use super::{EmoteEyeBlink, EmoteLayerCommand, EmoteState, draw_mesh};
    use crate::compositor::mock::MockProvider;
    use crate::render_pipeline::draw::DrawList;

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

    #[test]
    fn builds_nekomiko_draw_commands_when_fixture_is_available() {
        let Ok(root) = std::env::var("NEKOMIKO_DIR") else {
            return;
        };
        let path = std::path::Path::new(&root).join("image/fhd/fg/aya/tay_0.psb");
        let bytes = std::fs::read(&path).unwrap();

        let mut state = EmoteState::default();
        assert!(
            !state
                .create_layer("1.0", vec![(path.display().to_string(), bytes)], 1600, 1350,)
                .unwrap()
        );
        {
            let instance = state.layers["1.0"].active.as_ref().unwrap();
            let items = EmoteMotionEvaluator::new(&instance.model)
                .evaluate_base(&EmoteRenderState {
                    motion_time: 0.0,
                    variables: BTreeMap::from([("face_eye_open".to_string(), 5.0)]),
                })
                .unwrap();
            let position = |label: &str| {
                items
                    .iter()
                    .position(|item| item.layer_label == label)
                    .unwrap_or_else(|| panic!("missing E-Mote eye layer {label}"))
            };
            assert!(position("eye_L") < position("mabuta"));
            assert!(position("shirome") < position("mabuta"));
        }
        state
            .command(
                "1.0",
                false,
                EmoteLayerCommand::SetScale {
                    scale: 0.6,
                    origin_x: 0.0,
                    origin_y: 0.0,
                },
            )
            .unwrap();
        state
            .command(
                "1.0",
                false,
                EmoteLayerCommand::PlayTimeline {
                    label: "笑顔_ボイス再生用".to_string(),
                    flags: 1,
                },
            )
            .unwrap();
        state.advance(16);
        state.advance(3_000);

        let mut provider = MockProvider::new();
        let (commands, retained) = state.build_commands(&mut provider);
        assert!(!commands["1.0"].is_empty());
        assert_eq!(retained.len(), 8);
        assert!(
            state.layers["1.0"]
                .active
                .as_ref()
                .unwrap()
                .model
                .source_document()
                .is_none()
        );
        assert!(commands["1.0"].iter().any(|command| command.mesh.is_some()));
        let mut frame = DrawList {
            commands: commands["1.0"].clone(),
            ..DrawList::default()
        };
        frame.materialize_stencil_groups(crate::render_pipeline::shader::ALPHA_MASK_SHADER);
        assert!(!frame.mask_commands.is_empty());
        assert!(
            frame
                .shader_groups
                .iter()
                .any(|group| group.mask_range.is_some())
        );
    }
}
