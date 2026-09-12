//! GPU backend boundary shared by runtimes and concrete graphics APIs.
//!
//! Runtime code talks only to [`GpuBackend`]. Concrete API state such as
//! contexts, command encoders, framebuffers, swapchains and native texture
//! handles belongs to an implementation behind this trait.

use std::collections::HashSet;

pub use crate::draw::TextureOrigin;
pub use crate::external::{
    ExternalImage, ExternalImageKind, ExternalTextureHandle, FrameTargetHandle, GpuSyncKind,
    GpuSyncToken, ResourceOwnership, VideoImportCapability, VideoSurfaceHandle,
};
pub use crate::hlsl::{ShaderCompileError, ShaderId, ShaderSource};
pub use crate::types::{
    BackendCapabilities, BackendInfo, BackendKind, BackendStability, Extent2D, FrameTarget,
    NativeSurface, NativeSurfaceKind, PipelineId, RenderTarget, RenderTargetDesc, RenderTargetId,
    TextureData, TextureDesc, TextureFormat, TextureUpdate, TextureUsage,
};

use crate::draw::{DrawList, TextureId, TextureInfo, TextureProvider};
use crate::post_process::{PostProcessPass, PostProcessPipeline, RenderDimensions, UpscaleMode};

#[cfg(feature = "gl")]
pub mod gl;
#[cfg(all(feature = "metal", any(target_os = "macos", target_os = "ios")))]
pub mod metal;
#[cfg(all(
    feature = "vulkan",
    any(target_os = "android", target_os = "windows", target_os = "linux")
))]
pub mod vulkan;

/// Resource name to encoded image bytes.
pub type AssetSource = dyn Fn(&str) -> Option<Vec<u8>>;

/// Backend-neutral description of the pixels repainted in an internal target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RenderRegion {
    Full,
    Rect([f32; 4]),
}

/// Tightly described YUV420P planes uploaded by the runtime media decoder.
#[derive(Debug, Clone, Copy)]
pub struct Yuv420pPlanes<'a> {
    pub y: &'a [u8],
    pub y_stride: usize,
    pub u: &'a [u8],
    pub u_stride: usize,
    pub v: &'a [u8],
    pub v_stride: usize,
}

impl RenderRegion {
    pub fn from_damage(damage: Option<[f32; 4]>) -> Self {
        damage.map(Self::Rect).unwrap_or(Self::Full)
    }

    pub fn damage(self) -> Option<[f32; 4]> {
        match self {
            Self::Full => None,
            Self::Rect(rect) => Some(rect),
        }
    }

    pub fn union(self, other: Self) -> Self {
        match (self, other) {
            (Self::Full, _) | (_, Self::Full) => Self::Full,
            (Self::Rect(left), Self::Rect(right)) => Self::Rect(union_rect(left, right)),
        }
    }
}

fn union_rect(left: [f32; 4], right: [f32; 4]) -> [f32; 4] {
    let x0 = left[0].min(right[0]);
    let y0 = left[1].min(right[1]);
    let x1 = (left[0] + left[2]).max(right[0] + right[2]);
    let y1 = (left[1] + left[3]).max(right[1] + right[3]);
    [x0, y0, x1 - x0, y1 - y0]
}

/// A transition capture can stay on the GPU or fall back to CPU pixels.
pub enum FrameCapture {
    Texture(TextureId, TextureInfo, TextureOrigin),
    Pixels(Vec<u8>),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GpuProfileStats {
    pub texture_upload_ns: u64,
    pub uploaded_bytes: u64,
    pub video_upload_ns: u64,
    pub video_uploaded_bytes: u64,
    pub video_uploaded_frames: u64,
    pub draw_calls: u64,
    pub vertices: u64,
    pub texture_binds: u64,
    pub dynamic_mesh_uploaded_bytes: u64,
    pub texture_count: u64,
    pub texture_gpu_bytes: u64,
    pub texture_cpu_bytes: u64,
    pub upscale_enabled: bool,
    pub upscale_cpu_encode_ns: u64,
    pub upscale_gpu_ns: u64,
}

/// Complete GPU ownership boundary used by runtimes and engine adapters.
pub trait GpuBackend: TextureProvider {
    fn backend_info(&self) -> BackendInfo;

