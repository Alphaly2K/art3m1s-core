use asb_interpreter::EmoteLayerCommand;
use eluna::{
    EmoteDrawPass, EmoteLoadOptions, EmotePlayerControl, EmoteRuntime, EmoteStaticScene,
    EmoteStaticSprite, TimelinePlayMode,
};
use glam::{Affine2, Vec2};
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use crate::render_pipeline::draw::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawMesh, StencilMetadata, TextureId,
    TextureInfo, TextureProvider,
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
    data: Arc<[u8]>,
}

struct ElunaWorker {
    command_tx: mpsc::Sender<EmoteLayerCommand>,
    pending_ms: Arc<AtomicU64>,
    latest_scene: Arc<Mutex<Option<Arc<EmoteStaticScene>>>>,
}

impl ElunaEmoteInstance {
    pub(super) fn new(
        generation: u64,
        path: &str,
        bytes: &[u8],
        width: u32,
        height: u32,
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
                        data,
                    },
                )
            })
            .collect();
        let scene = Arc::new(runtime.scene().clone());
        let worker = ElunaWorker::spawn(runtime, path, scene.clone())?;

        Ok(Self {
            generation,
            width,
            height,
            worker,
            scene,
            transform: ElunaLayerTransform::default(),
            textures,
            source_bytes,
        })
    }

    pub(super) fn source_bytes(&self) -> u64 {
        self.source_bytes
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
        self.upload_textures(provider, retained)?;
        let layer_transform = self.layer_transform();
        let scene = &self.scene;
        Ok(scene
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
            .collect())
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
            let data = &texture.data;
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
        let texture = self.textures.get(&sprite.texture_resource_index)?;
        let (texture_id, texture_info) = texture.gpu?;
        let (multiply, alpha) = sprite_color(sprite);
        let mask_labels = sprite
            .draw_frame_info
            .parent_mask_path
            .as_ref()
            .or(sprite.draw_frame_info.stencil_parent_path.as_ref())
            .and_then(|owner| scene.composite_mask_owners.get(owner))
            .cloned()
            .unwrap_or_default();
        Some(DrawCommand {
            texture: texture_id,
            size: texture_info,
            transform: layer_transform,
            opacity: sprite.opacity * alpha,
            blend: eluna_blend(sprite.blend_mode),
            color: ColorFilter {
                multiply,
                grayscale: false,
                negative: false,
            },
            clip: ClipRect::full(texture_info),
            clip_bounds: None,
            shader: None,
            mesh: Some(sprite_mesh(sprite)),
            stencil: Some(StencilMetadata {
                namespace: self.generation,
                source_label: sprite.draw_frame_info.path.clone(),
                mask_labels,
            }),
        })
    }
}

impl ElunaWorker {
    fn spawn(
        runtime: EmoteRuntime,
        path: &str,
        initial_scene: Arc<EmoteStaticScene>,
    ) -> Result<Self, String> {
        let (command_tx, command_rx) = mpsc::channel();
        let pending_ms = Arc::new(AtomicU64::new(0));
        let latest_scene = Arc::new(Mutex::new(None));
        let worker_pending_ms = pending_ms.clone();
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
                    command_rx,
                    worker_pending_ms,
                    worker_latest_scene,
                    initial_scene,
                );
            })
            .map_err(|error| format!("failed to start Eluna worker for {path}: {error}"))?;
        Ok(Self {
            command_tx,
            pending_ms,
            latest_scene,
        })
    }

    fn send(&self, command: EmoteLayerCommand) -> Result<(), String> {
        self.command_tx
            .send(command)
            .map_err(|_| "Eluna worker stopped".to_owned())
    }

    fn advance(&self, delta_ms: u64) {
        let _ = self
            .pending_ms
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |pending| {
                Some(pending.saturating_add(delta_ms).min(100))
            });
    }

    fn take_latest_scene(&self) -> Option<Arc<EmoteStaticScene>> {
        self.latest_scene.lock().ok()?.take()
    }
}

