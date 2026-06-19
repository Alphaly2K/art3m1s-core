//! 合成器与 GPU 后端之间的唯一边界。
//!
//! 合成器核心不依赖任何图形 API：它只产出一个有序的 [`DrawList`]，再交给实现
//! [`Renderer`] 的后端执行。将来的 ANGLE 后端只需实现 [`Renderer`] 与
//! [`TextureProvider`] 两个 trait 即可接入；测试则用 `mock` 模块里的假后端。

use std::fmt::Debug;

/// 后端纹理的不透明句柄。
///
/// 合成器不关心句柄背后是什么（GL 纹理、占位 ID …），只负责在 draw 命令里透传。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureId(pub u64);

/// 纹理的像素尺寸，用于计算图层的世界变换（锚点、缩放都基于原始尺寸）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextureInfo {
    pub width: u32,
    pub height: u32,
}

/// 把逻辑资源名（如 `"black"`、`"bg/room"`）解析为后端纹理。
///
/// 解析与解码（PNG/TLG…）、上传 GPU 都由后端负责；合成器只通过这个 trait 索要
/// 句柄及其尺寸。返回 `None` 表示资源缺失，合成器会跳过该图层而非崩溃。
pub trait TextureProvider {
    fn resolve(&mut self, name: &str) -> Option<(TextureId, TextureInfo)>;

    /// 上传原始 RGBA 像素数据并返回纹理句柄。
    /// 用于字形 atlas 等动态纹理。
    fn upload_rgba(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Option<(TextureId, TextureInfo)>;

    /// 采样纹理在 (x, y) 处的 alpha 通道值（0-255）。
    ///
    /// 用于 [`Compositor::hit_test`] 实现 `clickablethreshold`：当坐标处像素 alpha
    /// 低于阈值时，指针穿透该图层。返回 `None` 表示无法采样（纹理不存在或坐标越界），
    /// 调用方应视为"不透明"（保守放行点击）。
    fn pixel_alpha(&self, _texture: TextureId, _x: u32, _y: u32) -> Option<u8> {
        None
    }
}

/// 混合模式。Artemis 的 `layermode` 字符串在归约阶段映射到这里。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlendMode {
    /// 常规 alpha 混合。
    #[default]
    Alpha,
    /// 加算。
    Add,
    /// 屏幕。
    Screen,
    /// 乘算。
    Multiply,
}

/// 逐图层的颜色滤镜，对应 `colormultiply` / `grayscale` / `negative`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorFilter {
    /// 颜色乘算（归一化 RGB），默认白色即不改色。
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
    /// 是否为恒等滤镜（不改变像素），后端可借此走快路径。
    pub fn is_identity(&self) -> bool {
        self.multiply == [1.0, 1.0, 1.0] && !self.grayscale && !self.negative
    }
}

/// 单条绘制命令：把一张纹理用给定的 2D 仿射变换画到舞台上。
///
/// `transform` 是把"纹理局部坐标（原点在左上、单位为像素）"映射到"舞台坐标"
/// 的仿射矩阵，已包含父图层链累积的平移/缩放/旋转/锚点。`opacity` 是从根到本
/// 图层累乘后的最终不透明度。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DrawCommand {
    pub texture: TextureId,
    /// 纹理原始尺寸，配合 `transform` 算出四个角的舞台坐标。
    pub size: TextureInfo,
    pub transform: glam::Affine2,
    pub opacity: f32,
    pub blend: BlendMode,
    pub color: ColorFilter,
    /// 精灵裁剪：要采样的纹理子区域。
    ///
    /// `uv_offset` / `uv_scale` 是归一化 0..1 的 UV 起点与跨度；`quad_size` 是该子
    /// 区域在像素下的绘制尺寸（顶点用它展开，而不是整张纹理尺寸）。无裁剪时
    /// 为整张纹理：offset=(0,0)、scale=(1,1)、quad_size=纹理原始尺寸。
    pub clip: ClipRect,
}

/// 绘制时的纹理裁剪矩形，UV 归一化、尺寸为像素。详见 [`DrawCommand::clip`]。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipRect {
    pub uv_offset: [f32; 2],
    pub uv_scale: [f32; 2],
    pub quad_size: [f32; 2],
}

impl ClipRect {
    /// 整张纹理（不裁剪）。
    pub fn full(size: TextureInfo) -> Self {
        Self {
            uv_offset: [0.0, 0.0],
            uv_scale: [1.0, 1.0],
            quad_size: [size.width as f32, size.height as f32],
        }
    }
}

/// 一帧的有序绘制列表，按从底到顶的绘制顺序排列（先画的在底层）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DrawList {
    pub commands: Vec<DrawCommand>,
}

impl DrawList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, command: DrawCommand) {
        self.commands.push(command);
    }

    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

/// GPU 后端：消费一帧的 [`DrawList`] 并把它画出来。
///
/// 合成器每帧调用一次 [`Renderer::render`]。后端负责清屏、按命令顺序绘制、呈现。
pub trait Renderer {
    fn render(&mut self, frame: &DrawList);
}
