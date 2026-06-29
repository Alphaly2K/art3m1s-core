//! Draw-list and backend boundary types.
//!
//! The compositor builds logical scene state; the render pipeline owns the
//! backend-facing draw commands and provider/renderer traits.

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

    /// Samples a texture alpha value at the given pixel coordinate.
    fn pixel_alpha(&self, _texture: TextureId, _x: u32, _y: u32) -> Option<u8> {
        None
    }

    /// Retains only the named resources. Implementations may no-op.
    fn retain(&mut self, _names: &std::collections::HashSet<String>) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlendMode {
    #[default]
    Alpha,
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DrawCommand {
    pub texture: TextureId,
    pub size: TextureInfo,
    pub transform: glam::Affine2,
    pub opacity: f32,
    pub blend: BlendMode,
    pub color: ColorFilter,
    pub clip: ClipRect,
    pub clip_bounds: Option<[f32; 4]>,
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

/// Backend renderer: consumes one frame of draw commands.
pub trait Renderer {
    fn render(&mut self, frame: &DrawList);
}