    /// Save the host graphics state and make this backend ready for work.
    /// Calls may nest; every call must be paired with [`Self::end_access`].
    fn begin_access(&mut self);
    fn end_access(&mut self);

    fn create_texture(
        &mut self,
        name: &str,
        desc: TextureDesc,
        data: TextureData<'_>,
    ) -> Result<TextureId, String>;
    fn update_texture(
        &mut self,
        texture: TextureId,
        update: TextureUpdate<'_>,
    ) -> Result<(), String>;
    fn destroy_texture(&mut self, texture: TextureId);

    fn create_render_target(
        &mut self,
        name: &str,
        desc: RenderTargetDesc,
    ) -> Result<RenderTarget, String>;
    fn destroy_render_target(&mut self, target: RenderTargetId);

    fn resize(&mut self, extent: Extent2D) -> Result<(), String>;
    fn render_dimensions(&self) -> Option<RenderDimensions> {
        None
    }
    fn set_render_scale(&mut self, scale: f32) -> Result<(), String> {
        if (scale - 1.0).abs() <= f32::EPSILON {
            Ok(())
        } else {
            Err("render scale is unsupported by this backend".into())
        }
    }
    fn configure_post_process(&mut self, pipeline: PostProcessPipeline) -> Result<(), String> {
        if !pipeline.render_scale.is_finite() || !(0.1..=1.0).contains(&pipeline.render_scale) {
            return Err("render scale must be finite and in [0.1, 1.0]".into());
        }
        if pipeline.passes.iter().all(|pass| {
            matches!(
                pass,
                PostProcessPass::Upscale(config) if config.mode == UpscaleMode::Linear
            )
        }) {
            Ok(())
        } else {
            Err("post-processing pass is unsupported by this backend".into())
        }
    }
    fn begin_frame(&mut self, target: FrameTarget) -> Result<(), String>;
    fn clear(&mut self, color: [f32; 4]);
    fn end_frame(&mut self);
    fn capture_frame(&mut self, name: &str, extent: Extent2D) -> FrameCapture;
    fn readback(
        &mut self,
        target: FrameTarget,
        extent: Extent2D,
        out: &mut [u8],
    ) -> Result<usize, String>;
    fn readback_owned(&mut self, target: FrameTarget, extent: Extent2D) -> Result<Vec<u8>, String> {
        let mut pixels = vec![0; extent.rgba8_len().ok_or("readback size overflow")?];
        let written = self.readback(target, extent, &mut pixels)?;
        pixels.truncate(written);
        Ok(pixels)
    }

    fn render(&mut self, frame: &DrawList) -> RenderRegion;
    fn render_damage(&mut self, frame: &DrawList, damage: [f32; 4]) -> RenderRegion;
    fn render_damage_visualized(&mut self, frame: &DrawList, damage: [f32; 4]) -> RenderRegion;
    fn render_visualized(&mut self, frame: &DrawList) -> RenderRegion;
    fn clear_damage_overlay(&mut self, frame: &DrawList) -> Option<RenderRegion>;

    fn replace_asset_source(&mut self, source: Box<AssetSource>);
    fn cached_texture_info(&self, name: &str) -> Option<TextureInfo>;
    fn texture_content_revision(&self) -> u64;
    fn changed_texture_ids_since(&self, revision: u64) -> HashSet<TextureId>;
    fn evict_texture_prefix(&mut self, prefix: &str) -> usize;
    fn upload_video_rgba(&mut self, name: &str, width: u32, height: u32, rgba: &[u8]) -> bool;
    fn supports_video_yuv420p(&self) -> bool {
        false
    }
    /// Uploads software-decoded YUV420P planes and converts them on the GPU.
    ///
    /// The portable fallback is to convert a frame to RGBA and call
    /// [`Self::upload_video_rgba`]. Backends that can sample or convert YUV
    /// natively should override this method to avoid a CPU full-frame copy.
    fn upload_video_yuv420p(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        planes: Yuv420pPlanes<'_>,
    ) -> bool {
        let _ = (name, width, height, planes);
        false
    }

