//! Render pipeline boundary.
//!
//! The compositor owns scene state, event reduction, animation clocks and
//! DrawList construction.  This module owns the next step: composing that
//! DrawList with transition capture, render-pass declarations and shader asset
//! selection.  Concrete backends compile the selected shaders and execute the
//! passes.

use crate::compositor::build::{build_frame, build_frame_with_content};
use crate::compositor::reduce::Compositor;
pub mod draw;
pub mod hlsl;
pub mod shader;
pub mod transition;

pub use draw::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawList, DrawMesh, LayerDrawSource, Renderer,
    ShaderEffect, ShaderGroup, StencilMetadata, TextureId, TextureInfo, TextureProvider,
};
pub use shader::{BuiltinShaderManager, ShaderManager, ShaderProfile, ShaderProgramSource};

/// Stateless rendering pipeline view over a [`Compositor`].
pub struct RenderPipeline<'a> {
    compositor: &'a Compositor,
}

impl<'a> RenderPipeline<'a> {
    pub fn new(compositor: &'a Compositor) -> Self {
        Self { compositor }
    }

    /// Builds the final draw list and submits it to the backend.
    pub fn render(&self, renderer: &mut dyn Renderer, provider: &mut dyn TextureProvider) {
        let frame = self.build_composited(provider);
        renderer.render(&frame);
    }

    /// Builds the final draw list including transition overlays.
    pub fn build_composited(&self, provider: &mut dyn TextureProvider) -> DrawList {
        self.build_composited_with_text(provider, None)
    }

    /// Builds the final draw list including external text commands and
    /// transition overlays.
    pub fn build_composited_with_text(
        &self,
        provider: &mut dyn TextureProvider,
        text_for: Option<&LayerDrawSource<'_>>,
    ) -> DrawList {
        self.build_composited_with_content(provider, None, text_for)
    }

    /// Builds the final draw list with host-owned layer content and text.
    pub fn build_composited_with_content(
        &self,
        provider: &mut dyn TextureProvider,
        content_for: Option<&LayerDrawSource<'_>>,
        text_for: Option<&LayerDrawSource<'_>>,
    ) -> DrawList {
        let compositor = self.compositor;
        let mut frame = build_frame_with_content(
            &compositor.scene,
            compositor.clock_ms,
            provider,
            content_for,
            text_for,
        );

        transition::overlay_old_frame(&compositor.trans_state, compositor.clock_ms, &mut frame);
        frame
    }

    pub fn needs_trans_capture(&self) -> bool {
        transition::needs_capture(&self.compositor.trans_state)
    }

    pub fn is_transition_in_progress(&self) -> bool {
        transition::is_in_progress(&self.compositor.trans_state, self.compositor.clock_ms)
    }

    pub fn capture_trans_texture(
        &self,
        pixels: &[u8],
        width: u32,
        height: u32,
        provider: &mut dyn TextureProvider,
    ) {
        transition::capture_texture(
            &self.compositor.trans_state,
            self.compositor.clock_ms,
            pixels,
            width,
            height,
            provider,
        );
    }

    pub fn retained_files(&self) -> Vec<String> {
        transition::retained_files(&self.compositor.trans_state)
    }

    /// Builds only the scene DrawList, with no transition overlay.
    pub fn build(&self, provider: &mut dyn TextureProvider) -> DrawList {
        self.build_with_text(provider, None)
    }

    /// Builds only the scene DrawList and allows the host to inject text draw
    /// commands for text layers.
    pub fn build_with_text(
        &self,
        provider: &mut dyn TextureProvider,
        text_for: Option<&LayerDrawSource<'_>>,
    ) -> DrawList {
        let compositor = self.compositor;
        build_frame(&compositor.scene, compositor.clock_ms, provider, text_for)
    }
}
