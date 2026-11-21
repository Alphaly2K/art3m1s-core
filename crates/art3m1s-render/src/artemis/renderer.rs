//! Host-owned rendering adapter for draft Artemis sprite frames.

use super::frame::{
    FrameGroup, FrameShaderEffect, FrameShaderLanguage, FrameSprite, FrameTransition, SpriteBlend,
};
use super::packet::{FramePacket, FrameTextureDescriptor};
use crate::backend::{FrameCapture, GpuBackend, RenderRegion, TextureData, TextureDesc};
use crate::draw::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawList, DrawMesh, LayerCommandKind,
    ShaderEffect, ShaderGroup, TextureId, TextureInfo, TextureOrigin,
};
use crate::types::Extent2D;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TextureKey {
    runtime: u64,
    epoch: u64,
    revision: u64,
    mask_revision: Option<u64>,
}

/// A typed scene for callers that already hold decoded drawing instructions.
pub struct FramePlan<'a> {
    pub runtime: u64,
    pub epoch: u64,
    pub stage_size: [u32; 2],
    pub sprites: &'a [FrameSprite],
    pub groups: &'a [FrameGroup],
    pub masks: &'a [FrameSprite],
    pub transition: Option<&'a FrameTransition>,
}

/// Draws CPU sprites with a backend supplied and configured by the Host.
/// The adapter owns the backend so cached handles remain tied to that backend.
pub struct SpriteRenderer<B> {
    backend: B,
    textures: HashMap<TextureKey, TextureId>,
    shaders: HashMap<(u64, u64, u64), Option<String>>,
}

