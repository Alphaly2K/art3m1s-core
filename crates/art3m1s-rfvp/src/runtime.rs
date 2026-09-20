//! Runtime adapter between RFVP's captured frames and an Art3m1s GPU backend.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};

use art3m1s_render::{
    DrawCommand, Extent2D, FrameTarget, GpuBackend, RenderRegion, TextureData, TextureDesc,
    TextureId, TextureInfo, TextureUpdate,
};
use rfvp::host_api::{TextureFormat, TextureHandle, TextureRect};
use rfvp::rendering::external::{
    ExternalFrame, RecordedTextureCommand, RecordedTextureCreate, RecordedTextureDestroy,
    RecordedTextureUpdate,
};

use crate::{AdaptedFrame, AdapterError, DrawListAdapter, HitProxyTable};

#[derive(Debug)]
pub enum ExternalRendererError {
    Adapter(AdapterError),
    Backend(String),
    UnsupportedTextureFormat(TextureFormat),
    TextureSizeOverflow {
        width: u16,
        height: u16,
    },
    InvalidTextureData {
        handle: u32,
        format: TextureFormat,
        expected: usize,
        actual: usize,
    },
    UnknownTexture(u32),
    TextureUpdateOutOfBounds {
        handle: u32,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        texture_width: u32,
        texture_height: u32,
    },
}

impl fmt::Display for ExternalRendererError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Adapter(error) => write!(f, "RFVP draw-list adaptation failed: {error}"),
            Self::Backend(error) => write!(f, "Art3m1s GPU backend failed: {error}"),
            Self::UnsupportedTextureFormat(format) => {
                write!(f, "RFVP texture format {format:?} is not supported")
            }
            Self::TextureSizeOverflow { width, height } => {
                write!(
                    f,
                    "RFVP texture size {width}x{height} overflows RGBA byte count"
                )
            }
            Self::InvalidTextureData {
                handle,
                format,
                expected,
                actual,
            } => write!(
                f,
                "RFVP texture handle {handle} has {actual} bytes for {format:?}, expected {expected}"
            ),
            Self::UnknownTexture(handle) => {
                write!(f, "RFVP texture handle {handle} is not bound")
            }
            Self::TextureUpdateOutOfBounds {
                handle,
                x,
                y,
                width,
                height,
                texture_width,
                texture_height,
            } => write!(
                f,
                "RFVP texture update {x},{y} {width}x{height} exceeds texture {handle} ({texture_width}x{texture_height})"
            ),
        }
    }
}

impl std::error::Error for ExternalRendererError {}

impl From<AdapterError> for ExternalRendererError {
    fn from(error: AdapterError) -> Self {
        Self::Adapter(error)
    }
}

#[derive(Debug, Clone, Copy)]
struct CachedTexture {
    texture: TextureId,
    info: TextureInfo,
    format: TextureFormat,
    generation: u64,
}

#[derive(Debug)]
pub struct RfvpRenderResult {
    pub hit_proxies: HitProxyTable,
    pub region: RenderRegion,
}

#[derive(Debug, Clone, Copy)]
struct CommandState {
    hash: u64,
    texture: TextureId,
    bbox: [f32; 4],
}

/// Presented-frame snapshot used to skip unchanged frames and to derive a
/// conservative damage rectangle for changed ones.
#[derive(Debug)]
struct FrameCache {
    signature: u64,
    commands: Vec<CommandState>,
    /// RFVP texture handle -> generation at the presented frame.
    generations: HashMap<u32, u64>,
}

/// Owns RFVP textures and submits captured frames to an Art3m1s backend.
pub struct ExternalRenderer {
    backend: Box<dyn GpuBackend>,
    adapter: DrawListAdapter,
    textures: HashMap<TextureHandle, CachedTexture>,
    clear_color: [f32; 4],
    frame_cache: Option<FrameCache>,
    damage_visualization: bool,
}

