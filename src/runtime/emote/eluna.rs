use asb_interpreter::EmoteLayerCommand;
use eluna::{
    EmoteDrawPass, EmoteLoadOptions, EmotePlayerControl, EmoteRuntime, EmoteStaticScene,
    EmoteStaticSprite, TimelinePlayMode,
};
use glam::{Affine2, Vec2};
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

#[path = "eluna_mesh.rs"]
mod mesh;

use super::EmoteProfileStats;
use crate::render_pipeline::draw::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawMesh, NativeEmoteMaterial, StencilMetadata,
    TextureId, TextureInfo, TextureProvider,
};

pub(super) struct ElunaEmoteInstance {
    generation: u64,
    width: u32,
    height: u32,
    worker: ElunaWorker,
    scene: Arc<EmoteStaticScene>,
    transform: ElunaLayerTransform,
    textures: BTreeMap<u32, ElunaTextureState>,
    source_bytes: u64,
    cached_scene: Option<Arc<EmoteStaticScene>>,
    cached_transform: Option<Affine2>,
    cached_commands: Vec<DrawCommand>,
    profile: Arc<ElunaProfileCounters>,
}

#[derive(Clone, Copy)]
struct ElunaLayerTransform {
    scale: f32,
    origin: [f32; 2],
    coord: [f32; 4],
}

impl Default for ElunaLayerTransform {
    fn default() -> Self {
        Self {
            scale: 1.0,
            origin: [0.0, 0.0],
            coord: [0.0; 4],
        }
    }
}

struct ElunaTextureState {
    name: String,
    width: u32,
    height: u32,
    gpu: Option<(TextureId, TextureInfo)>,
    data: Option<Arc<[u8]>>,
}

struct ElunaWorker {
    message_tx: mpsc::Sender<ElunaWorkerMessage>,
    latest_scene: Arc<Mutex<Option<Arc<EmoteStaticScene>>>>,
}

#[derive(Default)]
struct ElunaProfileCounters {
    enabled: AtomicBool,
    worker_eval_ns: AtomicU64,
    scene_clone_ns: AtomicU64,
    draw_build_ns: AtomicU64,
    mesh_build_ns: AtomicU64,
    worker_updates: AtomicU64,
    worker_input_frames: AtomicU64,
    worker_dropped_scenes: AtomicU64,
    sprites: AtomicU64,
    mesh_sprites: AtomicU64,
    mesh_vertices: AtomicU64,
}

impl ElunaProfileCounters {
    fn set_enabled(&self, enabled: bool) {
        self.reset();
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    fn reset(&self) {
        let _ = self.take();
    }

    fn take(&self) -> EmoteProfileStats {
        EmoteProfileStats {
            worker_eval_ns: self.worker_eval_ns.swap(0, Ordering::Relaxed),
            scene_clone_ns: self.scene_clone_ns.swap(0, Ordering::Relaxed),
            draw_build_ns: self.draw_build_ns.swap(0, Ordering::Relaxed),
            mesh_build_ns: self.mesh_build_ns.swap(0, Ordering::Relaxed),
            worker_updates: self.worker_updates.swap(0, Ordering::Relaxed),
            worker_input_frames: self.worker_input_frames.swap(0, Ordering::Relaxed),
            worker_dropped_scenes: self.worker_dropped_scenes.swap(0, Ordering::Relaxed),
            sprites: self.sprites.load(Ordering::Relaxed),
            mesh_sprites: self.mesh_sprites.load(Ordering::Relaxed),
            mesh_vertices: self.mesh_vertices.load(Ordering::Relaxed),
        }
    }
}

enum ElunaWorkerMessage {
    Command(EmoteLayerCommand),
    Advance(u64),
}

impl ElunaEmoteInstance {
    pub(super) fn new(
        generation: u64,
        path: &str,
        bytes: &[u8],
        width: u32,
        height: u32,
        profiling_enabled: bool,
    ) -> Result<Self, String> {
        let options = EmoteLoadOptions {
            autoplay_timeline: false,
            ..EmoteLoadOptions::default()
        };
        let mut runtime = match EmoteRuntime::from_bytes(bytes, options.clone()) {
            Ok(runtime) => runtime,
            Err(first_error) => {
                let Some(key) = infer_emote_header_key(bytes) else {
                    return Err(format!(
                        "Eluna failed to load E-Mote model {path}: {first_error}"
                    ));
                };
                EmoteRuntime::from_bytes(bytes, options.with_emote_key(key)).map_err(|error| {
                    format!(
                        "Eluna failed to load encrypted E-Mote model {path} with inferred key {key:#010x}: {error}"
                    )
                })?
            }
        };

        // A portable Eluna physics tick rebuilds the complete PSB scene twice.
        // Keep that work away from the host render thread; physics stays off on
        // this experimental path until Eluna provides incremental evaluation.
        runtime.set_physics_enabled(false);
        let mut source_bytes = 0u64;
        let textures = runtime
            .texture_sources()
            .values()
            .map(|source| {
                let data: Arc<[u8]> = runtime
                    .texture_bytes(source.resource_index)
                    .unwrap_or_default()
                    .into();
                source_bytes = source_bytes.saturating_add(data.len() as u64);
                (
                    source.resource_index,
                    ElunaTextureState {
                        name: format!(":emote/eluna/{generation}/{}", source.name),
                        width: source.width,
                        height: source.height,
                        gpu: None,
                        data: Some(data),
                    },
                )
            })
            .collect();
        let scene = runtime.shared_scene();
        let profile = Arc::new(ElunaProfileCounters::default());
        profile.set_enabled(profiling_enabled);
        let worker = ElunaWorker::spawn(runtime, path, scene.clone(), Arc::clone(&profile))?;

        Ok(Self {
            generation,
            width,
            height,
            worker,
            scene,
            transform: ElunaLayerTransform::default(),
            textures,
            source_bytes,
            cached_scene: None,
            cached_transform: None,
            cached_commands: Vec::new(),
            profile,
        })
    }

