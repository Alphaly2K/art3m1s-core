//! Siglus VM to Art3m1s GPU adapter. No Siglus-specific rendering code lives
//! in the engine fork beyond exposing its frame and image data.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use art3m1s_render::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawList, DrawMesh, FrameTarget, GpuBackend,
    TextureData, TextureDesc, TextureId, TextureInfo,
};
use siglus_scene_vm::host::{SiglusHost, SiglusHostConfig};
use siglus_scene_vm::image_manager::{ImageKey, ImageManager};
use siglus_scene_vm::layer::{
    RenderFrame, RenderSprite, Sprite, SpriteBlend, SpriteFit, SpriteSizeMode,
};
use siglus_scene_vm::render_math::sprite_quad_geometry_rect;

/// Probe only; constructing the host performs full game initialization.
pub fn is_siglus_project(path: &Path) -> bool {
    siglus_scene_vm::resource::find_scene_pck_path(path).is_ok()
        && siglus_scene_vm::resource::find_initial_gameexe_path(path).is_ok()
}

#[derive(Clone, Copy)]
struct CachedTexture {
    id: TextureId,
    version: u64,
    size: TextureInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ColorEffects {
    mono: u8,
    reverse: u8,
    bright: u8,
    dark: u8,
    color_rate: u8,
    color_add: [u8; 3],
    color_target: [u8; 3],
}

impl ColorEffects {
    fn from_sprite(s: &Sprite) -> Self {
        Self {
            mono: s.mono,
            reverse: s.reverse,
            bright: s.bright,
            dark: s.dark,
            color_rate: s.color_rate,
            color_add: [s.color_add_r, s.color_add_g, s.color_add_b],
            color_target: [s.color_r, s.color_g, s.color_b],
        }
    }