impl ExternalRenderer {
    pub fn new(backend: Box<dyn GpuBackend>, clear_color: [f32; 4]) -> Self {
        Self {
            backend,
            adapter: DrawListAdapter::new(),
            textures: HashMap::new(),
            clear_color,
            frame_cache: None,
            damage_visualization: false,
        }
    }

    /// Toggles the flashing damage-rect debug overlay. When enabled, frame
    /// skipping stays active but a stale overlay is still cleared on skip.
    pub fn set_damage_visualization(&mut self, enabled: bool) {
        if self.damage_visualization != enabled {
            self.damage_visualization = enabled;
            self.frame_cache = None;
        }
    }

    /// Drops the presented-frame snapshot so the next `render_frame`
    /// repaints fully. Call after backend resize or surface changes.
    pub fn invalidate_frame_cache(&mut self) {
        self.frame_cache = None;
    }

    pub fn backend(&self) -> &dyn GpuBackend {
        self.backend.as_ref()
    }

    pub fn backend_mut(&mut self) -> &mut dyn GpuBackend {
        self.backend.as_mut()
    }

    pub fn adapter(&self) -> &DrawListAdapter {
        &self.adapter
    }

    pub fn cached_texture_count(&self) -> usize {
        self.textures.len()
    }

    /// Renders a captured frame. Returns `Ok(None)` when the frame is
    /// pixel-identical to the previously presented one (same adapted commands
    /// and texture generations) so the host can skip presentation entirely.
    pub fn render_frame(
        &mut self,
        frame: &ExternalFrame,
    ) -> Result<Option<RfvpRenderResult>, ExternalRendererError> {
        if frame.texture_commands.is_empty() {
            self.sync_texture_creates(&frame.textures)?;
        } else {
            self.sync_texture_commands(&frame.texture_commands)?;
        }

        let adapted: AdaptedFrame = self.adapter.convert_rfvp_frame(&frame.frame)?;
        let hit_proxies = adapted.hit_proxies.clone();

        let commands: Vec<CommandState> = adapted
            .draw_list
            .commands
            .iter()
            .map(command_state)
            .collect();
        let generations: HashMap<u32, u64> = self
            .textures
            .iter()
            .map(|(handle, cached)| (handle.0, cached.generation))
            .collect();
        let signature = frame_signature(&commands, &generations);

        if let Some(cache) = &self.frame_cache
            && cache.signature == signature
        {
            // Even on a skipped frame a previously drawn debug overlay must
            // be cleaned up, or it would linger on screen forever.
            if self.damage_visualization {
                self.backend
                    .begin_frame(FrameTarget::Main)
                    .map_err(ExternalRendererError::Backend)?;
                let cleared = self.backend.clear_damage_overlay(&adapted.draw_list);
                self.backend.end_frame();
                if let Some(region) = cleared {
                    return Ok(Some(RfvpRenderResult {
                        hit_proxies,
                        region,
                    }));
                }
            }
            return Ok(None);
        }

        let damage = self
            .frame_cache
            .as_ref()
            .and_then(|cache| self.frame_damage(cache, &commands, &generations));

        // A change confined to fully clipped commands produces empty damage:
        // nothing to present, but the cache must still advance.
        if damage.is_some_and(|rect| rect[2] <= 0.0 || rect[3] <= 0.0) {
            self.frame_cache = Some(FrameCache {
                signature,
                commands,
                generations,
            });
            return Ok(None);
        }

        self.backend
            .begin_frame(FrameTarget::Main)
            .map_err(ExternalRendererError::Backend)?;
        // The damage path clears only the damage rect inside the backend; a
        // full clear here would erase undamaged content.
        if damage.is_none() {
            self.backend.clear(self.clear_color);
        }
        let region = match (damage, self.damage_visualization) {
            (Some(rect), true) => self
                .backend
                .render_damage_visualized(&adapted.draw_list, rect),
            (Some(rect), false) => self.backend.render_damage(&adapted.draw_list, rect),
            (None, true) => self.backend.render_visualized(&adapted.draw_list),
            (None, false) => self.backend.render(&adapted.draw_list),
        };
        self.backend.end_frame();

        self.frame_cache = Some(FrameCache {
            signature,
            commands,
            generations,
        });

        Ok(Some(RfvpRenderResult {
            hit_proxies,
            region,
        }))
    }

