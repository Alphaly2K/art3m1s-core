//! Host-owned rendering adapter for draft Artemis sprite frames.

use super::frame::{FrameLease, FrameTexture, SpriteBlend};
use super::runtime::RuntimeHandle;
use art3m1s_render::backend::{GpuBackend, RenderRegion, TextureData, TextureDesc};
use art3m1s_render::draw::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawList, LayerCommandKind, TextureId,
    TextureInfo,
};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TextureKey {
    runtime: RuntimeHandle,
    epoch: u64,
    revision: u64,
    mask_revision: Option<u64>,
}

/// Draws CPU sprites with a backend supplied and configured by the Host.
/// The adapter owns the backend so cached handles remain tied to that backend.
pub struct SpriteRenderer<B> {
    backend: B,
    textures: HashMap<TextureKey, TextureId>,
}

impl<B: GpuBackend> SpriteRenderer<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            textures: HashMap::new(),
        }
    }

    /// Set up the target, context and viewport before drawing. Backend methods
    /// that replace or evict textures require clearing the adapter's cache first.
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Upload and draw in the frame the Host has already begun.
    pub fn render(&mut self, frame: &FrameLease) -> Result<RenderRegion, String> {
        let mut live = HashSet::new();
        let mut uploaded = Vec::new();
        let result = prepare_draw_list(frame, |image, mask| {
            let key = TextureKey {
                runtime: frame.runtime(),
                epoch: frame.session_epoch,
                revision: image.revision,
                mask_revision: mask.map(|mask| mask.revision),
            };
            live.insert(key);
            if let Some(&texture) = self.textures.get(&key) {
                return Ok(texture);
            }
            let name = format!(
                "artemis-frame-{:016x}-{}-{}-{}",
                key.runtime.as_raw(),
                key.epoch,
                key.revision,
                key.mask_revision.unwrap_or(0)
            );
            let pixels = masked_pixels(image, mask)?;
            let texture = self.backend.create_texture(
                &name,
                TextureDesc::sampled_rgba8(image.width, image.height),
                TextureData::Rgba8(pixels.as_ref()),
            )?;
            self.textures.insert(key, texture);
            uploaded.push(key);
            Ok(texture)
        });
        match result {
            Ok(draw) => {
                let region = self.backend.render(&draw);
                self.textures.retain(|key, texture| {
                    let used = live.contains(key);
                    if !used {
                        self.backend.destroy_texture(*texture);
                    }
                    used
                });
                Ok(region)
            }
            Err(error) => {
                for key in &uploaded {
                    if let Some(texture) = self.textures.remove(key) {
                        self.backend.destroy_texture(texture);
                    }
                }
                Err(error)
            }
        }
    }

    /// Release the adapter's uploads while the Host's backend context is current.
    pub fn clear_cached_textures(&mut self) {
        for (_, texture) in self.textures.drain() {
            self.backend.destroy_texture(texture);
        }
    }
}

fn masked_pixels<'a>(
    image: &'a FrameTexture,
    mask: Option<&FrameTexture>,
) -> Result<Cow<'a, [u8]>, String> {
    let Some(mask) = mask else {
        return Ok(Cow::Borrowed(image.rgba()));
    };
    if (image.width, image.height) != (mask.width, mask.height) {
        return Err("sprite mask dimensions do not match its image".into());
    }
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(image.rgba().len())
        .map_err(|_| "cannot allocate masked pixels")?;
    pixels.extend_from_slice(image.rgba());
    for (dst, mask) in pixels.chunks_exact_mut(4).zip(mask.rgba().chunks_exact(4)) {
        dst[3] = (u16::from(dst[3]) * u16::from(mask[0]) / 255) as u8;
    }
    Ok(Cow::Owned(pixels))
}

fn prepare_draw_list(
    frame: &FrameLease,
    mut texture: impl FnMut(&FrameTexture, Option<&FrameTexture>) -> Result<TextureId, String>,
) -> Result<DrawList, String> {
    let mut draw = DrawList::new();
    for sprite in &frame.sprites {
        let image = frame
            .texture(sprite.texture)
            .ok_or("invalid sprite texture")?;
        let mask = sprite
            .mask
            .map(|index| frame.texture(index).ok_or("invalid sprite mask"))
            .transpose()?;
        draw.push_layer(
            &sprite.layer_id,
            LayerCommandKind::Visual,
            0,
            DrawCommand {
                texture: texture(image, mask)?,
                size: TextureInfo {
                    width: image.width,
                    height: image.height,
                },
                transform: glam::Affine2::from_cols_array(&sprite.transform),
                opacity: sprite.opacity,
                blend: match sprite.blend {
                    SpriteBlend::Alpha => BlendMode::Alpha,
                    SpriteBlend::Add => BlendMode::Add,
                    SpriteBlend::Screen => BlendMode::Screen,
                    SpriteBlend::Multiply => BlendMode::Multiply,
                },
                color: ColorFilter {
                    multiply: sprite.color_multiply,
                    grayscale: sprite.grayscale,
                    negative: sprite.negative,
                },
                clip: ClipRect {
                    uv_offset: sprite.uv_offset,
                    uv_scale: sprite.uv_scale,
                    quad_size: sprite.quad_size,
                },
                clip_bounds: sprite.clip_bounds,
                shader: None,
                mesh: None,
                stencil: None,
                native_emote: None,
            },
        );
    }
    Ok(draw)
}
