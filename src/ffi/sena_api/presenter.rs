use std::collections::HashMap;

use art3m1s_render::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawList, Extent2D, FrameTarget, GpuBackend,
    NativeSurface, SPRITE_NEAREST_SHADER, SPRITE_RESAMPLE_SHADER, ShaderEffect, TextureData,
    TextureDesc, TextureId, TextureInfo, TextureUpdate,
};
use glam::{Affine2, Vec2};
use pal_vm::scene::{
    DrawCommand as PalDraw, FrameScene, RectF, SceneTextureId, rasterize_scene_rgba,
};

use crate::backend::{BackendSelection, create_backend};

struct Texture {
    id: TextureId,
    generation: u64,
    size: TextureInfo,
}

pub(super) struct Presenter {
    gpu: Box<dyn GpuBackend>,
    textures: HashMap<SceneTextureId, Texture>,
    fallback: Option<TextureId>,
    extent: Extent2D,
}

impl Presenter {
    pub(super) fn new(scene: &FrameScene, surface: NativeSurface) -> Result<Self, String> {
        let extent = Extent2D::new(scene.logical_width, scene.logical_height);
        let mut gpu = create_backend(
            BackendSelection::PlatformDefault,
            extent.width,
            extent.height,
        )?;
        gpu.set_native_surface(surface)?;
        Ok(Self {
            gpu,
            textures: HashMap::new(),
            fallback: None,
            extent,
        })
    }

    pub(super) fn present(&mut self, scene: &FrameScene) -> Result<(), String> {
        self.gpu.begin_access();
        let result = self.draw(scene);
        self.gpu.end_access();
        result
    }