    fn video_import_capability(&self) -> VideoImportCapability {
        VideoImportCapability::cpu_only()
    }
    fn import_external_texture(
        &mut self,
        name: &str,
        image: ExternalImage<'_>,
    ) -> Result<ExternalTextureHandle, String> {
        let _ = (name, image);
        Err("external texture import is unsupported by this backend".into())
    }
    fn release_external_texture(&mut self, handle: ExternalTextureHandle) -> bool {
        let _ = handle;
        false
    }
    fn acquire_video_surface(
        &mut self,
        name: &str,
        extent: Extent2D,
    ) -> Result<VideoSurfaceHandle, String> {
        let _ = (name, extent);
        Err("video surfaces are unsupported by this backend".into())
    }
    fn commit_video_surface(&mut self, handle: VideoSurfaceHandle) -> bool {
        let _ = handle;
        false
    }
    fn video_surface_consumed(&mut self, handle: VideoSurfaceHandle) -> bool {
        let _ = handle;
        true
    }
    fn video_surface_gl_framebuffer(&self, handle: VideoSurfaceHandle) -> Option<u32> {
        let _ = handle;
        None
    }
    fn capture_screenshot(&mut self, extent: Extent2D, out: &mut [u8]) -> Result<usize, String> {
        self.readback(FrameTarget::Main, extent, out)
    }

    fn set_native_surface(&mut self, surface: NativeSurface) -> Result<(), String>;
    fn clear_native_surface(&mut self);
    fn present(&mut self, damage: Option<[f32; 4]>) -> Result<(), String>;

    fn register_hlsl_shader(
        &mut self,
        name: &str,
        source: &[u8],
    ) -> Result<ShaderId, ShaderCompileError>;
    fn replace_hlsl_shader(
        &mut self,
        name: &str,
        source: &[u8],
    ) -> Result<ShaderId, ShaderCompileError> {
        self.register_hlsl_shader(name, source)
    }
    fn reload_hlsl_shader(
        &mut self,
        name: &str,
        source: &[u8],
    ) -> Result<ShaderId, ShaderCompileError> {
        self.replace_hlsl_shader(name, source)
    }
    fn unregister_shader(&mut self, name: &str) -> bool;
    fn collect_retired_resources(&mut self) {}
    fn set_profile_enabled(&self, enabled: bool);
    fn take_profile_stats(&mut self) -> GpuProfileStats;

    fn external_renderer_proc_address(&self, _name: &str) -> *const std::ffi::c_void {
        std::ptr::null()
    }
    fn begin_external_render(&mut self) -> Result<(), String> {
        Err("external GPU rendering is unsupported by this backend".into())
    }
    fn external_render_target(
        &mut self,
        _name: &str,
        _width: u32,
        _height: u32,
    ) -> Result<u64, String> {
        Err("external GPU render targets are unsupported by this backend".into())
    }
    fn commit_external_render_target(&mut self, _name: &str) -> bool {
        false
    }
    fn end_external_render(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_regions_union_without_losing_full_damage() {
        let full = RenderRegion::Full;
        let rect = RenderRegion::Rect([1.0, 2.0, 3.0, 4.0]);
        assert_eq!(full.union(rect), RenderRegion::Full);
        assert_eq!(rect.union(full), RenderRegion::Full);
        assert_eq!(
            rect.union(RenderRegion::Rect([4.0, 6.0, 2.0, 2.0])),
            RenderRegion::Rect([1.0, 2.0, 5.0, 6.0])
        );
    }

    #[test]
    fn render_region_converts_to_legacy_damage() {
        assert_eq!(RenderRegion::from_damage(None), RenderRegion::Full);
        assert_eq!(RenderRegion::Full.damage(), None);
        assert_eq!(
            RenderRegion::from_damage(Some([1.0, 2.0, 3.0, 4.0])).damage(),
            Some([1.0, 2.0, 3.0, 4.0])
        );
    }
}
