//! Compatibility re-export.
//!
//! Draw-list and renderer/provider traits now live in
//! [`crate::render_pipeline::draw`].  New code should import them from
//! `render_pipeline`; this module remains to keep older tests and callers
//! compiling during the migration.

pub use crate::render_pipeline::draw::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawList, Renderer, TextureId, TextureInfo,
    TextureProvider,
};
