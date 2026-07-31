//! Draw-list and backend boundary types.
//!
//! The compositor builds logical scene state; the render pipeline owns the
//! backend-facing draw commands and provider/renderer traits.

use std::collections::BTreeMap;
use std::fmt::Debug;

/// Opaque backend texture handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureId(pub u64);

/// Texture pixel size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextureInfo {
    pub width: u32,
    pub height: u32,
}

/// Resolves logical resource names to backend textures.
pub trait TextureProvider {
    fn resolve(&mut self, name: &str) -> Option<(TextureId, TextureInfo)>;

    /// Uploads raw RGBA pixels and returns a backend texture handle.
    fn upload_rgba(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Option<(TextureId, TextureInfo)>;

    /// Uploads pixels that will only be sampled by the renderer.
    ///
    /// Backends may avoid retaining a CPU-readable copy. The default keeps
    /// compatibility with providers that only implement [`Self::upload_rgba`].
    fn upload_rgba_render_only(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        self.upload_rgba(name, width, height, data)
    }

    /// Samples a texture alpha value at the given pixel coordinate.
    fn pixel_alpha(&self, _texture: TextureId, _x: u32, _y: u32) -> Option<u8> {
        None
    }

    /// Retains only the named resources. Implementations may no-op.
    fn retain(&mut self, _names: &std::collections::HashSet<String>) {}

    /// 取一张 1x1 纯色纹理（`lyc` 缺省 file 的单色图层模式用）。
    ///
    /// 默认实现直接按稳定名字上传；带缓存的后端（如 GL provider）应覆写为
    /// 先查缓存，避免每帧重建纹理。
    fn solid_texture(&mut self, rgba: [u8; 4]) -> Option<(TextureId, TextureInfo)> {
        self.upload_rgba(&solid_texture_name(rgba), 1, 1, &rgba)
    }

    /// 解析 `file` 并用 `mask` 灰度图合成 alpha 后的组合纹理（`lyc` mask 参数）。
    ///
    /// 默认实现忽略蒙版、退化为普通 `resolve`；有像素访问能力的后端应覆写为
    /// 真正的双图合成（out.rgb = file.rgb，out.a = file.a × mask 灰度）。
    fn resolve_with_mask(&mut self, file: &str, _mask: &str) -> Option<(TextureId, TextureInfo)> {
        self.resolve(file)
    }

    /// 读取某逻辑资源的 CPU 侧 RGBA 像素（`lyedit` 像素加工用）。
    ///
    /// 返回 `(宽, 高, RGBA8)`。无法提供像素（无 CPU 缓存）时返回 `None`。
    fn pixels_of(&mut self, _name: &str) -> Option<(u32, u32, Vec<u8>)> {
        None
    }
}

/// `lyc` 单色图层使用的 1x1 纯色纹理的稳定缓存名。
///
/// provider 缓存与 `Scene::collect_files` 的保活列表都用这个名字，保证纹理
/// 不会被逐帧驱逐重建。
pub fn solid_texture_name(rgba: [u8; 4]) -> String {
    format!(
        "__solid_{:02x}{:02x}{:02x}{:02x}__",
        rgba[0], rgba[1], rgba[2], rgba[3]
    )
}

/// `lyc` file+mask 双图合成纹理的稳定缓存名。
///
/// provider 缓存与 `Scene::collect_files` 的保活列表共用，避免 retain 驱逐。
pub fn masked_texture_name(file: &str, mask: &str) -> String {
    format!("{file}\u{1f}mask\u{1f}{mask}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlendMode {
    #[default]
    Alpha,
    /// Source pixels already have RGB multiplied by alpha (for example an
    /// offscreen group texture).
    PremultipliedAlpha,
    /// Premultiplied-alpha source with additive composition.
    PremultipliedAdd,
    Add,
    Screen,
    Multiply,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorFilter {
    pub multiply: [f32; 3],
    pub grayscale: bool,
    pub negative: bool,
}

impl Default for ColorFilter {
    fn default() -> Self {
        Self {
            multiply: [1.0, 1.0, 1.0],
            grayscale: false,
            negative: false,
        }
    }
}

impl ColorFilter {
    pub fn is_identity(&self) -> bool {
        self.multiply == [1.0, 1.0, 1.0] && !self.grayscale && !self.negative
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShaderEffect {
    pub name: String,
    pub uniforms: BTreeMap<String, Vec<f32>>,
    pub mask_texture: Option<TextureId>,
    pub user_texture: Option<TextureId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShaderGroup {
    pub start: usize,
    pub end: usize,
    pub effect: ShaderEffect,
    pub clip_bounds: Option<[f32; 4]>,
    pub mask_range: Option<[usize; 2]>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StencilMetadata {
    pub namespace: u64,
    pub source_label: String,
    pub mask_labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DrawCommand {
    pub texture: TextureId,
    pub size: TextureInfo,
    pub transform: glam::Affine2,
    pub opacity: f32,
    pub blend: BlendMode,
    pub color: ColorFilter,
    pub clip: ClipRect,
    pub clip_bounds: Option<[f32; 4]>,
    pub shader: Option<ShaderEffect>,
    pub mesh: Option<DrawMesh>,
    pub stencil: Option<StencilMetadata>,
}

/// Host-owned draw commands attached to a compositor layer.
pub type LayerDrawSource<'a> = dyn Fn(&str) -> Vec<DrawCommand> + 'a;

/// Expanded triangle-list geometry for deformed sprites.
///
/// Positions are local pixels and UVs are normalized within `DrawCommand::clip`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DrawMesh {
    pub vertices: Vec<[f32; 4]>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipRect {
    pub uv_offset: [f32; 2],
    pub uv_scale: [f32; 2],
    pub quad_size: [f32; 2],
}

impl ClipRect {
    pub fn full(size: TextureInfo) -> Self {
        Self {
            uv_offset: [0.0, 0.0],
            uv_scale: [1.0, 1.0],
            quad_size: [size.width as f32, size.height as f32],
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DrawList {
    pub commands: Vec<DrawCommand>,
    pub mask_commands: Vec<DrawCommand>,
    pub shader_groups: Vec<ShaderGroup>,
}

impl DrawList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, command: DrawCommand) {
        self.commands.push(command);
    }

    pub fn push_shader_group(&mut self, group: ShaderGroup) {
        self.shader_groups.push(group);
    }

    pub fn materialize_stencil_groups(&mut self, effect_name: &str) {
        let masked = self
            .commands
            .iter()
            .enumerate()
            .filter_map(|(index, command)| {
                command
                    .stencil
                    .as_ref()
                    .filter(|stencil| !stencil.mask_labels.is_empty())
                    .map(|stencil| (index, stencil.clone()))
            })
            .collect::<Vec<_>>();

        for (index, stencil) in masked {
            let content_center = command_center(&self.commands[index]);
            let mask_start = self.mask_commands.len();
            for label in &stencil.mask_labels {
                let closest = self
                    .commands
                    .iter()
                    .filter(|candidate| {
                        candidate.stencil.as_ref().is_some_and(|metadata| {
                            metadata.namespace == stencil.namespace
                                && metadata.source_label == *label
                        })
                    })
                    .min_by(|left, right| {
                        center_distance_squared(left, content_center)
                            .total_cmp(&center_distance_squared(right, content_center))
                    });
                if let Some(mask) = closest {
                    let mut mask = mask.clone();
                    mask.blend = BlendMode::Alpha;
                    mask.color = ColorFilter::default();
                    mask.shader = None;
                    mask.stencil = None;
                    self.mask_commands.push(mask);
                }
            }
            let mask_end = self.mask_commands.len();
            if mask_end == mask_start {
                continue;
            }
            self.shader_groups.push(ShaderGroup {
                start: index,
                end: index + 1,
                effect: ShaderEffect {
                    name: effect_name.to_owned(),
                    uniforms: BTreeMap::new(),
                    mask_texture: None,
                    user_texture: None,
                },
                clip_bounds: self.commands[index].clip_bounds,
                mask_range: Some([mask_start, mask_end]),
            });
        }
    }

    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

fn command_center(command: &DrawCommand) -> glam::Vec2 {
    command.transform.transform_point2(glam::Vec2::new(
        command.clip.quad_size[0] * 0.5,
        command.clip.quad_size[1] * 0.5,
    ))
}

fn center_distance_squared(command: &DrawCommand, point: glam::Vec2) -> f32 {
    command_center(command).distance_squared(point)
}

/// Backend renderer: consumes one frame of draw commands.
pub trait Renderer {
    fn render(&mut self, frame: &DrawList);
}
