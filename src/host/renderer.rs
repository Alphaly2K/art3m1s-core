//! Rust adapter for rendering a Core frame lease.

use super::frame::{FrameLease, FramePacket, FrameTextureDescriptor};
use super::runtime::RuntimeHandle;
use art3m1s_render::artemis::{FramePlan, SpriteRenderer as HostRenderer};
use art3m1s_render::backend::{GpuBackend, RenderRegion};

/// Draws Core frames with a backend owned and configured by the Host.
pub struct SpriteRenderer<B>(HostRenderer<B>);

impl<B: GpuBackend> SpriteRenderer<B> {
    pub fn new(backend: B) -> Self {
        Self(HostRenderer::new(backend))
    }

    /// Configure the target, context and viewport. Clear cached resources before evicting them.
    pub fn backend_mut(&mut self) -> &mut B {
        self.0.backend_mut()
    }

    /// Upload and draw in the frame the Host has already begun.
    pub fn render(&mut self, frame: &FrameLease) -> Result<RenderRegion, String> {
        self.render_with_clear_color(frame, [0.0, 0.0, 0.0, 1.0])
    }

    /// Use the same clear color for both scenes of a transition.
    pub fn render_with_clear_color(
        &mut self,
        frame: &FrameLease,
        clear_color: [f32; 4],
    ) -> Result<RenderRegion, String> {
        self.0.render_plan(
            FramePlan {
                runtime: frame.runtime().as_raw(),
                epoch: frame.session_epoch,
                stage_size: frame.stage_size,
                sprites: &frame.sprites,
                groups: &frame.groups,
                masks: &frame.masks,
                transition: frame.transition.as_ref(),
            },
            |index| {
                frame.texture(index).map(|image| FrameTextureDescriptor {
                    revision: image.revision,
                    width: image.width,
                    height: image.height,
                })
            },
            |index| {
                frame
                    .texture(index)
                    .map(|image| image.rgba())
                    .ok_or_else(|| "missing frame texture".into())
            },
            clear_color,
        )
    }

    /// Draw an ABI packet in the Host's active target. Pixel borrows must be
    /// from this packet's frame token, which must remain live for this call.
    pub fn render_packet<'a>(
        &mut self,
        runtime: RuntimeHandle,
        packet: &FramePacket,
        pixels: impl FnMut(usize) -> Result<&'a [u8], String>,
        clear_color: [f32; 4],
    ) -> Result<RenderRegion, String> {
        self.0
            .render_packet(runtime.as_raw(), packet, pixels, clear_color)
    }

    pub fn clear_cached_textures(&mut self) {
        self.0.clear_cached_textures();
    }

    pub fn clear_cached_shaders(&mut self) {
        self.0.clear_cached_shaders();
    }
}
