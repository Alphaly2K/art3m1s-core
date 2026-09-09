//! GPU backend boundary.
//!
//! Runtime and render-pipeline code talk only to [`GpuBackend`]. Concrete API
//! state (contexts, command encoders, framebuffers, swapchains and native
//! texture handles) belongs to an implementation below this module.

pub use crate::render_pipeline::draw::TextureOrigin;
use crate::render_pipeline::draw::{DrawList, TextureId, TextureInfo, TextureProvider};
use crate::render_pipeline::post_process::{
    PostProcessPass, PostProcessPipeline, RenderDimensions, UpscaleMode,
};
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

pub use crate::render_pipeline::hlsl::{ShaderCompileError, ShaderId, ShaderSource};
pub use types::{
    BackendCapabilities, BackendInfo, BackendKind, BackendStability, Extent2D, FrameTarget,
    NativeSurface, NativeSurfaceKind, PipelineId, RenderTarget, RenderTargetDesc, RenderTargetId,
    TextureData, TextureDesc, TextureFormat, TextureUpdate, TextureUsage,
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
    pub upscale_enabled: bool,
    pub upscale_cpu_encode_ns: u64,
    pub upscale_gpu_ns: u64,
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
    fn backend_info(&self) -> BackendInfo;

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
    /// Returns the scene render target size and the current presentation size.
    /// `None` means the legacy backend has no explicit post-process surface.
    fn render_dimensions(&self) -> Option<RenderDimensions> {
        None
    }
    /// Changes SceneColor scale relative to the output surface while
    /// preserving logical scene coordinates. Native backends keep the result
    /// at least as large as the logical scene and recreate cached targets only
    /// when the resolved extent changes.
    fn set_render_scale(&mut self, scale: f32) -> Result<(), String> {
        if (scale - 1.0).abs() <= f32::EPSILON {
            Ok(())
        } else {
            Err("render scale is unsupported by this backend".into())
        }
    }
    /// Installs a linear post-process description. Native handles remain
    /// private to the backend; unsupported future passes are rejected here.
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

    fn register_hlsl_shader(
        &mut self,
        name: &str,
        source: &[u8],
    ) -> Result<ShaderId, ShaderCompileError>;
    /// Recompiles a logical shader while preserving its `ShaderId`. Backends
    /// must invalidate every native pipeline derived from the old generation.
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
    PlatformDefault,
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
    /// Maps the stable host integer ABI into an implementation-neutral choice.
    /// Value 0 is the platform default (Metal on Darwin, Vulkan on
    /// Android/Windows/Linux). Values 2 and 3 explicitly select Vulkan and
    /// Metal where those native backends are available. Other legacy values,
    /// or every value while `ART3M1S_FORCE_GL` is set, retain the GL/ANGLE
    /// debug paths used for A/B comparisons and regression tests.
    pub fn try_from_legacy_int(value: i32) -> Result<Self, String> {
        let force_gl = std::env::var_os("ART3M1S_FORCE_GL").is_some();
        if value == 0 && !force_gl {
            return Ok(Self::PlatformDefault);
        }

        #[cfg(all(feature = "metal-backend", any(target_os = "macos", target_os = "ios")))]
        if !force_gl && value == 3 {
            return Ok(Self::NativeMetal);
        }

        #[cfg(all(
            feature = "vulkan-backend",
            any(target_os = "android", target_os = "windows", target_os = "linux")
        ))]
        if !force_gl && value == 2 {
            return Ok(Self::NativeVulkan);
        }

        #[cfg(feature = "gl-backend")]
        return Ok(Self::ReferenceGl(reference_gl_selection(value)));

        #[cfg(not(feature = "gl-backend"))]
        Err(format!(
            "GL backend override {value} is unavailable in this build"
        ))
    }

    /// Compatibility helper for Rust callers. Invalid overrides degrade to the
    /// platform default; FFI uses the fallible function and reports the error.
    pub fn from_legacy_int(value: i32) -> Self {
        Self::try_from_legacy_int(value).unwrap_or(Self::PlatformDefault)
    }
}

#[cfg(feature = "gl-backend")]
fn reference_gl_selection(value: i32) -> gl::platform::GfxBackend {
    if value != 0 {
        return gl::platform::GfxBackend::from_int(value);
    }
    #[cfg(target_os = "macos")]
    return gl::platform::GfxBackend::Cgl;
    #[cfg(target_os = "ios")]
    return gl::platform::GfxBackend::Angle(gl::platform::AngleBackend::Metal);
    #[cfg(target_os = "android")]
    return gl::platform::GfxBackend::Angle(gl::platform::AngleBackend::OpenGL);
    #[cfg(target_os = "windows")]
    return gl::platform::GfxBackend::Angle(gl::platform::AngleBackend::D3D11);
    #[cfg(target_os = "linux")]
    return gl::platform::GfxBackend::Angle(gl::platform::AngleBackend::OpenGL);
    #[allow(unreachable_code)]
    gl::platform::GfxBackend::from_int(0)
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
        BackendSelection::PlatformDefault => create_platform_default(width, height),
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

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
fn create_platform_default(width: u32, height: u32) -> Result<Box<dyn GpuBackend>, String> {
    #[cfg(all(feature = "metal-backend", any(target_os = "macos", target_os = "ios")))]
    {
        return match metal::MetalBackend::new(width, height) {
            Ok(backend) => Ok(Box::new(backend)),
            Err(native_error) => fallback_to_gl(width, height, "Metal", native_error),
        };
    }

    #[cfg(all(
        feature = "vulkan-backend",
        any(target_os = "android", target_os = "windows", target_os = "linux")
    ))]
    {
        return match vulkan::VulkanBackend::new(width, height) {
            Ok(backend) => Ok(Box::new(backend)),
            Err(native_error) => fallback_to_gl(width, height, "Vulkan", native_error),
        };
    }

    #[cfg(all(
        feature = "gl-backend",
        not(any(
            all(feature = "metal-backend", any(target_os = "macos", target_os = "ios")),
            all(
                feature = "vulkan-backend",
                any(target_os = "android", target_os = "windows", target_os = "linux")
            )
        ))
    ))]
    return Ok(Box::new(gl::GlBackend::new(
        reference_gl_selection(0),
        width,
        height,
    )?));

    #[allow(unreachable_code)]
    Err("no GPU backend is available for this platform".into())
}

#[cfg(any(
    all(feature = "metal-backend", any(target_os = "macos", target_os = "ios")),
    all(
        feature = "vulkan-backend",
        any(target_os = "android", target_os = "windows", target_os = "linux")
    )
))]
fn fallback_to_gl(
    width: u32,
    height: u32,
    native_name: &str,
    native_error: String,
) -> Result<Box<dyn GpuBackend>, String> {
    #[cfg(feature = "gl-backend")]
    {
        crate::core_warn!(
            "[GpuBackend] platform-default {native_name} initialization failed; using legacy GL: {native_error}"
        );
        return Ok(Box::new(gl::GlBackend::new(
            reference_gl_selection(0),
            width,
            height,
        )?));
    }
    #[cfg(not(feature = "gl-backend"))]
    {
        let _ = (width, height);
        Err(format!(
            "platform-default {native_name} initialization failed and no GL fallback is built: {native_error}"
        ))
    }
}
