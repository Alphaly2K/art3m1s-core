use art3m1s_emote::{
    EmoteDrawItem, EmoteEyeControl, EmoteModel, EmoteMotionEvaluator, EmotePlayer,
    EmoteRenderState, PsbDocument, PsbResourceData,
};
use asb_interpreter::EmoteLayerCommand;
use glam::{Affine2, Vec2};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use super::CoreRuntime;
use crate::host_files::HostResources;
use crate::render_pipeline::draw::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawMesh, StencilMetadata, TextureId,
    TextureInfo, TextureProvider,
};

#[cfg(feature = "experimental-eluna")]
mod eluna;

pub(super) type SharedEmoteState = Arc<Mutex<EmoteState>>;

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct EmoteProfileStats {
    pub worker_eval_ns: u64,
    pub scene_clone_ns: u64,
    pub draw_build_ns: u64,
    pub mesh_build_ns: u64,
    pub worker_updates: u64,
    pub worker_input_frames: u64,
    pub worker_dropped_scenes: u64,
    pub sprites: u64,
    pub mesh_sprites: u64,
    pub mesh_vertices: u64,
}

impl EmoteProfileStats {
    fn merge(&mut self, other: Self) {
        self.worker_eval_ns = self.worker_eval_ns.saturating_add(other.worker_eval_ns);
        self.scene_clone_ns = self.scene_clone_ns.saturating_add(other.scene_clone_ns);
        self.draw_build_ns = self.draw_build_ns.saturating_add(other.draw_build_ns);
        self.mesh_build_ns = self.mesh_build_ns.saturating_add(other.mesh_build_ns);
        self.worker_updates = self.worker_updates.saturating_add(other.worker_updates);
        self.worker_input_frames = self
            .worker_input_frames
            .saturating_add(other.worker_input_frames);
        self.worker_dropped_scenes = self
            .worker_dropped_scenes
            .saturating_add(other.worker_dropped_scenes);
        self.sprites = self.sprites.saturating_add(other.sprites);
        self.mesh_sprites = self.mesh_sprites.saturating_add(other.mesh_sprites);
        self.mesh_vertices = self.mesh_vertices.saturating_add(other.mesh_vertices);
    }
}

pub(super) struct EmoteState {
    layers: BTreeMap<String, LayerSlots>,
    next_generation: u64,
    backend: EmoteBackend,
    profiling_enabled: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum EmoteBackend {
    #[default]
    Builtin,
    ElunaExperimental,
}

impl EmoteBackend {
    pub(crate) fn from_int(value: i32) -> Self {
        match value {
            1 => Self::ElunaExperimental,
            _ => Self::Builtin,
        }
    }
}

impl Default for EmoteState {
    fn default() -> Self {
        Self {
            layers: BTreeMap::new(),
            next_generation: 0,
            backend: EmoteBackend::Builtin,
            profiling_enabled: false,
        }
    }
}

#[derive(Default)]
struct LayerSlots {
    active: Option<EmoteInstanceSlot>,
    pending: Option<EmoteInstanceSlot>,
    attach_to_scene: bool,
}

enum EmoteInstanceSlot {
    Builtin(EmoteInstance),
    #[cfg(feature = "experimental-eluna")]
    Eluna(eluna::ElunaEmoteInstance),
}

struct EmoteInstance {
    evaluation_history: art3m1s_emote::EmoteEvaluationHistory,
    generation: u64,
    width: u32,
    height: u32,
    model: EmoteModel,
    player: EmotePlayer,
    eye_blinks: Vec<EmoteEyeBlink>,
    textures: BTreeMap<String, EmoteTextureState>,
    texture_source_bytes: usize,
    resources: HostResources,
    #[cfg(any(
        target_os = "android",
        target_os = "ios",
        all(target_os = "macos", target_arch = "aarch64")
    ))]
    astc_encoder: Option<crate::platform::mobile_astc::AstcEncoder>,
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

struct EmoteTextureState {
    name: String,
    width: u32,
    height: u32,
    gpu: Option<(TextureId, TextureInfo)>,
    source: Option<PsbResourceData>,
}

impl EmoteState {
    pub(super) fn set_backend(&mut self, backend: EmoteBackend) -> usize {
        if self.backend == backend {
            return 0;
        }
        self.backend = backend;
        self.clear()
    }