impl<B: GpuBackend> SpriteRenderer<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            textures: HashMap::new(),
            shaders: HashMap::new(),
        }
    }

    /// Set up the target, context and viewport before drawing. Backend methods
    /// that replace or evict textures require clearing the adapter's cache first.
    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Draw an ABI packet in the Host's active target. Pixel borrows must be
    /// from this packet's frame token, which must remain live for this call.
    /// `runtime` identifies the source runtime within this renderer's cache.
    pub fn render_packet<'a>(
        &mut self,
        runtime: u64,
        packet: &FramePacket,
        pixels: impl FnMut(usize) -> Result<&'a [u8], String>,
        clear_color: [f32; 4],
    ) -> Result<RenderRegion, String> {
        self.render_plan(
            FramePlan {
                runtime,
                epoch: packet.session_epoch,
                stage_size: packet.stage_size,
                sprites: &packet.sprites,
                groups: &packet.groups,
                masks: &packet.masks,
                transition: packet.transition.as_ref(),
            },
            |index| packet.textures().get(index).copied(),
            pixels,
            clear_color,
        )
    }

    /// Draw a typed frame with the same guarantees as [`Self::render_packet`].
    pub fn render_plan<'a>(
        &mut self,
        frame: FramePlan<'_>,
        descriptor: impl Fn(usize) -> Option<FrameTextureDescriptor>,
        mut pixels: impl FnMut(usize) -> Result<&'a [u8], String>,
        clear_color: [f32; 4],
    ) -> Result<RenderRegion, String> {
        let shaders = self.prepare_shaders(&frame);
        let mut live = HashSet::new();
        let mut uploaded = Vec::new();
        let mut upload = |index,
                          mask_index: Option<usize>|
         -> Result<(TextureId, TextureInfo), String> {
            let image = descriptor(index).ok_or("missing frame texture descriptor")?;
            let mask = mask_index
                .map(|index| descriptor(index).ok_or("missing frame mask descriptor"))
                .transpose()?;
            if mask.is_some_and(|mask| (image.width, image.height) != (mask.width, mask.height)) {
                return Err("sprite mask dimensions do not match its image".into());
            }
            let size = TextureInfo {
                width: image.width,
                height: image.height,
            };
            let key = TextureKey {
                runtime: frame.runtime,
                epoch: frame.epoch,
                revision: image.revision,
                mask_revision: mask.map(|mask| mask.revision),
            };
            live.insert(key);
            if let Some(&texture) = self.textures.get(&key) {
                return Ok((texture, size));
            }
            let name = format!(
                "artemis-frame-{:016x}-{}-{}-{}",
                key.runtime,
                key.epoch,
                key.revision,
                key.mask_revision.unwrap_or(0)
            );
            let rgba = pixels(index)?;
            let expected = image.rgba_len().ok_or("frame texture size overflow")?;
            if rgba.len() != expected {
                return Err("frame texture byte length does not match its descriptor".into());
            }
            let mask = mask_index.map(&mut pixels).transpose()?;
            let rgba = masked_pixels(rgba, mask)?;
            let texture = self.backend.create_texture(
                &name,
                TextureDesc::sampled_rgba8(image.width, image.height),
                TextureData::Rgba8(rgba.as_ref()),
            )?;
            self.textures.insert(key, texture);
            uploaded.push(key);
            Ok((texture, size))
        };
        let prepared = (|| {
            let draw = prepare_draw_list(
                frame.sprites,
                frame.groups,
                frame.masks,
                &shaders,
                &mut upload,
            )?;
            let source = frame
                .transition
                .map(|transition| -> Result<_, String> {
                    let source = prepare_draw_list(
                        &transition.source,
                        &transition.source_groups,
                        &transition.source_masks,
                        &shaders,
                        &mut upload,
                    )?;
                    let shader = transition
                        .rule
                        .map(|rule| -> Result<_, String> {
                            let (texture, _) = upload(rule, None)?;
                            Ok(ShaderEffect {
                                name: crate::shader::RULE_TRANS_SHADER.into(),
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
                Ok(self.backend.render_with_clear_color(&draw, clear_color))
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

    fn prepare_shaders(&mut self, frame: &FramePlan<'_>) -> HashMap<u64, String> {
        let mut live = HashSet::new();
        let mut names = HashMap::new();
        let mut prepare = |effect: &FrameShaderEffect| {
            let revision = effect.shader.revision;
            let key = (frame.runtime, frame.epoch, revision);
            if !live.insert(key) {
                return;
            }
            let name = self.shaders.entry(key).or_insert_with(|| {
                let name = format!("artemis-shader-{:016x}-{}-{revision}", key.0, key.1);
                let source = &effect.shader.source;
                let result = match effect.shader.language {
                    FrameShaderLanguage::Hlsl => self.backend.register_hlsl_shader(&name, source),
                    FrameShaderLanguage::Glsl => self.backend.register_glsl_shader(&name, source),
                };
                match result {
                    Ok(_) => Some(name),
                    Err(error) => {
                        log::warn!(
                            "{:?} shader revision {revision} could not compile: {error}",
                            effect.shader.language
                        );
                        None
                    }
                }
            });
            if let Some(name) = name {
                names.insert(revision, name.clone());
            }
        };
        let scene = |sprites: &[FrameSprite],
                     groups: &[FrameGroup],
                     prepare: &mut dyn FnMut(&FrameShaderEffect)| {
            for effect in sprites
                .iter()
                .filter_map(|sprite| sprite.shader.as_ref())
                .chain(groups.iter().filter_map(|group| group.shader.as_ref()))
            {
                prepare(effect);
            }
        };
        scene(frame.sprites, frame.groups, &mut prepare);
        if let Some(old) = frame.transition {
            scene(&old.source, &old.source_groups, &mut prepare);
        }
        self.shaders.retain(|key, name| {
            if live.contains(key) {
                return true;
            }
            if let Some(name) = name {
                self.backend.unregister_shader(name);
            }
            false
        });
        names
    }

    fn render_transition(
        &mut self,
        stage_size: [u32; 2],
        mut draw: DrawList,
        source: DrawList,
        blend: (f32, Option<ShaderEffect>),
        clear_color: [f32; 4],
    ) -> Result<RenderRegion, String> {
        self.backend.render_with_clear_color(&source, clear_color);
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
        let region = self.backend.render_with_clear_color(&draw, clear_color);
        self.backend.destroy_texture(texture);
        Ok(region)
    }

    /// Release the adapter's uploads while the Host's backend context is current.
    pub fn clear_cached_textures(&mut self) {
        for (_, texture) in self.textures.drain() {
            self.backend.destroy_texture(texture);
        }
    }

    /// Retry failed shader bindings on the next draw after preparing a program.
    pub fn retry_failed_shader_bindings(&mut self) {
        self.shaders.retain(|_, name| name.is_some());
    }

    /// Unregister the adapter's shader bindings.
    /// The backend may keep compiled programs for reuse.
    pub fn clear_cached_shaders(&mut self) {
        for name in self.shaders.drain().filter_map(|(_, name)| name) {
            self.backend.unregister_shader(&name);
        }
    }
}

fn masked_pixels<'a>(image: &'a [u8], mask: Option<&[u8]>) -> Result<Cow<'a, [u8]>, String> {
    let Some(mask) = mask else {
        return Ok(Cow::Borrowed(image));
    };
    if image.len() != mask.len() {
        return Err("frame mask byte length does not match its descriptor".into());
    }
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(image.len())
        .map_err(|_| "cannot allocate masked pixels")?;
    pixels.extend_from_slice(image);
    for (dst, mask) in pixels.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
        dst[3] = (u16::from(dst[3]) * u16::from(mask[0]) / 255) as u8;
    }
    Ok(Cow::Owned(pixels))
}

fn prepare_draw_list(
    sprites: &[FrameSprite],
    groups: &[FrameGroup],
    masks: &[FrameSprite],
    shaders: &HashMap<u64, String>,
    mut texture: impl FnMut(usize, Option<usize>) -> Result<(TextureId, TextureInfo), String>,
) -> Result<DrawList, String> {
    let mut draw = DrawList::new();
    let mut draw_sprite = |sprite: &FrameSprite| -> Result<DrawCommand, String> {
        let (handle, size) = texture(sprite.texture, sprite.mask)?;
        Ok(DrawCommand {
            texture: handle,
            size,
            transform: glam::Affine2::from_cols_array(&sprite.transform),
            opacity: sprite.opacity,
            blend: match sprite.blend {
                SpriteBlend::Alpha => BlendMode::Alpha,
                SpriteBlend::Add => BlendMode::Add,
                SpriteBlend::Screen => BlendMode::Screen,
                SpriteBlend::Multiply => BlendMode::Multiply,
                SpriteBlend::EmoteAdd => BlendMode::NativeAdd,
                SpriteBlend::EmoteReverseSubtract => BlendMode::NativeReverseSubtract,
                SpriteBlend::EmoteMultiply => BlendMode::NativeMultiply,
                SpriteBlend::EmoteScreen => BlendMode::NativeScreen,
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
            shader: sprite
                .shader
                .as_ref()
                .map(|effect| resolved_effect(effect, shaders, &mut texture))
                .transpose()?
                .flatten(),
            mesh: sprite.mesh.clone().map(|vertices| DrawMesh { vertices }),
            stencil: None,
            native_emote: sprite.emote_material.clone(),
        })
    };
    for sprite in sprites {
        draw.push_layer(
            &sprite.layer_id,
            LayerCommandKind::Visual,
            0,
            draw_sprite(sprite)?,
        );
    }
    draw.mask_commands = masks
        .iter()
        .map(&mut draw_sprite)
        .collect::<Result<_, _>>()?;
    for group in groups {
        if let Some(effect) = &group.shader {
            if let Some(effect) = resolved_effect(effect, shaders, &mut texture)? {
                draw.push_shader_group(ShaderGroup {
                    key: None,
                    start: group.sprites.start,
                    end: group.sprites.end,
                    effect,
                    clip_bounds: group.clip_bounds,
                    mask_range: None,
                });
            }
            continue;
        }
        if let Some(stencil) = &group.stencil {
            draw.push_shader_group(ShaderGroup {
                key: None,
                start: group.sprites.start,
                end: group.sprites.end,
                effect: ShaderEffect {
                    name: crate::ALPHA_MASK_SHADER.into(),
                    uniforms: Default::default(),
                    mask_texture: None,
                    user_texture: None,
                },
                clip_bounds: group.clip_bounds,
                mask_range: Some([stencil.start, stencil.end]),
            });
            continue;
        }
        let mask_texture = group
            .mask
            .map(|index| texture(index, None).map(|(texture, _)| texture))
            .transpose()?;
        let blend = match group.blend {
            SpriteBlend::Alpha => 0.0,
            SpriteBlend::Add => 1.0,
            SpriteBlend::Screen => 2.0,
            SpriteBlend::Multiply => 3.0,
            _ => return Err("invalid composite blend mode".into()),
        };
        draw.push_shader_group(ShaderGroup {
            key: None,
            start: group.sprites.start,
            end: group.sprites.end,
            effect: ShaderEffect {
                name: crate::GROUP_COMPOSITE_SHADER.into(),
                uniforms: [
                    ("alpha".into(), vec![group.opacity]),
                    ("colorMultiply".into(), group.color_multiply.to_vec()),
                    ("grayscale".into(), vec![u8::from(group.grayscale) as f32]),
                    ("negative".into(), vec![u8::from(group.negative) as f32]),
                    ("opaque".into(), vec![u8::from(group.opaque) as f32]),
                    ("blendMode".into(), vec![blend]),
                ]
                .into(),
                mask_texture,
                user_texture: None,
            },
            clip_bounds: group.clip_bounds,
            mask_range: None,
        });
    }
    Ok(draw)
}

fn resolved_effect(
    effect: &FrameShaderEffect,
    shaders: &HashMap<u64, String>,
    texture: &mut impl FnMut(usize, Option<usize>) -> Result<(TextureId, TextureInfo), String>,
) -> Result<Option<ShaderEffect>, String> {
    let Some(name) = shaders.get(&effect.shader.revision) else {
        return Ok(None);
    };
    let mut resolve = |index| texture(index, None).map(|(texture, _)| texture);
    Ok(Some(ShaderEffect {
        name: name.clone(),
        uniforms: effect.uniforms.clone(),
        mask_texture: effect.mask.map(&mut resolve).transpose()?,
        user_texture: effect.user.map(&mut resolve).transpose()?,
    }))
}
