//! RFVP integration boundary for Art3m1s.
//!
//! The base crate provides backend-neutral RFVP command types and conversion
//! into the Art3m1s draw list. Optional features add the dependency on the
//! RFVP fork and the host runtime that drives it. The crate never depends on
//! `art3m1s-core`.

pub mod commands;
#[cfg(feature = "host-runtime")]
pub mod host_runtime;
pub mod protocol;
#[cfg(feature = "rfvp-fork")]
pub mod rfvp_bridge;
#[cfg(feature = "rfvp-fork")]
pub mod runtime;
pub mod textures;

pub use commands::{AdaptedFrame, AdapterError, DrawListAdapter};
#[cfg(feature = "host-runtime")]
pub use host_runtime::{
    RfvpAudioSampleFormat, RfvpEncodedAudioKind, RfvpHostAudioCommand, RfvpHostAudioCommandKind,
    RfvpHostInputEvent, RfvpHostRuntime, RfvpHostRuntimeError, RfvpNls, RfvpPointerButton,
    RfvpTouchPhase,
};
pub use protocol::{
    ColorRgba, CommandBlendMode, DrawGlyphCmd, DrawImageCmd, DrawSolidCmd, HitProxy, HitProxyTable,
    PrimId, RectI16, RenderCommand, RenderFrame, Rgba8, TextureHandle, Vertex2D,
};
#[cfg(feature = "rfvp-fork")]
pub use runtime::{ExternalRenderer, ExternalRendererError, RfvpRenderResult};
pub use textures::TextureBindings;