    fn is_identity(self) -> bool {
        self.mono == 0
            && self.reverse == 0
            && self.bright == 0
            && self.dark == 0
            && self.color_rate == 0
            && self.color_add == [0; 3]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum TextureKey {
    Image(ImageKey, ColorEffects),
    Solid(ColorEffects),
}

pub struct SiglusAdapter {
    host: SiglusHost,
    textures: HashMap<TextureKey, CachedTexture>,
    last_sprite_count: usize,
    last_command_count: usize,
}

impl SiglusAdapter {
    pub fn open(config: SiglusHostConfig) -> Result<Self> {
        Ok(Self {
            host: SiglusHost::new_external(config)?,
            textures: HashMap::new(),
            last_sprite_count: 0,
            last_command_count: 0,
        })
    }

    pub fn host(&mut self) -> &mut SiglusHost {
        &mut self.host
    }

    pub fn logical_size(&self) -> (u32, u32) {
        self.host.logical_size()
    }

    pub fn last_frame_counts(&self) -> (usize, usize) {
        (self.last_sprite_count, self.last_command_count)
    }

    /// Advance the VM and submit a new frame directly to the shared backend.
    /// A missing frame means the VM did not request a redraw this step.
    pub fn step(&mut self, dt_ms: u32, gpu: &mut dyn GpuBackend) -> Result<bool> {
        self.step_inner(dt_ms, gpu, true)
    }

    /// Headless diagnostics: draw into the backend's main target without
    /// presenting to a native window or shared texture surface.
    pub fn step_without_present(&mut self, dt_ms: u32, gpu: &mut dyn GpuBackend) -> Result<bool> {
        self.step_inner(dt_ms, gpu, false)
    }

    fn step_inner(&mut self, dt_ms: u32, gpu: &mut dyn GpuBackend, present: bool) -> Result<bool> {
        let exiting = self.host.step(dt_ms)?;
        if let Some(frame) = self.host.take_external_frame() {
            self.last_sprite_count = frame.submitted_sprite_count();
            let size = self.host.logical_size();
            let images = self.host.external_images();
            gpu.begin_access();
            let rendered: Result<()> = (|| {
                let mut used = HashSet::new();
                let draw = convert_frame(&frame, images, size, gpu, &mut self.textures, &mut used)?;
                self.last_command_count = draw.commands.len();
                gpu.begin_frame(FrameTarget::Main)
                    .map_err(anyhow::Error::msg)?;
                gpu.clear([0.0, 0.0, 0.0, 1.0]);
                let damage = gpu.render(&draw).damage();
                gpu.end_frame();
                if present {
                    gpu.present(damage).map_err(anyhow::Error::msg)?;
                }
                self.textures.retain(|key, cached| {
                    if used.contains(key) {
                        true
                    } else {
                        gpu.destroy_texture(cached.id);
                        false
                    }
                });
                Ok(())
            })();
            gpu.end_access();
            rendered?;
        }
        Ok(exiting)
    }

    /// The GPU owner must outlive this adapter. Call before replacing it.
    pub fn release_textures(&mut self, gpu: &mut dyn GpuBackend) {
        gpu.begin_access();
        for texture in self.textures.drain().map(|(_, cached)| cached.id) {
            gpu.destroy_texture(texture);
        }
        gpu.end_access();
    }
}

fn convert_frame(
    frame: &RenderFrame,
    images: &ImageManager,
    size: (u32, u32),
    gpu: &mut dyn GpuBackend,
    cache: &mut HashMap<TextureKey, CachedTexture>,
    used: &mut HashSet<TextureKey>,
) -> Result<DrawList> {
    if frame.wipe.is_some() {
        bail!("Siglus stage wipe requires a two-target compositor");
    }
    let mut draw = DrawList::new();
    for entry in &frame.sprites {
        let sprite = &entry.sprite;
        if !sprite.visible || sprite.alpha == 0 {
            continue;
        }
        validate_sprite(sprite)?;
        let effects = ColorEffects::from_sprite(sprite);
        let image_entry = sprite
            .image_id
            .as_ref()
            .map(|handle| {
                images
                    .get_entry(handle)
                    .context("Siglus frame references an unavailable image")
            })
            .transpose()?;
        let (key, version, texture_size, rgba): (_, _, _, &[u8]) = match &image_entry {
            Some((image, version)) => (
                TextureKey::Image(sprite.image_id.as_ref().unwrap().key(), effects),
                *version,
                TextureInfo {
                    width: image.width,
                    height: image.height,
                },
                &image.rgba,
            ),
            None => (
                TextureKey::Solid(effects),
                0,
                TextureInfo {
                    width: 1,
                    height: 1,
                },
                &[255, 255, 255, 255],
            ),
        };
        used.insert(key);
        if cache
            .get(&key)
            .is_none_or(|cached| cached.version != version)
        {
            let adjusted = (!effects.is_identity()).then(|| apply_color_effects(rgba, effects));
            let pixels = adjusted.as_deref().unwrap_or(rgba);
            let id = gpu
                .create_texture(
                    &format!("siglus-{:?}-{version}", key),
                    TextureDesc::sampled_rgba8(texture_size.width, texture_size.height),
                    TextureData::Rgba8(pixels),
                )
                .map_err(anyhow::Error::msg)?;
            if let Some(previous) = cache.insert(
                key,
                CachedTexture {
                    id,
                    version,
                    size: texture_size,
                },
            ) {
                gpu.destroy_texture(previous.id);
            }
        }
        let cached = cache[&key];
        let (texture, texture_size) = (cached.id, cached.size);
        if let Some(command) = map_sprite(entry, texture, texture_size, size)? {
            draw.push(command);
        }
    }
    Ok(draw)
}

fn apply_color_effects(rgba: &[u8], effects: ColorEffects) -> Vec<u8> {
    let mut pixels = rgba.to_vec();
    let mono = effects.mono as f32 / 255.0;
    let reverse = effects.reverse as f32 / 255.0;
    let bright = effects.bright as f32 / 255.0;
    let dark = effects.dark as f32 / 255.0;
    let color_rate = effects.color_rate as f32 / 255.0;
    for pixel in pixels.chunks_exact_mut(4) {
        let original = [
            pixel[0] as f32 / 255.0,
            pixel[1] as f32 / 255.0,
            pixel[2] as f32 / 255.0,
        ];
        let gray = original[0] * 0.2989 + original[1] * 0.5886 + original[2] * 0.1145;
        for channel in 0..3 {
            let mut value = original[channel];
            value = value * (1.0 - reverse) + (1.0 - value) * reverse;
            value = value * (1.0 - mono) + gray * mono;
            value += bright - dark;
            value = value * (1.0 - color_rate)
                + (effects.color_target[channel] as f32 / 255.0) * color_rate;
            value += effects.color_add[channel] as f32 / 255.0;
            pixel[channel] = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    pixels
}

fn validate_sprite(s: &Sprite) -> Result<()> {
    // Siglus's ordinary 2D alpha test discards alpha < 1/255. With this
    // adapter's color-only composition, zero-alpha texels have the same
    // visible result under alpha blending; depth/shadow paths stay rejected.
    if s.emote_render.is_some()
        || s.mask_image_id.is_some()
        || s.tonecurve_image_id.is_some()
        || s.wipe_src_image_id.is_some()
        || s.fog_texture_image_id.is_some()
        || s.mesh_kind != 0
        || s.billboard
        || s.camera_enabled
        || s.z.abs() > f32::EPSILON
        || s.pivot_z.abs() > f32::EPSILON
        || (s.scale_z - 1.0).abs() > 1e-6
        || s.rotate_x.abs() > f32::EPSILON
        || s.rotate_y.abs() > f32::EPSILON
        || s.light_enabled
        || s.fog_enabled
        || s.wipe_fx_mode != 0
        || s.mask_mode != 0
        || !s.alpha_blend
    {
        bail!(
            "Siglus unsupported sprite effect: emote={} mask={} tonecurve={} wipe_source={} fog_texture={} mesh_kind={} billboard={} camera={} z={} pivot_z={} scale_z={} rotate_x={} rotate_y={} light={} fog={} wipe_mode={} mask_mode={} alpha_test={} alpha_blend={}",
            s.emote_render.is_some(),
            s.mask_image_id.is_some(),
            s.tonecurve_image_id.is_some(),
            s.wipe_src_image_id.is_some(),
            s.fog_texture_image_id.is_some(),
            s.mesh_kind,
            s.billboard,
            s.camera_enabled,
            s.z,
            s.pivot_z,
            s.scale_z,
            s.rotate_x,
            s.rotate_y,
            s.light_enabled,
            s.fog_enabled,
            s.wipe_fx_mode,
            s.mask_mode,
            s.alpha_test,
            s.alpha_blend,
        );
    }
    if !matches!(
        s.blend,
        SpriteBlend::Normal | SpriteBlend::Add | SpriteBlend::Mul | SpriteBlend::Screen
    ) {
        bail!("Siglus sprite blend mode is unsupported");
    }
    Ok(())
}

fn map_sprite(
    entry: &RenderSprite,
    texture: TextureId,
    texture_size: TextureInfo,
    viewport: (u32, u32),
) -> Result<Option<DrawCommand>> {
    let s = &entry.sprite;
    let sw = texture_size.width.max(1) as f32;
    let sh = texture_size.height.max(1) as f32;
    let vw = viewport.0.max(1) as f32;
    let vh = viewport.1.max(1) as f32;
    let (dx, dy, l, t, r, b, u0, v0, u1, v1) = match s.fit {
        SpriteFit::FullScreen => {
            let (l, t, r, b) = source_clip(s, sw, sh);
            (0.0, 0.0, 0.0, 0.0, vw, vh, l / sw, t / sh, r / sw, b / sh)
        }
        SpriteFit::PixelRect => {
            let (lw, lh) = match s.size_mode {
                SpriteSizeMode::Intrinsic => (sw, sh),
                SpriteSizeMode::Explicit { width, height } => {
                    (width.max(1) as f32, height.max(1) as f32)
                }
            };
            let Some((l, t, r, b)) = local_clip(s, lw, lh) else {
                return Ok(None);
            };
            (
                s.x as f32,
                s.y as f32,
                l,
                t,
                r,
                b,
                l / lw,
                t / lh,
                r / lw,
                b / lh,
            )
        }
    };
    let geometry = sprite_quad_geometry_rect(s, dx, dy, l, t, r, b, vw, vh)
        .context("Siglus sprite projection failed")?;
    let p = geometry.projected;
    let vertices = [
        [p[0].x, p[0].y, u0, v0],
        [p[1].x, p[1].y, u1, v0],
        [p[2].x, p[2].y, u1, v1],
        [p[0].x, p[0].y, u0, v0],
        [p[2].x, p[2].y, u1, v1],
        [p[3].x, p[3].y, u0, v1],
    ];
    let blend = match s.blend {
        SpriteBlend::Normal => BlendMode::Alpha,
        SpriteBlend::Add => BlendMode::Add,
        SpriteBlend::Mul => BlendMode::Multiply,
        SpriteBlend::Screen => BlendMode::Screen,
        _ => unreachable!("validated above"),
    };
    Ok(Some(DrawCommand {
        texture,
        size: texture_size,
        transform: glam::Affine2::IDENTITY,
        opacity: (s.alpha as f32 / 255.0) * (s.tr as f32 / 255.0),
        blend,
        color: ColorFilter::default(),
        clip: ClipRect::full(texture_size),
        clip_bounds: s.dst_clip.map(|c| {
            [
                c.left as f32,
                c.top as f32,
                (c.right - c.left) as f32,
                (c.bottom - c.top) as f32,
            ]
        }),
        shader: None,
        mesh: Some(DrawMesh {
            vertices: Arc::new(vertices),
        }),
        stencil: None,
        native_emote: None,
    }))
}

fn source_clip(s: &Sprite, w: f32, h: f32) -> (f32, f32, f32, f32) {
    if let Some(c) = s.src_clip {
        let l = (c.left as f32).clamp(0.0, w);
        let t = (c.top as f32).clamp(0.0, h);
        let r = (c.right as f32).clamp(0.0, w);
        let b = (c.bottom as f32).clamp(0.0, h);
        if r > l && b > t {
            return (l, t, r, b);
        }
    }
    (0.0, 0.0, w, h)
}

fn local_clip(s: &Sprite, w: f32, h: f32) -> Option<(f32, f32, f32, f32)> {
    let cx = s.pivot_x
        + if s.object_anchor {
            s.texture_center_x
        } else {
            0.0
        };
    let cy = s.pivot_y
        + if s.object_anchor {
            s.texture_center_y
        } else {
            0.0
        };
    let mut l = -cx;
    let mut t = -cy;
    let mut r = w - cx;
    let mut b = h - cy;
    if let Some(c) = s.src_clip {
        l = l.max(c.left as f32);
        t = t.max(c.top as f32);
        r = r.min(c.right as f32);
        b = b.min(c.bottom as f32);
    }
    (r > l && b > t).then_some((l + cx, t + cy, r + cx, b + cy))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_sprite_maps_to_direct_mesh() {
        let sprite = Sprite {
            visible: true,
            x: 12,
            y: 34,
            ..Sprite::default()
        };
        let entry = RenderSprite::new(None, None, sprite);
        let command = map_sprite(
            &entry,
            TextureId(7),
            TextureInfo {
                width: 20,
                height: 10,
            },
            (1280, 720),
        )
        .unwrap()
        .unwrap();
        let vertices = &command.mesh.unwrap().vertices;
        assert_eq!(vertices[0], [12.0, 34.0, 0.0, 0.0]);
        assert_eq!(vertices[2], [32.0, 44.0, 1.0, 1.0]);
    }

    #[test]
    fn unsupported_effect_fails_explicitly() {
        let sprite = Sprite {
            wipe_fx_mode: 1,
            ..Sprite::default()
        };
        assert!(validate_sprite(&sprite).is_err());
    }
}
