use crate::draw::NativeEmoteMaterial;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpriteBlend {
    Alpha,
    Add,
    Screen,
    Multiply,
    EmoteAdd,
    EmoteReverseSubtract,
    EmoteMultiply,
    EmoteScreen,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrameSprite {
    pub layer_id: String,
    pub texture: usize,
    /// If present, set each source texel's alpha to `alpha * mask_red / 255` before sampling.
    pub mask: Option<usize>,
    /// Two affine basis vectors (columns), followed by the translation.
    pub transform: [f32; 6],
    pub quad_size: [f32; 2],
    pub uv_offset: [f32; 2],
    pub uv_scale: [f32; 2],
    /// Stage coordinates (left, top, width, height).
    pub clip_bounds: Option<[f32; 4]>,
    pub opacity: f32,
    pub color_multiply: [f32; 3],
    pub grayscale: bool,
    pub negative: bool,
    pub blend: SpriteBlend,
    pub mesh: Option<Arc<[[f32; 4]]>>,
    pub emote_material: Option<NativeEmoteMaterial>,
    pub shader: Option<FrameShaderEffect>,
}

#[derive(Debug, PartialEq)]
pub struct FrameTransition {
    /// Draw this scene into the transition source target before blending it over `sprites`.
    pub source: Box<[FrameSprite]>,
    pub source_groups: Box<[FrameGroup]>,
    pub source_masks: Box<[FrameSprite]>,
    pub progress: f32,
    pub rule: Option<usize>,
    pub vague: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrameGroup {
    pub sprites: std::ops::Range<usize>,
    pub mask: Option<usize>,
    pub clip_bounds: Option<[f32; 4]>,
    pub opacity: f32,
    pub color_multiply: [f32; 3],
    pub grayscale: bool,
    pub negative: bool,
    pub opaque: bool,
    pub blend: SpriteBlend,
    pub stencil: Option<std::ops::Range<usize>>,
    pub shader: Option<FrameShaderEffect>,
}

#[derive(Debug, PartialEq)]
pub struct FrameShader {
    pub revision: u64,
    pub source: Box<[u8]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrameShaderEffect {
    pub shader: Arc<FrameShader>,
    pub uniforms: std::collections::BTreeMap<String, Vec<f32>>,
    pub mask: Option<usize>,
    pub user: Option<usize>,
}
