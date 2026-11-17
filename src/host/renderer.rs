//! Host-owned rendering adapter for draft Artemis sprite frames.

use super::frame::{FrameLease, FrameSprite, FrameTexture, SpriteBlend};
use super::runtime::RuntimeHandle;
use art3m1s_render::backend::{FrameCapture, GpuBackend, RenderRegion, TextureData, TextureDesc};
use art3m1s_render::draw::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawList, LayerCommandKind, ShaderEffect,
    TextureId, TextureInfo, TextureOrigin,
};
use art3m1s_render::types::Extent2D;
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
        self.render_with_clear_color(frame, [0.0, 0.0, 0.0, 1.0])
    }

    /// Use the same clear color for both scenes of a transition.
    pub fn render_with_clear_color(
        &mut self,
        frame: &FrameLease,
        clear_color: [f32; 4],
    ) -> Result<RenderRegion, String> {
        let mut live = HashSet::new();
        let mut uploaded = Vec::new();
        let mut upload = |image: &FrameTexture, mask: Option<&FrameTexture>| {
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
        };
        let prepared = (|| {
            let draw = prepare_draw_list(frame, &frame.sprites, &mut upload)?;
            let source = frame
                .transition
                .as_ref()
                .map(|transition| -> Result<_, String> {
                    let source = prepare_draw_list(frame, &transition.source, &mut upload)?;
                    let shader = transition
                        .rule
                        .map(|rule| -> Result<_, String> {
                            let image = frame
                                .texture(rule)
                                .ok_or("invalid transition rule texture")?;
                            let texture = upload(image, None)?;
                            Ok(ShaderEffect {
                                name: art3m1s_render::shader::RULE_TRANS_SHADER.into(),
                                uniforms: [
                                    ("progress".into(), vec![transition.progress]),
                                    ("vague".into(), vec![transition.vague]),
                                ]
                                .into(),
                                mask_texture: Some(texture),
                                user_texture: None,
                            })
                        })
                        .transpose()?;
                    Ok((source, transition.progress, shader))
                })
                .transpose()?;
            Ok::<_, String>((draw, source))
        })();
        let result = prepared.and_then(|(draw, source)| {
            if let Some((source, progress, shader)) = source {
                self.render_transition(
                    frame.stage_size,
                    draw,
                    source,
                    (progress, shader),
                    clear_color,
                )
            } else {
                Ok(self.backend.render(&draw))
            }
        });
        match result {
            Ok(region) => {
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

    fn render_transition(
        &mut self,
        stage_size: [u32; 2],
        mut draw: DrawList,
        source: DrawList,
        blend: (f32, Option<ShaderEffect>),
        clear_color: [f32; 4],
    ) -> Result<RenderRegion, String> {
        self.backend.clear(clear_color);
        self.backend.render(&source);
        let extent = self
            .backend
            .render_dimensions()
            .map_or(Extent2D::new(stage_size[0], stage_size[1]), |size| {
                size.render_size
            });
        let name = ":host/transition-capture";
        let (texture, info, origin) = match self.backend.capture_frame(name, extent) {
            FrameCapture::Texture(texture, info, origin) => (texture, info, origin),
            FrameCapture::Pixels(pixels) if Some(pixels.len()) == extent.rgba8_len() => {
                let texture = self.backend.create_texture(
                    name,
                    TextureDesc::sampled_rgba8(extent.width, extent.height),
                    TextureData::Rgba8(&pixels),
                )?;
                (
                    texture,
                    TextureInfo {
                        width: extent.width,
                        height: extent.height,
                    },
                    TextureOrigin::TopLeft,
                )
            }
            FrameCapture::Pixels(_) => return Err("cannot capture the transition source".into()),
        };
        let mut clip = ClipRect::full(info);
        clip.quad_size = stage_size.map(|size| size as f32);
        if origin == TextureOrigin::BottomLeft {
            clip.uv_offset[1] = 1.0;
            clip.uv_scale[1] = -1.0;
        }
        draw.push(DrawCommand {
            texture,
            size: info,
            transform: glam::Affine2::IDENTITY,
            opacity: if blend.1.is_some() {
                1.0
            } else {
                1.0 - blend.0
            },
            blend: BlendMode::Alpha,
            color: ColorFilter::default(),
            clip,
            clip_bounds: None,
            shader: blend.1,
            mesh: None,
            stencil: None,
            native_emote: None,
        });
        self.backend.clear(clear_color);
        self.backend.render(&draw);
        self.backend.destroy_texture(texture);
        Ok(RenderRegion::Full)
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
    sprites: &[FrameSprite],
    mut texture: impl FnMut(&FrameTexture, Option<&FrameTexture>) -> Result<TextureId, String>,
) -> Result<DrawList, String> {
    let mut draw = DrawList::new();
    for sprite in sprites {
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