fn run_eluna_worker(
    mut runtime: EmoteRuntime,
    command_rx: mpsc::Receiver<EmoteLayerCommand>,
    pending_ms: Arc<AtomicU64>,
    latest_scene: Arc<Mutex<Option<Arc<EmoteStaticScene>>>>,
    initial_scene: Arc<EmoteStaticScene>,
) {
    if let Ok(mut slot) = latest_scene.lock() {
        *slot = Some(initial_scene);
    }
    loop {
        let first_command = match command_rx.recv_timeout(Duration::from_millis(33)) {
            Ok(command) => Some(command),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let mut commands = first_command.into_iter().collect::<Vec<_>>();
        commands.extend(command_rx.try_iter());
        let delta_ms = pending_ms.swap(0, Ordering::Relaxed);
        if commands.is_empty() && delta_ms == 0 {
            continue;
        }

        let had_commands = !commands.is_empty();
        for command in commands {
            if let Err(error) = apply_worker_command(&mut runtime, command) {
                crate::core_warn!("[E-Mote:Eluna] worker command failed: {error}");
            }
        }
        let update = if delta_ms != 0 {
            runtime.progress_milliseconds_capped(delta_ms as f32)
        } else if had_commands {
            runtime.rebuild_scene()
        } else {
            continue;
        };
        if let Err(error) = update {
            crate::core_warn!("[E-Mote:Eluna] worker update failed: {error}");
            continue;
        }
        runtime.clear_modified();
        if let Ok(mut slot) = latest_scene.lock() {
            *slot = Some(Arc::new(runtime.scene().clone()));
        }
    }
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
                    .set_variable_immediate(&label, value)
                    .map_err(|error| error.to_string())?;
            } else {
                runtime.set_variable_timed(&label, value, frames, easing as f32);
            }
        }
        EmoteLayerCommand::PlayTimeline { label, flags } => runtime
            .play_timeline(&label, TimelinePlayMode::from_flags(flags))
            .map_err(|error| error.to_string())?,
        EmoteLayerCommand::FadeInTimeline {
            label,
            frames,
            easing,
        } => runtime
            .fade_in_timeline(&label, frames, easing as f32)
            .map_err(|error| error.to_string())?,
        EmoteLayerCommand::FadeOutTimeline {
            label,
            frames,
            easing,
        } => runtime
            .fade_out_timeline(&label, frames, easing as f32)
            .map_err(|error| error.to_string())?,
        EmoteLayerCommand::StopTimeline { label } => runtime
            .stop_timeline(&label)
            .map_err(|error| error.to_string())?,
        EmoteLayerCommand::Pass => runtime.pass().map_err(|error| error.to_string())?,
        EmoteLayerCommand::Step => runtime.step().map_err(|error| error.to_string())?,
        EmoteLayerCommand::Skip => runtime.inner_player_mut().skip(),
        EmoteLayerCommand::SetScale { .. } | EmoteLayerCommand::SetCoord { .. } => {}
    }
    Ok(())
}

fn sprite_mesh(sprite: &EmoteStaticSprite) -> DrawMesh {
    let division_x = sprite
        .mesh
        .as_ref()
        .map_or(1, |mesh| mesh.division_x.max(1)) as usize;
    let division_y = sprite
        .mesh
        .as_ref()
        .map_or(1, |mesh| mesh.division_y.max(1)) as usize;
    let left = sprite.center_x - sprite.width * 0.5;
    let top = sprite.center_y - sprite.height * 0.5;
    let vertex = |x: usize, y: usize| {
        let u = x as f32 / division_x as f32;
        let v = y as f32 / division_y as f32;
        let point = sprite
            .mesh
            .as_ref()
            .map_or([u, v], |mesh| mesh.sample(u, v));
        let position = transform_sprite_point(
            sprite,
            [
                left + point[0] * sprite.width,
                top + point[1] * sprite.height,
            ],
        );
        [
            position[0],
            position[1],
            sprite.uv_left + (sprite.uv_right - sprite.uv_left) * u,
            sprite.uv_top + (sprite.uv_bottom - sprite.uv_top) * v,
        ]
    };

    let mut vertices = Vec::with_capacity(division_x * division_y * 6);
    for y in 0..division_y {
        for x in 0..division_x {
            let top_left = vertex(x, y);
            let bottom_left = vertex(x, y + 1);
            let top_right = vertex(x + 1, y);
            let bottom_right = vertex(x + 1, y + 1);
            vertices.extend_from_slice(&[
                top_left,
                bottom_left,
                top_right,
                top_right,
                bottom_left,
                bottom_right,
            ]);
        }
    }
    DrawMesh { vertices }
}

fn transform_sprite_point(sprite: &EmoteStaticSprite, point: [f32; 2]) -> [f32; 2] {
    let angle = sprite.rotation_degrees.to_radians();
    let cos = angle.cos();
    let sin = angle.sin();
    let dx = (point[0] - sprite.center_x) * sprite.scale_x;
    let dy = (point[1] - sprite.center_y) * sprite.scale_y;
    let local = [
        sprite.center_x + dx * cos - dy * sin,
        sprite.center_y + dx * sin + dy * cos,
    ];
    let matrix = sprite.world_transform;
    [
        matrix[0] * local[0] + matrix[1] * local[1] + matrix[4],
        matrix[2] * local[0] + matrix[3] * local[1] + matrix[5],
    ]
}

fn sprite_color(sprite: &EmoteStaticSprite) -> ([f32; 3], f32) {
    let mut rgba = [0.0f32; 4];
    for packed in sprite.corner_colors {
        rgba[0] += ((packed >> 24) & 0xff) as f32 / 255.0;
        rgba[1] += ((packed >> 16) & 0xff) as f32 / 255.0;
        rgba[2] += ((packed >> 8) & 0xff) as f32 / 255.0;
        rgba[3] += (packed & 0xff) as f32 / 255.0;
    }
    for value in &mut rgba {
        *value *= 0.25;
    }
    if sprite.blend_mode & 0xf0 == 0x10 {
        rgba[0] = (rgba[0] * 2.0).min(1.0);
        rgba[1] = (rgba[1] * 2.0).min(1.0);
        rgba[2] = (rgba[2] * 2.0).min(1.0);
    }
    ([rgba[0], rgba[1], rgba[2]], rgba[3])
}

fn eluna_blend(mode: u32) -> BlendMode {
    match mode & 0x0f {
        1 => BlendMode::Add,
        3 => BlendMode::Multiply,
        4 => BlendMode::Screen,
        2 | 5 => BlendMode::Multiply,
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
