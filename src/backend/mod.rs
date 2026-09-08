//! GPU backend boundary.
//!
//! Runtime and render-pipeline code talk only to [`GpuBackend`]. Concrete API
//! state (contexts, command encoders, framebuffers, swapchains and native
//! texture handles) belongs to an implementation below this module.

pub use crate::render_pipeline::draw::TextureOrigin;
use crate::render_pipeline::draw::{DrawList, TextureId, TextureInfo, TextureProvider};
use std::collections::HashSet;

#[cfg(feature = "gl-backend")]
pub mod gl;
#[cfg(all(feature = "metal-backend", any(target_os = "macos", target_os = "ios")))]
pub mod metal;
pub mod types;
#[cfg(all(
    feature = "vulkan-backend",
    any(target_os = "android", target_os = "windows", target_os = "linux")
))]
pub mod vulkan;

pub use types::{
    Extent2D, FrameTarget, NativeSurface, NativeSurfaceKind, PipelineId, RenderTarget,
    RenderTargetDesc, RenderTargetId, ShaderId, TextureData, TextureDesc, TextureFormat,
    TextureUpdate, TextureUsage,
};

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
}

/// Complete GPU ownership boundary used by [`crate::runtime::CoreRuntime`].
///
/// The trait deliberately covers only responsibilities that currently cross
/// the runtime/backend boundary. It keeps [`TextureId`] and [`DrawList`]
/// stable while allowing a future Metal or Vulkan implementation to own its
/// native device, render targets and presentation objects.
/// [`TextureProvider`] is only the draw-list construction view of this same
/// owner, so texture resolution and submission share one synchronization domain.
pub trait GpuBackend: TextureProvider {
    /// Save the host graphics state and make this backend ready for work.
    /// Calls may nest; every call must be paired with [`Self::end_access`].
    fn begin_access(&mut self);
    fn end_access(&mut self);

    /// Creates a named texture resource. Names are for cache/debug identity and
    /// are not backend handles.
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
    /// Releases logical ownership. Implementations may defer physical deletion
    /// until every in-flight frame that references the texture has completed.
    fn destroy_texture(&mut self, texture: TextureId);

    fn create_render_target(
        &mut self,
        name: &str,
        desc: RenderTargetDesc,
    ) -> Result<RenderTarget, String>;
    /// Like [`Self::destroy_texture`], this may enqueue deferred retirement.
    fn destroy_render_target(&mut self, target: RenderTargetId);

    fn resize(&mut self, extent: Extent2D) -> Result<(), String>;
    fn begin_frame(&mut self, target: FrameTarget) -> Result<(), String>;
    /// Clears the currently selected frame target.
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

    fn set_native_surface(&mut self, surface: NativeSurface) -> Result<(), String>;
    fn clear_native_surface(&mut self);
    fn present(&mut self, damage: Option<[f32; 4]>) -> Result<(), String>;

    fn register_hlsl_shader(&mut self, name: &str, source: &[u8]) -> Result<ShaderId, String>;
    /// Gives the backend an explicit opportunity to retire resources whose GPU
    /// submissions have completed. GL may no-op; Vulkan/Metal can poll fences.
    fn collect_retired_resources(&mut self) {}
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

/// Backend choice kept outside `CoreRuntime`.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendSelection {
    #[cfg(all(feature = "metal-backend", any(target_os = "macos", target_os = "ios")))]
    NativeMetal,
    #[cfg(all(
        feature = "vulkan-backend",
        any(target_os = "android", target_os = "windows", target_os = "linux")
    ))]
    NativeVulkan,
    #[cfg(feature = "gl-backend")]
    ReferenceGl(gl::platform::GfxBackend),
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
impl BackendSelection {
    /// Preserves legacy ANGLE selections while making native Metal the Apple
    /// default. On Apple, values 0 and 3 select Metal; value 5 explicitly
    /// selects CGL and value 6 explicitly selects ANGLE-Metal for A/B
    /// comparison. Android values 0 and 1 select ANGLE/OpenGL ES; value 2 is
    /// the experimental native Vulkan backend. Setting `ART3M1S_FORCE_GL`
    /// restores the legacy GL/ANGLE mappings for A/B tests; an iOS default
    /// selection is routed to ANGLE-Metal because CGL is absent.
    pub fn from_legacy_int(value: i32) -> Self {
        #[cfg(all(feature = "metal-backend", any(target_os = "macos", target_os = "ios")))]
        if std::env::var_os("ART3M1S_FORCE_GL").is_none() && matches!(value, 0 | 3) {
            return Self::NativeMetal;
        }

        #[cfg(all(feature = "vulkan-backend", target_os = "android"))]
        if std::env::var_os("ART3M1S_FORCE_GL").is_none() && value == 2 {
            return Self::NativeVulkan;
        }

        #[cfg(feature = "gl-backend")]
        {
            #[cfg(target_os = "ios")]
            if std::env::var_os("ART3M1S_FORCE_GL").is_some() && value == 0 {
                return Self::ReferenceGl(gl::platform::GfxBackend::Angle(
                    gl::platform::AngleBackend::Metal,
                ));
            }
            #[cfg(target_os = "android")]
            if matches!(value, 0 | 1) {
                return Self::ReferenceGl(gl::platform::GfxBackend::Angle(
                    gl::platform::AngleBackend::OpenGL,
                ));
            }
            Self::ReferenceGl(gl::platform::GfxBackend::from_int(value))
        }

        #[cfg(not(feature = "gl-backend"))]
        {
            panic!("no GPU backend is enabled for selection {value}")
        }
    }
}

#[cfg(feature = "gl-backend")]
impl From<gl::platform::GfxBackend> for BackendSelection {
    fn from(value: gl::platform::GfxBackend) -> Self {
        Self::ReferenceGl(value)
    }
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub(crate) fn create_backend(
    selection: BackendSelection,
    width: u32,
    height: u32,
) -> Result<Box<dyn GpuBackend>, String> {
    match selection {
        #[cfg(all(feature = "metal-backend", any(target_os = "macos", target_os = "ios")))]
        BackendSelection::NativeMetal => Ok(Box::new(metal::MetalBackend::new(width, height)?)),
        #[cfg(all(
            feature = "vulkan-backend",
            any(target_os = "android", target_os = "windows", target_os = "linux")
        ))]
        BackendSelection::NativeVulkan => Ok(Box::new(vulkan::VulkanBackend::new(width, height)?)),
        #[cfg(feature = "gl-backend")]
        BackendSelection::ReferenceGl(config) => {
            Ok(Box::new(gl::GlBackend::new(config, width, height)?))
        }
    }
}