    pub(super) fn profile_memory(&self) -> (usize, u64) {
        let mut instances = 0usize;
        let mut source_bytes = 0u64;
        for slots in self.layers.values() {
            for instance in [&slots.active, &slots.pending].into_iter().flatten() {
                instances += 1;
                source_bytes = source_bytes.saturating_add(instance.source_bytes());
            }
        }
        (instances, source_bytes)
    }

    pub(super) fn set_profile_enabled(&mut self, enabled: bool) {
        self.profiling_enabled = enabled;
        for slots in self.layers.values_mut() {
            for instance in [&mut slots.active, &mut slots.pending]
                .into_iter()
                .flatten()
            {
                instance.set_profile_enabled(enabled);
            }
        }
    }

    pub(super) fn take_profile_stats(&self) -> EmoteProfileStats {
        let mut total = EmoteProfileStats::default();
        for slots in self.layers.values() {
            for instance in [&slots.active, &slots.pending].into_iter().flatten() {
                total.merge(instance.take_profile_stats());
            }
        }
        total
    }

    pub fn create_layer(
        &mut self,
        id: &str,
        files: Vec<(String, Vec<u8>)>,
        width: u32,
        height: u32,
        resources: HostResources,
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
        self.next_generation = self.next_generation.wrapping_add(1);
        let generation = self.next_generation;
        let instance = match self.backend {
            EmoteBackend::Builtin => EmoteInstanceSlot::Builtin(EmoteInstance::new(
                generation, &path, bytes, width, height, resources,
            )?),
            EmoteBackend::ElunaExperimental => {
                #[cfg(feature = "experimental-eluna")]
                {
                    EmoteInstanceSlot::Eluna(eluna::ElunaEmoteInstance::new(
                        generation,
                        &path,
                        &bytes,
                        width,
                        height,
                        self.profiling_enabled,
                    )?)
                }
                #[cfg(not(feature = "experimental-eluna"))]
                {
                    return Err(
                        "Eluna E-Mote backend is unavailable in this core build; enable the experimental-eluna Cargo feature"
                            .to_owned(),
                    );
                }
            }
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

        instance.command(command)
    }

    pub fn advance(&mut self, delta_ms: u64) -> bool {
        let frames = delta_ms as f32 * 60.0 / 1000.0;
        let mut changed = false;
        for slots in self.layers.values_mut() {
            for instance in [&mut slots.active, &mut slots.pending]
                .into_iter()
                .flatten()
            {
                changed |= instance.advance(delta_ms, frames);
            }
        }
        changed
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

    pub fn clear(&mut self) -> usize {
        let count = self.layers.len();
        self.layers.clear();
        count
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

impl EmoteInstanceSlot {
    fn set_profile_enabled(&mut self, enabled: bool) {
        match self {
            Self::Builtin(_) => {}
            #[cfg(feature = "experimental-eluna")]
            Self::Eluna(instance) => instance.set_profile_enabled(enabled),
        }
    }

    fn take_profile_stats(&self) -> EmoteProfileStats {
        match self {
            Self::Builtin(_) => EmoteProfileStats::default(),
            #[cfg(feature = "experimental-eluna")]
            Self::Eluna(instance) => instance.take_profile_stats(),
        }
    }

    #[cfg(test)]
    fn as_builtin(&self) -> &EmoteInstance {
        match self {
            Self::Builtin(instance) => instance,
            #[cfg(feature = "experimental-eluna")]
            Self::Eluna(_) => panic!("expected built-in E-Mote instance"),
        }
    }

    fn source_bytes(&self) -> u64 {
        match self {
            Self::Builtin(instance) => instance.source_bytes(),
            #[cfg(feature = "experimental-eluna")]
            Self::Eluna(instance) => instance.source_bytes(),
        }
    }

    fn command(&mut self, command: EmoteLayerCommand) -> Result<(), String> {
        match self {
            Self::Builtin(instance) => {
                instance.command(command);
                Ok(())
            }
            #[cfg(feature = "experimental-eluna")]
            Self::Eluna(instance) => instance.command(command),
        }
    }

    fn advance(&mut self, _delta_ms: u64, builtin_frames: f32) -> bool {
        match self {
            Self::Builtin(instance) => instance.advance(builtin_frames),
            #[cfg(feature = "experimental-eluna")]
            Self::Eluna(instance) => instance.advance(_delta_ms),
        }
    }

    fn build_commands(
        &mut self,
        provider: &mut dyn TextureProvider,
        retained: &mut HashSet<String>,
    ) -> Result<Vec<DrawCommand>, String> {
        match self {
            Self::Builtin(instance) => instance.build_commands(provider, retained),
            #[cfg(feature = "experimental-eluna")]
            Self::Eluna(instance) => instance.build_commands(provider, retained),
        }
    }
}

impl EmoteInstance {
    fn new(
        generation: u64,
        path: &str,
        bytes: Vec<u8>,
        width: u32,
        height: u32,
        resources: HostResources,
    ) -> Result<Self, String> {
        let document = PsbDocument::from_bytes(bytes)
            .map_err(|error| format!("failed to parse E-Mote model {path}: {error}"))?;
        let mut model = EmoteModel::from_document(document)
            .map_err(|error| format!("failed to load E-Mote model {path}: {error}"))?;
        let (texture_source_bytes, mut texture_data) = model
            .take_texture_data()
            .map_err(|error| format!("failed to detach E-Mote textures {path}: {error}"))?;
        let mut textures = BTreeMap::new();
        for (texture_id, texture) in model.atlas().textures() {
            textures.insert(
                texture_id.clone(),
                EmoteTextureState {
                    name: format!(":emote/{generation}/{texture_id}"),
                    width: texture.width,
                    height: texture.height,
                    gpu: None,
                    source: texture_data.remove(texture_id),
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
        Ok(Self {
            evaluation_history: Default::default(),
            generation,
            width,
            height,
            model,
            player: EmotePlayer::default(),
            eye_blinks,
            textures,
            texture_source_bytes,
            resources,
            #[cfg(any(
                target_os = "android",
                target_os = "ios",
                all(target_os = "macos", target_arch = "aarch64")
            ))]
            astc_encoder: None,
        })
    }

    fn source_bytes(&self) -> u64 {
        let live_sources = self
            .textures
            .values()
            .filter_map(|texture| texture.source.as_ref())
            .map(|source| source.as_bytes().len() as u64)
            .sum::<u64>();
        (self.texture_source_bytes as u64).max(live_sources)
    }

    fn command(&mut self, command: EmoteLayerCommand) {
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
                let compressed = texture.source.as_ref().ok_or_else(|| {
                    format!("E-Mote texture {texture_id} was evicted after source release")
                })?;
                let compressed = compressed.as_bytes();
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
                    if let Ok(cached) = self.resources.read_file(&cache_path)
                        && crate::platform::mobile_astc::astc_4x4_len(texture.width, texture.height)
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
                            .decode_texture_data_rgba8(texture_id, compressed)
                            .map_err(|error| {
                                format!("failed to decode E-Mote texture {texture_id}: {error}")
                            })?;
                        if self.astc_encoder.is_none() {
                            match crate::platform::mobile_astc::AstcEncoder::new() {
                                Ok(encoder) => self.astc_encoder = Some(encoder),
                                Err(error) => {
                                    crate::core_warn!("[E-Mote] ASTC encoder unavailable: {error}");
                                }
                            }
                        }
                        if let Some(encoder) = self.astc_encoder.as_mut() {
                            match encoder.encode_rgba8(texture.width, texture.height, &rgba) {
                                Ok(astc) => {
                                    if let Err(error) = self.resources.write(&cache_path, &astc) {
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
                            .decode_texture_data_rgba8(texture_id, compressed)
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
                        .decode_texture_data_rgba8(texture_id, compressed)
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
                if texture.gpu.is_some() {
                    texture.source = None;
                }
            }
        }
        if self
            .textures
            .values()
            .all(|texture| texture.source.is_none())
        {
            #[cfg(any(
                target_os = "android",
                target_os = "ios",
                all(target_os = "macos", target_arch = "aarch64")
            ))]
            {
                self.astc_encoder = None;
            }
            let released = std::mem::take(&mut self.texture_source_bytes);
            if released != 0 {
                crate::core_info!(
                    "[E-Mote] released {:.1} MiB shared texture source after GPU upload",
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
            .evaluate_base_with_history(&state, &mut self.evaluation_history)
            .map_err(|error| error.to_string())?;
        Ok(items
            .into_iter()
            .filter_map(|item| self.draw_command(item))
            .collect())
    }

    fn draw_command(&self, item: EmoteDrawItem) -> Option<DrawCommand> {
        let texture = self.textures.get(&item.texture_id)?;
        let (texture_id, texture_info) = texture.gpu?;
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
            }),
            native_emote: Some(native_material),
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
    fn advance(&mut self, frames: f32) -> bool {
        let mut changed = self.player.advance_model(&self.model, frames);
        for blink in &mut self.eye_blinks {
            changed |= blink.advance(frames);
        }
        changed
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

impl CoreRuntime {
    pub(crate) fn set_emote_backend(&mut self, backend: EmoteBackend) {
        let cleared = self.emote.lock().unwrap().set_backend(backend);
        self.gpu.begin_access();
        let textures = self.gpu.evict_texture_prefix(":emote/");
        self.gpu.end_access();
        crate::core_info!(
            "[E-Mote] backend={backend:?}; cleared {cleared} layer(s) and {textures} texture(s)"
        );
        self.last_submitted_frame = None;
    }

    pub(super) fn clear_emote_state(&mut self, reason: &str) {
        let layers = self.emote.lock().unwrap().clear();
        self.gpu.begin_access();
        let textures = self.gpu.evict_texture_prefix(":emote/");
        self.gpu.end_access();
        if layers != 0 || textures != 0 {
            crate::core_info!(
                "[E-Mote] cleared {layers} layer(s) and {textures} GPU texture(s): {reason}"
            );
        }
        self.last_submitted_frame = None;
    }

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
        self.emote.lock().unwrap().build_commands(&mut *self.gpu)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use art3m1s_emote::{EmoteEyeControl, EmoteMotionEvaluator, EmoteRenderState};

    use super::{EmoteEyeBlink, EmoteLayerCommand, EmoteState, draw_mesh};
    use crate::compositor::mock::MockProvider;
    use crate::render_pipeline::draw::DrawList;

    fn nekomiko_model_path() -> std::path::PathBuf {
        let root = std::env::var_os("ART3M1S_FIXTURE_NEKOMIKO_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("ART3M1S_FIXTURES_DIR")
                    .map(std::path::PathBuf::from)
                    .map(|base| base.join("nekomiko"))
            })
            .expect(
                "set ART3M1S_FIXTURE_NEKOMIKO_DIR or ART3M1S_FIXTURES_DIR before running ignored compatibility tests",
            );
        root.join("image/fhd/fg/aya/tay_0.psb")
    }

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
    #[ignore = "requires the external nekomiko fixture"]
    fn builds_nekomiko_draw_commands() {
        let path = nekomiko_model_path();
        let bytes = std::fs::read(&path).unwrap();

        let mut state = EmoteState::default();
        assert!(
            !state
                .create_layer(
                    "1.0",
                    vec![(path.display().to_string(), bytes)],
                    1600,
                    1350,
                    crate::host_files::default_resources().clone(),
                )
                .unwrap()
        );
        let instance = state.layers["1.0"].active.as_ref().unwrap().as_builtin();
        assert!(instance.model.source_document().is_none());
        assert!(
            instance
                .textures
                .values()
                .all(|texture| texture.source.is_some())
        );
        {
            let instance = state.layers["1.0"].active.as_ref().unwrap().as_builtin();
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
                .as_builtin()
                .model
                .source_document()
                .is_none()
        );
        assert!(
            state.layers["1.0"]
                .active
                .as_ref()
                .unwrap()
                .as_builtin()
                .textures
                .values()
                .all(|texture| texture.source.is_none())
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
        assert_eq!(state.clear(), 1);
        assert!(state.layers.is_empty());
    }

    #[cfg(feature = "experimental-eluna")]
    #[test]
    #[ignore = "requires the external nekomiko fixture"]
    fn builds_nekomiko_draw_commands_with_eluna() {
        let path = nekomiko_model_path();
        let bytes = std::fs::read(&path).unwrap();
        let mut state = EmoteState::default();
        state.set_backend(super::EmoteBackend::ElunaExperimental);
        assert!(
            !state
                .create_layer(
                    "1.0",
                    vec![(path.display().to_string(), bytes)],
                    1600,
                    1350,
                    crate::host_files::default_resources().clone(),
                )
                .unwrap()
        );
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
        let advance_started = std::time::Instant::now();
        state.advance(16);
        assert!(
            advance_started.elapsed() < std::time::Duration::from_millis(100),
            "Eluna scene evaluation must not block the host frame"
        );
        let mut provider = MockProvider::new();
        let (commands, retained) = state.build_commands(&mut provider);
        assert!(!commands["1.0"].is_empty());
        assert!(!retained.is_empty());
        assert!(commands["1.0"].iter().all(|command| command.mesh.is_some()));
        assert!(
            commands["1.0"]
                .iter()
                .all(|command| command.native_emote.is_some())
        );
    }
}
