//! Compatibility re-export for the old pipeline module name.
//!
//! New code should use [`crate::render_pipeline::RenderPipeline`].  This module
//! remains so existing callers do not need to migrate in the same patch.

pub use crate::render_pipeline::{
    BuiltinShaderManager, CompositorPipeline, PostprocessPass, RenderPass, RenderPipeline,
    ShaderManager, ShaderProfile, ShaderProgramSource,
};