    pub fn readback_rgba(&mut self, extent: Extent2D) -> Result<Vec<u8>, ExternalRendererError> {
        self.backend
            .readback_owned(FrameTarget::Main, extent)
            .map_err(ExternalRendererError::Backend)
    }

    fn sync_texture_creates(
        &mut self,
        textures: &[RecordedTextureCreate],
    ) -> Result<(), ExternalRendererError> {
        for upload in textures {
            self.sync_texture_create(upload)?;
        }

        Ok(())
    }

    fn sync_texture_commands(
        &mut self,
        commands: &[RecordedTextureCommand],
    ) -> Result<(), ExternalRendererError> {
        for command in commands {
            match command {
                RecordedTextureCommand::Create(texture) => self.sync_texture_create(texture)?,
                RecordedTextureCommand::Update(update) => self.sync_texture_update(update)?,
                RecordedTextureCommand::Destroy(destroy) => self.sync_texture_destroy(*destroy),
            }
        }
        Ok(())
    }

    fn sync_texture_create(
        &mut self,
        upload: &RecordedTextureCreate,
    ) -> Result<(), ExternalRendererError> {
        let cached = self.textures.get(&upload.handle).copied();
        // Generation 0 means the producer does not track generations; such
        // creates always carry fresh pixels and must not be deduplicated.
        if upload.generation != 0
            && cached.is_some_and(|cached| {
                cached.format == upload.desc.format
                    && cached.info.width == u32::from(upload.desc.width)
                    && cached.info.height == u32::from(upload.desc.height)
                    && cached.generation == upload.generation
            })
        {
            return Ok(());
        }

        let (width, height, rgba) = texture_rgba(upload)?;
        let same_layout = cached.is_some_and(|cached| {
            cached.format == upload.desc.format
                && cached.info.width == width
                && cached.info.height == height
        });

        let texture = if let Some(cached) = cached.filter(|_| same_layout) {
            self.backend
                .update_texture(
                    cached.texture,
                    TextureUpdate {
                        origin: [0, 0],
                        extent: Extent2D::new(width, height),
                        data: TextureData::Rgba8(&rgba),
                    },
                )
                .map_err(ExternalRendererError::Backend)?;
            cached.texture
        } else {
            if let Some(cached) = cached {
                self.backend.destroy_texture(cached.texture);
            }
            self.backend
                .create_texture(
                    &format!("rfvp-texture-{}", upload.handle.0),
                    TextureDesc::sampled_rgba8(width, height),
                    TextureData::Rgba8(&rgba),
                )
                .map_err(ExternalRendererError::Backend)?
        };

        let info = TextureInfo { width, height };
        self.textures.insert(
            upload.handle,
            CachedTexture {
                texture,
                info,
                format: upload.desc.format,
                generation: upload.generation,
            },
        );
        self.adapter
            .bindings_mut()
            .insert(crate::TextureHandle(upload.handle.0), texture, info);
        Ok(())
    }

