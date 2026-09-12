//! GPU backend selection and shared backend re-exports.
//!
//! The backend contract and concrete implementations live in
//! `art3m1s-render`. Core keeps only the product/platform selection policy and
//! legacy fallback order.

pub use art3m1s_render::backend::*;
pub use art3m1s_render::{external, types};

#[cfg(feature = "gl-backend")]
pub(crate) fn set_angle_path_prefix(prefix: &str) -> bool {
    gl::platform::set_angle_path_prefix(prefix)
}

#[cfg(not(feature = "gl-backend"))]
pub(crate) fn set_angle_path_prefix(_prefix: &str) -> bool {
    false
}

/// Backend choice kept outside `CoreRuntime`.
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

pub(crate) fn create_backend(
    selection: BackendSelection,
    width: u32,
    height: u32,
) -> Result<Box<dyn GpuBackend>, String> {
    crate::host::logging::install_once();
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

fn create_platform_default(width: u32, height: u32) -> Result<Box<dyn GpuBackend>, String> {
    let _ = (width, height);

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