    pub(super) fn set_profile_enabled(&self, enabled: bool) {
        self.profile.set_enabled(enabled);
    }

    pub(super) fn take_profile_stats(&self) -> EmoteProfileStats {
        self.profile.take()
    }

    pub(super) fn source_bytes(&self) -> u64 {
        self.source_bytes.saturating_add(
            self.textures
                .values()
                .filter_map(|texture| texture.data.as_ref())
                .map(|data| data.len() as u64)
                .sum::<u64>(),
        )
    }

    pub(super) fn command(&mut self, command: EmoteLayerCommand) -> Result<(), String> {
        match command {
            EmoteLayerCommand::SetScale {
                scale,
                origin_x,
                origin_y,
            } => {
                self.transform.scale = scale;
                self.transform.origin = [origin_x, origin_y];
                return Ok(());
            }
            EmoteLayerCommand::SetCoord { x, y, z, angle } => {
                self.transform.coord = [x, y, z, angle];
                return Ok(());
            }
            command => self.worker.send(command)?,
        }
        Ok(())
    }

    pub(super) fn advance(&mut self, delta_ms: u64) -> bool {
        self.worker.advance(delta_ms);
        let Some(scene) = self.worker.take_latest_scene() else {
            return false;
        };
        self.scene = scene;
        true
    }

    pub(super) fn build_commands(
        &mut self,
        provider: &mut dyn TextureProvider,
        retained: &mut HashSet<String>,
    ) -> Result<Vec<DrawCommand>, String> {
        let profiling = self.profile.enabled.load(Ordering::Relaxed);
        let started = profiling.then(Instant::now);
        self.upload_textures(provider, retained)?;
        let layer_transform = self.layer_transform();
        if self.cached_transform == Some(layer_transform)
            && self
                .cached_scene
                .as_ref()
                .is_some_and(|scene| Arc::ptr_eq(scene, &self.scene))
        {
            return Ok(self.cached_commands.clone());
        }
        let scene = &self.scene;
        let commands = scene
            .sprites
            .iter()
            .filter(|sprite| {
                sprite.visible
                    && sprite.opacity > 0.0
                    && matches!(
                        sprite.draw_frame_info.pass,
                        EmoteDrawPass::Normal | EmoteDrawPass::Filtered
                    )
                    && !sprite.feedback_history
            })
            .filter_map(|sprite| self.draw_command(scene, sprite, layer_transform))
            .collect::<Vec<_>>();
        if let Some(started) = started {
            let mut mesh_sprites = 0u64;
            let mut mesh_vertices = 0u64;
            for command in &commands {
                if let Some(mesh) = &command.mesh {
                    mesh_sprites += 1;
                    mesh_vertices = mesh_vertices.saturating_add(mesh.vertices.len() as u64);
                }
            }
            self.profile
                .draw_build_ns
                .fetch_add(elapsed_ns(started), Ordering::Relaxed);
            self.profile
                .sprites
                .store(commands.len() as u64, Ordering::Relaxed);
            self.profile
                .mesh_sprites
                .store(mesh_sprites, Ordering::Relaxed);
            self.profile
                .mesh_vertices
                .store(mesh_vertices, Ordering::Relaxed);
        }
        self.cached_scene = Some(Arc::clone(&self.scene));
        self.cached_transform = Some(layer_transform);
        self.cached_commands = commands;
        Ok(self.cached_commands.clone())
    }

