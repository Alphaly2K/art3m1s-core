//! Artemis frame data and optional drawing for Host-owned render targets.

mod frame;
mod packet;
#[cfg(feature = "artemis")]
mod renderer;
#[doc(hidden)]
pub mod wire;

pub use frame::{
    FrameGroup, FrameShader, FrameShaderEffect, FrameShaderLanguage, FrameSprite, FrameTransition,
    SpriteBlend,
};
pub use packet::{FramePacket, FramePacketError, FrameTextureDescriptor};
#[cfg(feature = "artemis")]
pub use renderer::{FramePlan, SpriteRenderer};
pub use wire::FRAME_PACKET_VERSION;
