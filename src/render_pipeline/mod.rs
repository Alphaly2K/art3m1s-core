//! Render pipeline boundary.
//!
//! The compositor owns scene state, event reduction, animation clocks and
//! DrawList construction.  This module owns the next step: composing that
//! DrawList with transition capture, render-pass declarations and shader asset
//! selection.  Concrete backends compile the selected shaders and execute the
//! passes.

use crate::compositor::build::build_frame_with_content;
use crate::compositor::reduce::Compositor;
use crate::compositor::scene::Scene;
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
        text_for: Option<&mut LayerDrawSource<'_>>,
    ) -> DrawList {
        self.build_composited_with_content(provider, None, text_for)
    }

    /// Builds the final draw list with host-owned layer content and text.
    pub fn build_composited_with_content(
        &self,
        provider: &mut dyn TextureProvider,
        content_for: Option<&mut LayerDrawSource<'_>>,
        text_for: Option<&mut LayerDrawSource<'_>>,
    ) -> DrawList {
        let compositor = self.compositor;
        // [lyedit] 像素加工在进入帧构建前落地（需要 provider 才能读写像素）。
        compositor.process_layer_edits(provider);
        let overrides = compositor.layer_edit_overrides();
        let mut frame = build_frame_with_content(
            &compositor.scene,
            compositor.clock_ms,
            provider,
            content_for,
            text_for,
            if overrides.is_empty() {
                None
            } else {
                Some(&overrides)
            },
        );

        transition::overlay_old_frame(
            &compositor.trans_state,
            compositor.clock_ms,
            &mut frame,
            provider,
        );
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

    pub fn capture_trans_gpu_texture(&self, texture: TextureId, info: TextureInfo) {
        transition::capture_gpu_texture(
            &self.compositor.trans_state,
            self.compositor.clock_ms,
            texture,
            info,
        );
    }

    pub fn retained_files(&self) -> Vec<String> {
        let mut files = transition::retained_files(&self.compositor.trans_state);
        // [lyedit] 生成的加工后纹理不在场景 file 列表里，显式保活。
        files.extend(self.compositor.layer_edit_overrides().into_values());
        files
    }

    /// 用户输入请求跳过进行中的转场（`[trans]` input 参数语义）。
    ///
    /// `in_skip_mode`：引擎当前是否处于跳过（skip）状态，input=2 时用。
    /// 返回是否真的跳过了转场。宿主在转场等待期间收到点击时调用。
    pub fn skip_transition_by_input(&self, in_skip_mode: bool) -> bool {
        transition::skip_by_input(&self.compositor.trans_state, in_skip_mode)
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
        text_for: Option<&mut LayerDrawSource<'_>>,
    ) -> DrawList {
        self.build_with_content(provider, None, text_for)
    }

    /// Builds only the scene DrawList with host-owned layer content and text,
    /// without a transition overlay.
    pub fn build_with_content(
        &self,
        provider: &mut dyn TextureProvider,
        content_for: Option<&mut LayerDrawSource<'_>>,
        text_for: Option<&mut LayerDrawSource<'_>>,
    ) -> DrawList {
        let compositor = self.compositor;
        compositor.process_layer_edits(provider);
        let overrides = compositor.layer_edit_overrides();
        build_frame_with_content(
            &compositor.scene,
            compositor.clock_ms,
            provider,
            content_for,
            text_for,
            if overrides.is_empty() {
                None
            } else {
                Some(&overrides)
            },
        )
    }

    /// Builds a previously rendered scene without transition overlays.
    ///
    /// The runtime uses this to reconstruct a transition source frame from the
    /// previous image-layer state while supplying text filtered by the current
    /// message-layer visibility.
    pub fn build_scene_with_content(
        &self,
        scene: &Scene,
        clock_ms: u64,
        provider: &mut dyn TextureProvider,
        content_for: Option<&mut LayerDrawSource<'_>>,
        text_for: Option<&mut LayerDrawSource<'_>>,
    ) -> DrawList {
        self.compositor.process_layer_edits(provider);
        let overrides = self.compositor.layer_edit_overrides();
        build_frame_with_content(
            scene,
            clock_ms,
            provider,
            content_for,
            text_for,
            if overrides.is_empty() {
                None
            } else {
                Some(&overrides)
            },
        )
    }
}