    fn upload_textures(
        &mut self,
        provider: &mut dyn TextureProvider,
        retained: &mut HashSet<String>,
    ) -> Result<(), String> {
        for (resource_index, texture) in &mut self.textures {
            retained.insert(texture.name.clone());
            if texture.gpu.is_some() {
                continue;
            }
            let data = texture.data.as_deref().ok_or_else(|| {
                format!("Eluna texture {resource_index} was evicted after source release")
            })?;
            texture.gpu = provider.upload_dxt5_render_only(
                &texture.name,
                texture.width,
                texture.height,
                data,
            );
            if texture.gpu.is_none() {
                let rgba =
                    decode_dxt5_rgba8(data, texture.width, texture.height).map_err(|error| {
                        format!("Eluna failed to decode texture {resource_index}: {error}")
                    })?;
                texture.gpu = provider.upload_rgba_render_only(
                    &texture.name,
                    texture.width,
                    texture.height,
                    &rgba,
                );
            }
            if texture.gpu.is_none() {
                return Err(format!(
                    "Eluna failed to upload texture resource {resource_index}"
                ));
            }
            texture.data = None;
        }
        Ok(())
    }

    fn layer_transform(&self) -> Affine2 {
        let model_origin = Vec2::new(self.width as f32 * 0.5, self.height as f32 * 0.5);
        Affine2::from_translation(
            model_origin + Vec2::new(self.transform.coord[0], self.transform.coord[1]),
        ) * Affine2::from_angle(self.transform.coord[3].to_radians())
            * Affine2::from_scale(Vec2::splat(self.transform.scale))
            * Affine2::from_translation(Vec2::new(
                -self.transform.origin[0],
                -self.transform.origin[1],
            ))
    }

    fn draw_command(
        &self,
        scene: &EmoteStaticScene,
        sprite: &EmoteStaticSprite,
        layer_transform: Affine2,
    ) -> Option<DrawCommand> {
        let mask_labels = sprite
            .draw_frame_info
            .parent_mask_path
            .as_ref()
            .or(sprite.draw_frame_info.stencil_parent_path.as_ref())
            .and_then(|owner| scene.composite_mask_owners.get(owner));
        // An existing composite owner with no active sources is an empty
        // alpha mask, not an instruction to draw the child without a mask.
        if mask_labels.is_some_and(Vec::is_empty) {
            return None;
        }
        let texture = self.textures.get(&sprite.texture_resource_index)?;
        let (texture_id, texture_info) = texture.gpu?;
        let has_mesh = sprite.mesh.is_some() || !sprite.mesh_deformer_chain.is_empty();
        // `world_transform` is the complete Eluna layer transform for this
        // sprite. Keep it in the DrawCommand transform and leave mesh points
        // in the sprite's local icon space; the GL backend applies the command
        // transform exactly once.
        let sprite_transform = sprite_affine(sprite);
        let (transform, clip, mesh) = if has_mesh {
            let started = self
                .profile
                .enabled
                .load(Ordering::Relaxed)
                .then(Instant::now);
            let vertices = mesh::sprite_vertices(sprite, layer_transform * sprite_transform);
            if let Some(started) = started {
                self.profile
                    .mesh_build_ns
                    .fetch_add(elapsed_ns(started), Ordering::Relaxed);
            }
            (
                layer_transform * sprite_transform,
                ClipRect {
                    uv_offset: [0.0, 0.0],
                    uv_scale: [1.0, 1.0],
                    quad_size: [1.0, 1.0],
                },
                Some(DrawMesh {
                    vertices: vertices.into(),
                }),
            )
        } else {
            (
                layer_transform * sprite_transform,
                ClipRect {
                    uv_offset: [sprite.uv_left, sprite.uv_top],
                    uv_scale: [
                        sprite.uv_right - sprite.uv_left,
                        sprite.uv_bottom - sprite.uv_top,
                    ],
                    quad_size: [sprite.width, sprite.height],
                },
                None,
            )
        };
        Some(DrawCommand {
            texture: texture_id,
            size: texture_info,
            transform,
            // Sprite opacity is already folded into NativeEmoteMaterial corner
            // alpha; keeping command opacity at 1 avoids multiplying it twice.
            opacity: 1.0,
            blend: eluna_blend(sprite.blend_mode),
            color: ColorFilter::default(),
            clip,
            clip_bounds: None,
            shader: None,
            mesh,
            stencil: Some(StencilMetadata {
                namespace: self.generation,
                source_label: sprite.draw_frame_info.path.clone(),
                mask_labels: mask_labels.cloned().unwrap_or_default(),
            }),
            native_emote: Some(native_emote_material(sprite)),
        })
    }
}

impl ElunaWorker {
    fn spawn(
        runtime: EmoteRuntime,
        path: &str,
        initial_scene: Arc<EmoteStaticScene>,
        profile: Arc<ElunaProfileCounters>,
    ) -> Result<Self, String> {
        let (message_tx, message_rx) = mpsc::channel();
        let latest_scene = Arc::new(Mutex::new(None));
        let worker_latest_scene = latest_scene.clone();
        let thread_name = format!(
            "art3m1s-eluna-{}",
            path.rsplit('/').next().unwrap_or("model")
        );
        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                run_eluna_worker(
                    runtime,
                    message_rx,
                    worker_latest_scene,
                    initial_scene,
                    profile,
                );
            })
            .map_err(|error| format!("failed to start Eluna worker for {path}: {error}"))?;
        Ok(Self {
            message_tx,
            latest_scene,
        })
    }

    fn send(&self, command: EmoteLayerCommand) -> Result<(), String> {
        self.message_tx
            .send(ElunaWorkerMessage::Command(command))
            .map_err(|_| "Eluna worker stopped".to_owned())
    }

    fn advance(&self, delta_ms: u64) {
        // Advance is both elapsed time and the host-frame commit marker. Keeping
        // it in the channel preserves every delayed tick while waking the worker
        // immediately instead of capping animation output with a fixed timeout.
        let _ = self.message_tx.send(ElunaWorkerMessage::Advance(delta_ms));
    }

    fn take_latest_scene(&self) -> Option<Arc<EmoteStaticScene>> {
        self.latest_scene.lock().ok()?.take()
    }
}