    fn sync_texture_update(
        &mut self,
        update: &RecordedTextureUpdate,
    ) -> Result<(), ExternalRendererError> {
        let Some(cached) = self.textures.get(&update.handle).copied() else {
            return Err(ExternalRendererError::UnknownTexture(update.handle.0));
        };
        validate_texture_rect(cached, update.handle.0, update.rect)?;
        if cached.format != update.format {
            return Err(ExternalRendererError::UnsupportedTextureFormat(
                update.format,
            ));
        }

        let extent = Extent2D::new(update.rect.width, update.rect.height);
        let rgba = texture_patch_rgba(
            update.handle.0,
            cached.format,
            &update.pixels,
            update.rect.width,
            update.rect.height,
        )?;
        self.backend
            .update_texture(
                cached.texture,
                TextureUpdate {
                    origin: [update.rect.x, update.rect.y],
                    extent,
                    data: TextureData::Rgba8(&rgba),
                },
            )
            .map_err(ExternalRendererError::Backend)?;
        if let Some(cached) = self.textures.get_mut(&update.handle) {
            cached.generation = update.generation;
        }
        Ok(())
    }

    fn sync_texture_destroy(&mut self, destroy: RecordedTextureDestroy) {
        if let Some(cached) = self.textures.remove(&destroy.handle) {
            self.backend.destroy_texture(cached.texture);
        }
        self.adapter
            .bindings_mut()
            .remove(crate::TextureHandle(destroy.handle.0));
    }

    /// Conservative damage rect for a changed frame, in stage pixels.
    /// Positional diff over the adapted draw list (RFVP's prim-tree traversal
    /// keeps command order stable within a scene); any structural change or
    /// ambiguity falls back to a full repaint.
    fn frame_damage(
        &self,
        cache: &FrameCache,
        commands: &[CommandState],
        generations: &HashMap<u32, u64>,
    ) -> Option<[f32; 4]> {
        if cache.commands.len() != commands.len() {
            return None;
        }
        let changed_textures: HashSet<TextureId> = generations
            .iter()
            .filter(|(handle, generation)| cache.generations.get(*handle) != Some(generation))
            .filter_map(|(handle, _)| {
                self.adapter
                    .bindings()
                    .get(crate::TextureHandle(*handle))
                    .map(|binding| binding.texture)
            })
            .collect();

        let mut damage: Option<[f32; 4]> = None;
        for (index, command) in commands.iter().enumerate() {
            let previous = cache.commands[index];
            if previous.hash == command.hash && !changed_textures.contains(&command.texture) {
                continue;
            }
            damage = Some(match damage {
                Some(rect) => union_rect(union_rect(rect, previous.bbox), command.bbox),
                None => union_rect(previous.bbox, command.bbox),
            });
        }

        let [x, y, width, height] = damage?;
        let (stage_width, stage_height) = self.stage_extent();
        let x0 = (x - 2.0).floor().max(0.0);
        let y0 = (y - 2.0).floor().max(0.0);
        let x1 = (x + width + 2.0).ceil().min(stage_width as f32);
        let y1 = (y + height + 2.0).ceil().min(stage_height as f32);
        if x1 <= x0 || y1 <= y0 {
            return Some([0.0, 0.0, 0.0, 0.0]);
        }
        let rect = [x0, y0, x1 - x0, y1 - y0];
        let stage_area = stage_width as f32 * stage_height as f32;
        // A near-full damage rect costs more to scissor than to repaint.
        if rect[2] * rect[3] >= stage_area * 0.8 {
            return None;
        }
        Some(rect)
    }

    fn stage_extent(&self) -> (u32, u32) {
        self.backend
            .render_dimensions()
            .map(|dimensions| (dimensions.render_size.width, dimensions.render_size.height))
            .unwrap_or((0, 0))
    }
}

fn union_rect(left: [f32; 4], right: [f32; 4]) -> [f32; 4] {
    let x0 = left[0].min(right[0]);
    let y0 = left[1].min(right[1]);
    let x1 = (left[0] + left[2]).max(right[0] + right[2]);
    let y1 = (left[1] + left[3]).max(right[1] + right[3]);
    [x0, y0, x1 - x0, y1 - y0]
}

