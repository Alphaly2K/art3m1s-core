//! GPU backend boundary.
//!
//! Runtime and render-pipeline code talk only to [`GpuBackend`]. Concrete API
//! state (contexts, command encoders, framebuffers, swapchains and native
//! texture handles) belongs to an implementation below this module.

use crate::render_pipeline::draw::{DrawList, TextureId, TextureInfo, TextureProvider};
use std::collections::HashSet;

#[cfg(feature = "gl-backend")]
pub mod gl;

/// Resource name to encoded image bytes.
pub type AssetSource = dyn Fn(&str) -> Option<Vec<u8>>;

/// Backend-neutral description of the pixels repainted in an internal target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RenderRegion {
    Full,
    Rect([f32; 4]),
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
    Texture(TextureId, TextureInfo),
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
}

/// Host-owned destination used for zero-copy presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputSurfaceKind {
    AndroidNativeWindow,
    AppleIoSurface,
    AppleMetalTexture,
}

impl OutputSurfaceKind {
    pub fn from_legacy_int(value: i32) -> Result<Self, String> {
        match value {
            1 => Ok(Self::AndroidNativeWindow),
            2 => Ok(Self::AppleIoSurface),
            3 => Ok(Self::AppleMetalTexture),
            _ => Err(format!("unsupported external surface kind: {value}")),
        }
    }

    #[cfg(feature = "gl-backend")]
    pub(crate) fn legacy_int(self) -> i32 {
        match self {
            Self::AndroidNativeWindow => 1,
            Self::AppleIoSurface => 2,
            Self::AppleMetalTexture => 3,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct OutputSurface {
    pub kind: OutputSurfaceKind,
    pub handle: *mut std::ffi::c_void,
    pub width: u32,
    pub height: u32,
}

impl OutputSurface {
    pub fn from_legacy_parts(
        kind: i32,
        handle: *mut std::ffi::c_void,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        if handle.is_null() || width == 0 || height == 0 {
            return Err("invalid external surface".into());
        }
        Ok(Self {
            kind: OutputSurfaceKind::from_legacy_int(kind)?,
            handle,
            width,
            height,
        })
    }
}

/// Complete GPU ownership boundary used by [`crate::runtime::CoreRuntime`].
///
/// The trait deliberately covers only responsibilities that currently cross
/// the runtime/backend boundary. It keeps [`TextureId`] and [`DrawList`]
/// stable while allowing a future Metal or Vulkan implementation to own its
/// native device, render targets and presentation objects.
pub trait GpuBackend: TextureProvider {
    /// Save the host graphics state and make this backend ready for work.
    /// Calls may nest; every call must be paired with [`Self::end_access`].
    fn begin_access(&mut self);
    fn end_access(&mut self);

    fn resize(&mut self, width: u32, height: u32) -> Result<(), String>;
    fn begin_frame(&mut self);
    fn end_frame(&mut self);
    fn capture_frame(&mut self, name: &str, width: u32, height: u32) -> FrameCapture;
    fn read_frame_into(&mut self, width: u32, height: u32, out: &mut [u8]) -> usize;
    fn read_frame(&mut self, width: u32, height: u32) -> Vec<u8>;

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

    fn set_output_surface(&mut self, surface: OutputSurface) -> Result<(), String>;
    fn clear_output_surface(&mut self);
    fn present(&mut self, damage: Option<[f32; 4]>) -> Result<(), String>;

    fn register_hlsl_shader(&mut self, name: &str, source: &[u8]) -> Result<(), String>;
    fn set_profile_enabled(&self, enabled: bool);
    fn take_profile_stats(&self) -> GpuProfileStats;

    /// Optional host renderer interop. The reference backend exposes the GL
    /// entry points and framebuffer handles required by the existing libmpv ABI.
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

/// Creation choice kept outside `CoreRuntime`. This stage contains only the
/// existing reference GL backend; native Metal/Vulkan variants are intentionally
/// not introduced yet.
#[cfg(feature = "gl-backend")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendSelection {
    ReferenceGl(gl::platform::GfxBackend),
}

#[cfg(feature = "gl-backend")]
impl BackendSelection {
    /// Preserves the existing C ABI values used by the Flutter host.
    pub fn from_legacy_int(value: i32) -> Self {
        Self::ReferenceGl(gl::platform::GfxBackend::from_int(value))
    }
}

#[cfg(feature = "gl-backend")]
impl From<gl::platform::GfxBackend> for BackendSelection {
    fn from(value: gl::platform::GfxBackend) -> Self {
        Self::ReferenceGl(value)
    }
}

#[cfg(feature = "gl-backend")]
pub(crate) fn create_backend(
    selection: BackendSelection,
    width: u32,
    height: u32,
) -> Result<Box<dyn GpuBackend>, String> {
    match selection {
        BackendSelection::ReferenceGl(config) => {
            Ok(Box::new(gl::GlBackend::new(config, width, height)?))
        }
    }
}