fn run_eluna_worker(
    mut runtime: EmoteRuntime,
    message_rx: mpsc::Receiver<ElunaWorkerMessage>,
    latest_scene: Arc<Mutex<Option<Arc<EmoteStaticScene>>>>,
    initial_scene: Arc<EmoteStaticScene>,
    profile: Arc<ElunaProfileCounters>,
) {
    if let Ok(mut slot) = latest_scene.lock() {
        *slot = Some(initial_scene);
    }
    let mut deferred_commands = Vec::new();
    loop {
        let Some((commands, delta_ms, input_frames)) =
            receive_worker_batch(&message_rx, &mut deferred_commands)
        else {
            break;
        };
        if commands.is_empty() && delta_ms == 0 {
            continue;
        }

        let had_commands = !commands.is_empty();
        for command in commands {
            if let Err(error) = apply_worker_command(&mut runtime, command) {
                crate::core_warn!("[E-Mote:Eluna] worker command failed: {error}");
            }
        }
        // A static model has no time-dependent state to evaluate.  The old
        // worker rebuilt all layer state on every host frame even after the
        // initial scene had settled, keeping one CPU core busy per model.
        // Commands still wake the worker and force a rebuild; only a pure
        // elapsed-time tick can be discarded while the runtime is idle.
        if delta_ms != 0 && !had_commands && !runtime.is_animating() && !runtime.is_modified() {
            continue;
        }
        let profiling = profile.enabled.load(Ordering::Relaxed);
        let update_started = profiling.then(Instant::now);
        let update = if delta_ms != 0 {
            // Scene evaluation can take longer than one host frame. Advance
            // directly to the newest model time and build only that scene;
            // the capped SDK helper would otherwise discard all but 100 ms
            // and make animation speed depend on evaluator performance.
            runtime.progress_ticks(eluna::milliseconds_to_emote_ticks(delta_ms as f32))
        } else if had_commands {
            runtime.rebuild_scene()
        } else {
            continue;
        };
        if let Err(error) = update {
            crate::core_warn!("[E-Mote:Eluna] worker update failed: {error}");
            continue;
        }
        if let Some(started) = update_started {
            profile
                .worker_eval_ns
                .fetch_add(elapsed_ns(started), Ordering::Relaxed);
            profile.worker_updates.fetch_add(1, Ordering::Relaxed);
            profile
                .worker_input_frames
                .fetch_add(input_frames, Ordering::Relaxed);
        }
        runtime.clear_modified();
        let clone_started = profiling.then(Instant::now);
        let scene = runtime.shared_scene();
        if let Some(started) = clone_started {
            profile
                .scene_clone_ns
                .fetch_add(elapsed_ns(started), Ordering::Relaxed);
        }
        if let Ok(mut slot) = latest_scene.lock() {
            let dropped = slot.replace(scene).is_some();
            if profiling && dropped {
                profile
                    .worker_dropped_scenes
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

fn receive_worker_batch(
    message_rx: &mpsc::Receiver<ElunaWorkerMessage>,
    deferred_commands: &mut Vec<EmoteLayerCommand>,
) -> Option<(Vec<EmoteLayerCommand>, u64, u64)> {
    let mut commands = std::mem::take(deferred_commands);
    let mut delta_ms = 0u64;
    let mut input_frames = 0u64;
    // Advance is the host-frame commit point. Commands commonly arrive as a
    // pass/play/fade/step sequence; evaluating before that sequence is complete
    // produces redundant full scene builds and exposes transient face states.
    let mut committed_commands = loop {
        match message_rx.recv().ok()? {
            ElunaWorkerMessage::Command(command) => commands.push(command),
            ElunaWorkerMessage::Advance(delta) => {
                delta_ms = delta_ms.saturating_add(delta);
                input_frames += 1;
                break commands.len();
            }
        }
    };
    for message in message_rx.try_iter() {
        match message {
            ElunaWorkerMessage::Command(command) => commands.push(command),
            ElunaWorkerMessage::Advance(delta) => {
                delta_ms = delta_ms.saturating_add(delta);
                input_frames += 1;
                committed_commands = commands.len();
            }
        }
    }
    *deferred_commands = commands.split_off(committed_commands);
    Some((commands, delta_ms, input_frames))
}

fn elapsed_ns(started: Instant) -> u64 {
    started.elapsed().as_nanos().min(u64::MAX as u128) as u64
}

fn apply_worker_command(
    runtime: &mut EmoteRuntime,
    command: EmoteLayerCommand,
) -> Result<(), String> {
    match command {
        EmoteLayerCommand::SetVariable {
            label,
            value,
            frames,
            easing,
        } => {
            if frames <= 0.0 {
                runtime
                    .inner_player_mut()
                    .set_variable_immediate(&label, value);
            } else {
                runtime
                    .inner_player_mut()
                    .set_variable_timed(&label, value, frames, easing as f32);
            }
        }
        EmoteLayerCommand::PlayTimeline { label, flags } => {
            if !runtime.inner_player().timelines().contains_key(&label) {
                return Err(format!("timeline '{label}' does not exist"));
            }
            runtime
                .inner_player_mut()
                .play_timeline(&label, TimelinePlayMode::from_flags(flags));
        }
        EmoteLayerCommand::FadeInTimeline {
            label,
            frames,
            easing,
        } => runtime
            .inner_player_mut()
            .fade_in_timeline(&label, frames, easing as f32),
        EmoteLayerCommand::FadeOutTimeline {
            label,
            frames,
            easing,
        } => runtime
            .inner_player_mut()
            .fade_out_timeline(&label, frames, easing as f32),
        EmoteLayerCommand::StopTimeline { label } => {
            runtime.inner_player_mut().stop_timeline(&label)
        }
        EmoteLayerCommand::Pass => runtime.inner_player_mut().pass(),
        EmoteLayerCommand::Step => runtime.inner_player_mut().step(),
        EmoteLayerCommand::Skip => runtime.inner_player_mut().skip(),
        EmoteLayerCommand::SetScale { .. } | EmoteLayerCommand::SetCoord { .. } => {}
    }
    Ok(())
}

fn sprite_affine(sprite: &EmoteStaticSprite) -> Affine2 {
    let world = sprite.world_transform;
    let world_affine = Affine2::from_cols(
        Vec2::new(world[0], world[2]),
        Vec2::new(world[1], world[3]),
        Vec2::new(world[4], world[5]),
    );
    // Atlas icons are drawn from a top-left quad and pivot around originX/Y.
    // Frame-local scale/rotation therefore acts on `(p - origin)` before the
    // already-resolved world transform is applied.
    let origin = Vec2::new(-sprite.left(), -sprite.top());
    let scale = Vec2::new(
        if sprite.scale_x.is_finite() {
            sprite.scale_x
        } else {
            1.0
        },
        if sprite.scale_y.is_finite() {
            sprite.scale_y
        } else {
            1.0
        },
    );
    let angle = if sprite.rotation_degrees.is_finite() {
        sprite.rotation_degrees.to_radians()
    } else {
        0.0
    };
    world_affine
        * Affine2::from_angle(angle)
        * Affine2::from_scale(scale)
        * Affine2::from_translation(-origin)
}

fn native_emote_material(sprite: &EmoteStaticSprite) -> NativeEmoteMaterial {
    NativeEmoteMaterial {
        corner_colors: sprite.corner_colors.map(|packed| {
            [
                ((packed >> 24) & 0xff) as f32 / 255.0,
                ((packed >> 16) & 0xff) as f32 / 255.0,
                ((packed >> 8) & 0xff) as f32 / 255.0,
                ((packed & 0xff) as f32 / 255.0 * sprite.opacity).clamp(0.0, 1.0),
            ]
        }),
        uv_rect: [
            sprite.uv_left,
            sprite.uv_top,
            sprite.uv_right,
            sprite.uv_bottom,
        ],
        blend_mode: sprite.blend_mode,
        clip_rect: sprite
            .draw_frame_info
            .clip_rect
            .unwrap_or([-1.0e30, -1.0e30, 1.0e30, 1.0e30]),
        wipe: [
            sprite.draw_frame_info.stencil_wipe_scale,
            sprite.draw_frame_info.stencil_wipe_bias,
            sprite.draw_frame_info.stencil_wipe_enabled as u8 as f32,
        ],
    }
}

fn eluna_blend(mode: u32) -> BlendMode {
    match mode & 0x0f {
        1 => BlendMode::NativeAdd,
        2 | 5 => BlendMode::NativeReverseSubtract,
        3 => BlendMode::NativeMultiply,
        4 => BlendMode::NativeScreen,
        _ => BlendMode::Alpha,
    }
}

fn infer_emote_header_key(data: &[u8]) -> Option<u32> {
    if data.get(..4)? != b"PSB\0" {
        return None;
    }
    let version = u16::from_le_bytes(data.get(4..6)?.try_into().ok()?);
    let flags = u16::from_le_bytes(data.get(6..8)?.try_into().ok()?);
    if flags & 1 == 0 {
        return None;
    }
    let header_length = match version {
        1 | 2 => 40u32,
        3 => 44u32,
        4 => 56u32,
        _ => return None,
    };
    let encrypted = u32::from_le_bytes(data.get(8..12)?.try_into().ok()?);
    let first_stream_word = encrypted ^ header_length;
    let key1 = 123_456_789u32;
    let shifted = key1 ^ key1.wrapping_shl(11);
    let rhs = first_stream_word ^ shifted ^ (shifted >> 8);
    Some(rhs ^ (rhs >> 19))
}

fn decode_dxt5_rgba8(data: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let blocks_x = width.div_ceil(4) as usize;
    let blocks_y = height.div_ceil(4) as usize;
    let expected = blocks_x
        .checked_mul(blocks_y)
        .and_then(|blocks| blocks.checked_mul(16))
        .ok_or_else(|| "DXT5 texture size overflow".to_owned())?;
    if data.len() != expected {
        return Err(format!(
            "DXT5 data has {} bytes, expected {expected}",
            data.len()
        ));
    }
    let pixel_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| "RGBA texture size overflow".to_owned())?;
    let mut output = vec![0; pixel_count * 4];
    for block_y in 0..blocks_y {
        for block_x in 0..blocks_x {
            let offset = (block_y * blocks_x + block_x) * 16;
            decode_dxt5_block(
                &data[offset..offset + 16],
                &mut output,
                width as usize,
                height as usize,
                block_x * 4,
                block_y * 4,
            );
        }
    }
    Ok(output)
}

fn decode_dxt5_block(
    block: &[u8],
    output: &mut [u8],
    width: usize,
    height: usize,
    origin_x: usize,
    origin_y: usize,
) {
    let alphas = alpha_palette(block[0], block[1]);
    let alpha_bits = u64::from_le_bytes([
        block[2], block[3], block[4], block[5], block[6], block[7], 0, 0,
    ]);
    let colors = color_palette(
        u16::from_le_bytes([block[8], block[9]]),
        u16::from_le_bytes([block[10], block[11]]),
    );
    let color_bits = u32::from_le_bytes([block[12], block[13], block[14], block[15]]);
    for pixel in 0..16 {
        let x = origin_x + pixel % 4;
        let y = origin_y + pixel / 4;
        if x >= width || y >= height {
            continue;
        }
        let color = colors[((color_bits >> (pixel * 2)) & 3) as usize];
        let alpha = alphas[((alpha_bits >> (pixel * 3)) & 7) as usize];
        let offset = (y * width + x) * 4;
        output[offset..offset + 4].copy_from_slice(&[color[0], color[1], color[2], alpha]);
    }
}

fn alpha_palette(a0: u8, a1: u8) -> [u8; 8] {
    let mut result = [a0, a1, 0, 0, 0, 0, 0, 0];
    if a0 > a1 {
        for index in 1..=6 {
            result[index + 1] =
                (((7 - index) as u16 * a0 as u16 + index as u16 * a1 as u16) / 7) as u8;
        }
    } else {
        for index in 1..=4 {
            result[index + 1] =
                (((5 - index) as u16 * a0 as u16 + index as u16 * a1 as u16) / 5) as u8;
        }
        result[6] = 0;
        result[7] = 255;
    }
    result
}

fn color_palette(c0: u16, c1: u16) -> [[u8; 3]; 4] {
    let a = rgb565(c0);
    let b = rgb565(c1);
    [
        a,
        b,
        [
            ((2 * a[0] as u16 + b[0] as u16) / 3) as u8,
            ((2 * a[1] as u16 + b[1] as u16) / 3) as u8,
            ((2 * a[2] as u16 + b[2] as u16) / 3) as u8,
        ],
        [
            ((a[0] as u16 + 2 * b[0] as u16) / 3) as u8,
            ((a[1] as u16 + 2 * b[1] as u16) / 3) as u8,
            ((a[2] as u16 + 2 * b[2] as u16) / 3) as u8,
        ],
    ]
}

fn rgb565(value: u16) -> [u8; 3] {
    let red = ((value >> 11) & 0x1f) as u8;
    let green = ((value >> 5) & 0x3f) as u8;
    let blue = (value & 0x1f) as u8;
    [
        (red << 3) | (red >> 2),
        (green << 2) | (green >> 4),
        (blue << 3) | (blue >> 2),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(all(target_os = "macos", feature = "gl-backend"))]
    #[ignore = "requires EMOTE_TEST_MODEL pointing at a NekoMiko PSB"]
    fn model_blink_reaches_eyelids_and_renders_distinct_frames() {
        use crate::backend::gl::{GlRenderer, GlTextureProvider, ShaderProfile, platform};
        use crate::render_pipeline::draw::DrawList;
        use glow::HasContext;

        let path = std::env::var("EMOTE_TEST_MODEL").unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let mut options = EmoteLoadOptions {
            autoplay_timeline: false,
            ..Default::default()
        };
        if let Some(key) = infer_emote_header_key(&bytes) {
            options = options.with_emote_key(key);
        }
        let mut runtime = EmoteRuntime::from_bytes(&bytes, options).unwrap();
        runtime.set_physics_enabled(false);
        let mut instance = ElunaEmoteInstance::new(1, &path, &bytes, 1024, 1024, false).unwrap();
        let centers = runtime
            .sprites()
            .iter()
            .filter(|s| s.label.as_deref() == Some("mabuta"))
            .map(|s| sprite_affine(s).transform_point2(Vec2::new(s.width * 0.5, s.height * 0.5)))
            .collect::<Vec<_>>();
        assert_eq!(centers.len(), 2);
        instance.transform.origin = ((centers[0] + centers[1]) * 0.5).to_array();
        instance.transform.scale = 1.0;
        // The validation host can select ANGLE without requiring a windowing
        // system.  `EMOTE_TEST_ANGLE_PATH` points at a directory containing
        // libEGL/libGLESv2 (either macOS dylibs or framework bundles).
        let requested_backend = std::env::var("EMOTE_TEST_BACKEND")
            .ok()
            .map(|value| match value.to_ascii_lowercase().as_str() {
                "angle" | "metal" | "3" => {
                    platform::GfxBackend::Angle(platform::AngleBackend::Metal)
                }
                "gl" | "opengl" | "1" => {
                    platform::GfxBackend::Angle(platform::AngleBackend::OpenGL)
                }
                "vulkan" | "2" => platform::GfxBackend::Angle(platform::AngleBackend::Vulkan),
                _ => platform::GfxBackend::Cgl,
            })
            .unwrap_or(platform::GfxBackend::Cgl);
        if let Ok(path) = std::env::var("EMOTE_TEST_ANGLE_PATH") {
            let path = std::ffi::CString::new(path).unwrap();
            unsafe { crate::ffi::art3m1s_set_angle_path(path.as_ptr()) };
        }
        let (gl, _context, effective_backend) =
            platform::create_offscreen_context(requested_backend, 1024, 1024).unwrap();
        let profile = match effective_backend {
            platform::GfxBackend::Angle(_) => ShaderProfile::Gles300,
            platform::GfxBackend::Cgl => ShaderProfile::GlCore330,
        };
        let mut renderer = GlRenderer::new(gl.clone(), 1024, 1024, profile).unwrap();
        let mut textures = GlTextureProvider::new(gl.clone(), profile);
        let (fbo, color) = unsafe { platform::create_fbo_target(&gl, 1024, 1024).unwrap() };
        let mut previous_pixels = None;
        let mut previous_icons = Vec::new();
        let mut captures = 0;
        for _ in 0..1200 {
            runtime.progress_ticks(1.0).unwrap();
            let eye = runtime.inner_player().evaluated_variable_values()["face_eye_open"];
            let wanted = if captures == 1 { 10.0 } else { 0.0 };
            if eye != wanted {
                continue;
            }
            let lids = runtime
                .sprites()
                .iter()
                .filter(|s| s.label.as_deref() == Some("mabuta"))
                .collect::<Vec<_>>();
            assert_eq!(lids.len(), 2);
            assert!(
                lids.iter()
                    .all(|s| s.visible && s.draw_frame_info.local_time_ticks == Some(eye + 10.0))
            );
            let whites = runtime
                .sprites()
                .iter()
                .filter(|s| s.label.as_deref() == Some("shirome"))
                .collect::<Vec<_>>();
            assert_eq!(whites.len(), 2);
            assert!(whites.iter().all(|s| s.visible == (captures != 1)));
            let icons = lids.iter().map(|s| s.icon_name.clone()).collect::<Vec<_>>();
            if captures > 0 {
                assert_ne!(icons, previous_icons);
            }
            previous_icons = icons;
            instance.scene = runtime.shared_scene();
            let mut frame = DrawList::new();
            for command in instance
                .build_commands(&mut textures, &mut HashSet::new())
                .unwrap()
            {
                frame.push(command);
            }
            frame.materialize_stencil_groups(crate::render_pipeline::shader::ALPHA_MASK_SHADER);
            assert!(
                frame.commands.len() > 20,
                "the rest of the model must remain visible"
            );
            if captures == 1 {
                assert!(
                    frame
                        .commands
                        .iter()
                        .all(|command| command.stencil.as_ref().is_none_or(|s| {
                            !s.source_label.ends_with("/eye_L")
                                && !s.source_label.ends_with("/eye_R")
                        })),
                    "an empty white-eye mask must not expose unmasked pupils"
                );
            }
            unsafe {
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            }
            renderer.render(&frame);
            unsafe {
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            }
            let pixels = unsafe { platform::read_pixels(&gl, 1024, 1024) };
            assert!(pixels.chunks_exact(4).any(|p| p[3] != 0));
            if let Ok(output) = std::env::var("EMOTE_TEST_OUTPUT") {
                image::save_buffer(
                    format!("{output}/blink-{captures}.png"),
                    &pixels,
                    1024,
                    1024,
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
            if let Some(previous) = &previous_pixels {
                assert!(&pixels != previous);
            }
            previous_pixels = Some(pixels);
            captures += 1;
            if captures == 3 {
                break;
            }
        }
        assert_eq!(captures, 3, "expected open, closed and reopened frames");
        unsafe {
            gl.delete_framebuffer(fbo);
            gl.delete_texture(color);
        }
    }

    #[test]
    fn worker_accumulates_elapsed_time_and_wakes_for_each_host_batch() {
        let (message_tx, message_rx) = mpsc::channel();
        let worker = ElunaWorker {
            message_tx,
            latest_scene: Arc::new(Mutex::new(None)),
        };

        worker.advance(16);
        worker.advance(500);
        worker.advance(250);

        assert_eq!(
            receive_worker_batch(&message_rx, &mut Vec::new()),
            Some((Vec::new(), 766, 3))
        );
    }

    #[test]
    fn worker_batches_face_commands_until_the_host_frame_commit() {
        let (message_tx, message_rx) = mpsc::channel();
        let worker = ElunaWorker {
            message_tx,
            latest_scene: Arc::new(Mutex::new(None)),
        };
        let play = EmoteLayerCommand::PlayTimeline {
            label: "face".to_owned(),
            flags: 1,
        };
        let fade = EmoteLayerCommand::FadeInTimeline {
            label: "idle".to_owned(),
            frames: 0.0,
            easing: 0,
        };

        worker.send(EmoteLayerCommand::Pass).unwrap();
        worker.send(play.clone()).unwrap();
        worker.send(fade.clone()).unwrap();
        worker.advance(16);

        assert_eq!(
            receive_worker_batch(&message_rx, &mut Vec::new()),
            Some((vec![EmoteLayerCommand::Pass, play, fade], 16, 1))
        );
    }

    #[test]
    fn worker_defers_next_frame_commands_until_their_commit() {
        let (message_tx, message_rx) = mpsc::channel();
        message_tx.send(ElunaWorkerMessage::Advance(16)).unwrap();
        message_tx
            .send(ElunaWorkerMessage::Command(EmoteLayerCommand::Pass))
            .unwrap();

        let mut deferred = Vec::new();
        assert_eq!(
            receive_worker_batch(&message_rx, &mut deferred),
            Some((Vec::new(), 16, 1))
        );
        assert_eq!(deferred, vec![EmoteLayerCommand::Pass]);

        message_tx.send(ElunaWorkerMessage::Advance(17)).unwrap();
        assert_eq!(
            receive_worker_batch(&message_rx, &mut deferred),
            Some((vec![EmoteLayerCommand::Pass], 17, 1))
        );
    }
}