fn command_state(command: &DrawCommand) -> CommandState {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hash_f32_slice(&mut hasher, &command.transform.matrix2.x_axis.to_array());
    hash_f32_slice(&mut hasher, &command.transform.matrix2.y_axis.to_array());
    hash_f32_slice(&mut hasher, &command.transform.translation.to_array());
    command.texture.0.hash(&mut hasher);
    command.size.width.hash(&mut hasher);
    command.size.height.hash(&mut hasher);
    command.opacity.to_bits().hash(&mut hasher);
    (command.blend as u8).hash(&mut hasher);
    hash_f32_slice(&mut hasher, &command.color.multiply);
    command.color.grayscale.hash(&mut hasher);
    command.color.negative.hash(&mut hasher);
    hash_f32_slice(&mut hasher, &command.clip.uv_offset);
    hash_f32_slice(&mut hasher, &command.clip.uv_scale);
    hash_f32_slice(&mut hasher, &command.clip.quad_size);
    if let Some(bounds) = &command.clip_bounds {
        hash_f32_slice(&mut hasher, bounds);
    }
    if let Some(shader) = &command.shader {
        shader.name.hash(&mut hasher);
        for (key, values) in &shader.uniforms {
            key.hash(&mut hasher);
            hash_f32_slice(&mut hasher, values);
        }
        shader.mask_texture.map(|id| id.0).hash(&mut hasher);
        shader.user_texture.map(|id| id.0).hash(&mut hasher);
    }
    if let Some(mesh) = &command.mesh {
        for vertex in mesh.vertices.iter() {
            hash_f32_slice(&mut hasher, vertex);
        }
    }
    CommandState {
        hash: hasher.finish(),
        texture: command.texture,
        bbox: command_bbox(command),
    }
}

fn hash_f32_slice(hasher: &mut impl Hasher, values: &[f32]) {
    for value in values {
        value.to_bits().hash(hasher);
    }
}

/// Screen-space bbox of a draw command in stage pixels, clipped to
/// `clip_bounds` when present.
fn command_bbox(command: &DrawCommand) -> [f32; 4] {
    let (mut x0, mut y0, mut x1, mut y1) = if let Some(mesh) = &command.mesh {
        let mut bounds = (
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        );
        for vertex in mesh.vertices.iter() {
            bounds.0 = bounds.0.min(vertex[0]);
            bounds.1 = bounds.1.min(vertex[1]);
            bounds.2 = bounds.2.max(vertex[0]);
            bounds.3 = bounds.3.max(vertex[1]);
        }
        bounds
    } else {
        let [width, height] = command.clip.quad_size;
        let corners = [
            glam::Vec2::new(0.0, 0.0),
            glam::Vec2::new(width, 0.0),
            glam::Vec2::new(0.0, height),
            glam::Vec2::new(width, height),
        ];
        let mut bounds = (
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        );
        for corner in corners {
            let point = command.transform.transform_point2(corner);
            bounds.0 = bounds.0.min(point.x);
            bounds.1 = bounds.1.min(point.y);
            bounds.2 = bounds.2.max(point.x);
            bounds.3 = bounds.3.max(point.y);
        }
        bounds
    };
    if let Some([cx, cy, cw, ch]) = command.clip_bounds {
        x0 = x0.max(cx);
        y0 = y0.max(cy);
        x1 = x1.min(cx + cw);
        y1 = y1.min(cy + ch);
    }
    if x1 <= x0 || y1 <= y0 {
        return [0.0, 0.0, 0.0, 0.0];
    }
    [x0, y0, x1 - x0, y1 - y0]
}

fn frame_signature(commands: &[CommandState], generations: &HashMap<u32, u64>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    commands.len().hash(&mut hasher);
    for command in commands {
        command.hash.hash(&mut hasher);
    }
    let mut generations: Vec<(u32, u64)> = generations
        .iter()
        .map(|(handle, generation)| (*handle, *generation))
        .collect();
    generations.sort_unstable();
    generations.hash(&mut hasher);
    hasher.finish()
}

