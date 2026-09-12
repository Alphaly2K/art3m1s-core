//! Backend-neutral command types emitted by RFVP.
//!
//! These are an owned mirror of `rfvp::host_api::render`. Keeping the adapter
//! crate independent from the full RFVP dependency tree lets the host compile
//! and test its rendering boundary before the engine-side feature split lands.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureHandle(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PrimId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorRgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RectI16 {
    pub x: i16,
    pub y: i16,
    pub w: i16,
    pub h: i16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RectU16 {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgba8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba8 {
    pub fn to_color_rgba(self) -> ColorRgba {
        ColorRgba {
            r: self.r as f32 / 255.0,
            g: self.g as f32 / 255.0,
            b: self.b as f32 / 255.0,
            a: self.a as f32 / 255.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vertex2D {
    pub position: [f32; 2],
    pub tex_coord: [f32; 2],
    pub color: ColorRgba,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandBlendMode {
    Normal,
    Add,
    Sub,
    Mul,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DrawImageCmd {
    pub texture: TextureHandle,
    pub src: RectU16,
    pub dst: RectI16,
    pub color: Rgba8,
    pub blend: CommandBlendMode,
    pub effect_id: u16,
    pub clip: Option<RectI16>,
    pub vertices: [Vertex2D; 4],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DrawGlyphCmd {
    pub texture: TextureHandle,
    pub src: RectU16,
    pub dst: RectI16,
    pub color: Rgba8,
    pub clip: Option<RectI16>,
}

/// Future production-path solid command.
///
/// RFVP 0.6.0's `RenderFrame` does not emit this yet, but the external-renderer
/// ABI and dissolve/UI paths both need it. Supporting the conversion now keeps
/// that engine-side change mechanical.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DrawSolidCmd {
    pub rect: RectI16,
    pub color: ColorRgba,
    pub blend: CommandBlendMode,
    pub clip: Option<RectI16>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RenderCommand {
    DrawImage(DrawImageCmd),
    DrawGlyph(DrawGlyphCmd),
    DrawSolid(DrawSolidCmd),
    SetClip(RectI16),
    ClearClip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HitProxy {
    pub prim_id: PrimId,
    pub rect: RectI16,
    pub enabled: bool,
    pub visible: bool,
    pub order: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HitProxyTable {
    pub proxies: Vec<HitProxy>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RenderFrame {
    pub commands: Vec<RenderCommand>,
    pub hit_proxies: HitProxyTable,
}