    fn draw(&mut self, scene: &FrameScene) -> Result<(), String> {
        let extent = Extent2D::new(scene.logical_width, scene.logical_height);
        if self.extent != extent {
            self.gpu.resize(extent)?;
            self.extent = extent;
            if let Some(id) = self.fallback.take() {
                self.gpu.destroy_texture(id);
            }
        }
        let mut list = DrawList::new();
        if scene.raster_stage.is_some()
            || scene.commands.iter().any(
                |command| matches!(command, PalDraw::Sprite(sprite) if sprite.raster_fx.is_some()),
            )
        {
            let pixels = rasterize_scene_rgba(scene);
            let size = TextureInfo {
                width: extent.width,
                height: extent.height,
            };
            let id = match self.fallback {
                Some(id) => {
                    self.gpu.update_texture(
                        id,
                        TextureUpdate {
                            origin: [0, 0],
                            extent,
                            data: TextureData::Rgba8(&pixels),
                        },
                    )?;
                    id
                }
                None => {
                    let id = self.gpu.create_texture(
                        "sena-raster",
                        TextureDesc::sampled_rgba8(size.width, size.height),
                        TextureData::Rgba8(&pixels),
                    )?;
                    self.fallback = Some(id);
                    id
                }
            };
            list.push(command(
                id,
                size,
                RectF::new(0., 0., size.width as f32, size.height as f32),
                RectF::new(0., 0., 1., 1.),
                [1.; 4],
                BlendMode::Opaque,
            ));
        } else {
            self.textures.retain(|key, texture| {
                let keep = scene.textures.iter().any(|item| item.id == *key);
                if !keep {
                    self.gpu.destroy_texture(texture.id);
                }
                keep
            });
            for texture in &scene.textures {
                let size = TextureInfo {
                    width: texture.width,
                    height: texture.height,
                };
                if let Some(old) = self
                    .textures
                    .get_mut(&texture.id)
                    .filter(|old| old.size == size)
                {
                    if old.generation != texture.generation {
                        self.gpu.update_texture(
                            old.id,
                            TextureUpdate {
                                origin: [0, 0],
                                extent: Extent2D::new(size.width, size.height),
                                data: TextureData::Rgba8(&texture.pixels),
                            },
                        )?;
                        old.generation = texture.generation;
                    }
                    continue;
                }
                let id = self.gpu.create_texture(
                    &format!("sena-sprite-{}", texture.id.0),
                    TextureDesc::sampled_rgba8(size.width, size.height),
                    TextureData::Rgba8(&texture.pixels),
                )?;
                if let Some(old) = self.textures.insert(
                    texture.id,
                    Texture {
                        id,
                        size,
                        generation: texture.generation,
                    },
                ) {
                    self.gpu.destroy_texture(old.id);
                }
            }
            for draw in &scene.commands {
                match draw {
                    PalDraw::Sprite(sprite)
                        if sprite.dst.is_drawable() && sprite.src.is_drawable() =>
                    {
                        let Some(texture) = self.textures.get(&sprite.texture_id) else {
                            continue;
                        };
                        let state = sprite.effective_blend_state();
                        let mut draw = command(
                            texture.id,
                            texture.size,
                            sprite.dst,
                            sprite.src,
                            state.modulation(sprite.color),
                            blend(state.pipeline_index(sprite.render_mode)),
                        );
                        let [top_left, top_right, _, bottom_left] =
                            sprite.quad(false).map(Vec2::from_array);
                        draw.transform = Affine2::from_cols(
                            (top_right - top_left) / sprite.dst.w,
                            (bottom_left - top_left) / sprite.dst.h,
                            top_left,
                        );
                        let src_size = Vec2::new(
                            sprite.src.w * texture.size.width as f32,
                            sprite.src.h * texture.size.height as f32,
                        );
                        let scale = (Vec2::new(sprite.dst.w, sprite.dst.h) / src_size).abs();
                        let position = Vec2::new(sprite.dst.x, sprite.dst.y);
                        let aligned = |v: Vec2| (v - v.round()).abs().max_element() < 0.001;
                        let preserve_texels = !sprite.smooth_upscale
                            && sprite.rotation[2].rem_euclid(512.) == 0.
                            && scale.min_element() >= 0.999
                            && aligned(scale)
                            && aligned(position);
                        draw.shader = Some(ShaderEffect {
                            name: if preserve_texels {
                                SPRITE_NEAREST_SHADER
                            } else {
                                SPRITE_RESAMPLE_SHADER
                            }
                            .into(),
                            uniforms: Default::default(),
                            mask_texture: None,
                            user_texture: None,
                        });
                        list.push(draw);
                    }
                    PalDraw::SolidQuad(quad) if quad.dst.is_drawable() => list.push(command(
                        TextureId(0),
                        TextureInfo {
                            width: 1,
                            height: 1,
                        },
                        quad.dst,
                        RectF::new(0., 0., 1., 1.),
                        quad.color,
                        BlendMode::Alpha,
                    )),
                    _ => {}
                }
            }
        }
        self.gpu.begin_frame(FrameTarget::Main)?;
        let mut clear = scene.clear_color.map(|value| value as f32);
        clear[3] = 1.;
        self.gpu.render_with_clear_color(&list, clear);
        self.gpu.end_frame();
        self.gpu.present(None)
    }
}

fn command(
    texture: TextureId,
    size: TextureInfo,
    dst: RectF,
    uv: RectF,
    color: [f32; 4],
    blend: BlendMode,
) -> DrawCommand {
    DrawCommand {
        texture,
        size,
        transform: Affine2::from_translation(Vec2::new(dst.x, dst.y)),
        opacity: color[3],
        blend,
        color: ColorFilter {
            multiply: [color[0], color[1], color[2]],
            ..Default::default()
        },
        clip: ClipRect {
            uv_offset: [uv.x, uv.y],
            uv_scale: [uv.w, uv.h],
            quad_size: [dst.w, dst.h],
        },
        clip_bounds: None,
        shader: None,
        mesh: None,
        stencil: None,
        native_emote: None,
    }
}

fn blend(mode: usize) -> BlendMode {
    match mode {
        2 => BlendMode::NativeAdd,
        3 | 5 => BlendMode::Product,
        4 => BlendMode::DestinationSquared,
        6 => BlendMode::NativeScreen,
        7 => BlendMode::Minimum,
        8 => BlendMode::Maximum,
        9 => BlendMode::PremultipliedAdd,
        10 => BlendMode::Subtract,
        11 => BlendMode::Opaque,
        _ => BlendMode::Alpha,
    }
}