fn texture_rgba(
    upload: &RecordedTextureCreate,
) -> Result<(u32, u32, Vec<u8>), ExternalRendererError> {
    let width = u32::from(upload.desc.width);
    let height = u32::from(upload.desc.height);

    match upload.desc.format {
        TextureFormat::Rgba8 => {
            let expected = rgba_len(width, height, upload.desc.width, upload.desc.height)?;
            if upload.pixels.len() != expected {
                return Err(ExternalRendererError::InvalidTextureData {
                    handle: upload.handle.0,
                    format: upload.desc.format,
                    expected,
                    actual: upload.pixels.len(),
                });
            }
            Ok((width, height, upload.pixels.clone()))
        }
        TextureFormat::LumaA8 => {
            let expected = usize::try_from(width)
                .ok()
                .and_then(|width| width.checked_mul(usize::try_from(height).ok()?))
                .and_then(|pixels| pixels.checked_mul(2))
                .ok_or(ExternalRendererError::TextureSizeOverflow {
                    width: upload.desc.width,
                    height: upload.desc.height,
                })?;
            if upload.pixels.len() != expected {
                return Err(ExternalRendererError::InvalidTextureData {
                    handle: upload.handle.0,
                    format: upload.desc.format,
                    expected,
                    actual: upload.pixels.len(),
                });
            }

            let mut rgba = Vec::with_capacity(expected.saturating_mul(2));
            for pixel in upload.pixels.chunks_exact(2) {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]);
            }
            Ok((width, height, rgba))
        }
        format => Err(ExternalRendererError::UnsupportedTextureFormat(format)),
    }
}

fn texture_patch_rgba(
    handle: u32,
    format: TextureFormat,
    pixels: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<u8>, ExternalRendererError> {
    let expected = pixel_bytes(format, width, height)?;
    if pixels.len() != expected {
        return Err(ExternalRendererError::InvalidTextureData {
            handle,
            format,
            expected,
            actual: pixels.len(),
        });
    }

    match format {
        TextureFormat::Rgba8 => Ok(pixels.to_vec()),
        TextureFormat::LumaA8 => {
            let mut rgba = Vec::with_capacity(expected.saturating_mul(2));
            for pixel in pixels.chunks_exact(2) {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]);
            }
            Ok(rgba)
        }
        format => Err(ExternalRendererError::UnsupportedTextureFormat(format)),
    }
}

fn validate_texture_rect(
    cached: CachedTexture,
    handle: u32,
    rect: TextureRect,
) -> Result<(), ExternalRendererError> {
    let right = rect.x.saturating_add(rect.width);
    let bottom = rect.y.saturating_add(rect.height);
    if rect.width == 0
        || rect.height == 0
        || right > cached.info.width
        || bottom > cached.info.height
    {
        return Err(ExternalRendererError::TextureUpdateOutOfBounds {
            handle,
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
            texture_width: cached.info.width,
            texture_height: cached.info.height,
        });
    }
    Ok(())
}

fn pixel_bytes(
    format: TextureFormat,
    width: u32,
    height: u32,
) -> Result<usize, ExternalRendererError> {
    let bytes_per_pixel = match format {
        TextureFormat::Rgba8 => 4usize,
        TextureFormat::LumaA8 => 2usize,
        format => return Err(ExternalRendererError::UnsupportedTextureFormat(format)),
    };
    usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(usize::try_from(height).ok()?))
        .and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
        .ok_or(ExternalRendererError::TextureSizeOverflow {
            width: u16::try_from(width).unwrap_or(u16::MAX),
            height: u16::try_from(height).unwrap_or(u16::MAX),
        })
}

fn rgba_len(
    width: u32,
    height: u32,
    source_width: u16,
    source_height: u16,
) -> Result<usize, ExternalRendererError> {
    usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(usize::try_from(height).ok()?))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(ExternalRendererError::TextureSizeOverflow {
            width: source_width,
            height: source_height,
        })
}
