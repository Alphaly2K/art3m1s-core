//! RFVP command protocol and its conversion into the Art3m1s draw list.
//!
//! This crate deliberately does not depend on `rfvp` or `art3m1s-core`. The
//! protocol types mirror the backend-neutral RFVP frame API so the conversion
//! can be tested before the upstream production renderer is split from wgpu.

pub mod commands;
pub mod protocol;
#[cfg(feature = "rfvp-fork")]
pub mod rfvp_bridge;
#[cfg(feature = "rfvp-fork")]
pub mod runtime;
pub mod textures;

pub use commands::{AdaptedFrame, AdapterError, DrawListAdapter};
pub use protocol::{
    ColorRgba, CommandBlendMode, DrawGlyphCmd, DrawImageCmd, DrawSolidCmd, HitProxy, HitProxyTable,
    PrimId, RectI16, RenderCommand, RenderFrame, Rgba8, TextureHandle, Vertex2D,
};
#[cfg(feature = "rfvp-fork")]
pub use runtime::{ExternalRenderer, ExternalRendererError, RfvpRenderResult};
pub use textures::TextureBindings;
