//! Backend-neutral external textures, video surfaces and screenshot targets.
//!
//! Runtime and `render_pipeline` see only opaque handles and ordinary
//! [`TextureId`] values. Concrete GPU objects (`GLuint`, `MTLTexture`,
//! `VkImage`, `EGLImage`, `AHardwareBuffer`) stay inside the active backend.
//!
//! # Ownership
//!
//! [`ResourceOwnership`] describes who releases the *native* producer object
//! passed across FFI:
//!
//! - [`ResourceOwnership::Borrowed`]: the host keeps the object alive until
//!   this frame is consumed or released. Core may hold a derived GPU view
//!   (for example a `CVMetalTexture`) until GPU work completes.
//! - [`ResourceOwnership::Imported`]: core retains the native object. The host
//!   may drop its own reference as soon as import returns success.
//! - [`ResourceOwnership::Owned`]: the host transfers its retain/refcount.
//!   Core will release the native object after GPU consumers finish.
//!
//! # Synchronization
//!
//! Layer video is a producer/consumer pair. The producer is the host decoder
//! (VideoToolbox, libmpv, MediaCodec, …). The consumer is the compositor
//! inside the active GPU backend.
//!
//! 1. **Producer finished writing** when it calls
//!    [`GpuBackend::import_external_texture`] or
//!    [`GpuBackend::commit_video_surface`]. Optional [`GpuSyncToken`] carries a
//!    platform fence/event if the decoder exposes one. `kind = None` means the
//!    host has already CPU/GPU-synchronized the native image.
//! 2. **Renderer may read** after that call returns success. The named video
//!    texture is then a regular sampled [`TextureId`] for the next
//!    `begin_frame` / `render`. Import does not present by itself.
//! 3. **Producer may reuse** the native object after
//!    [`GpuBackend::video_surface_consumed`] returns true for the handle of
//!    that frame. Replacement, layer stop, or runtime destroy retires the
//!    previous retain and releases it only after in-flight command buffers
//!    complete.
//!
//! Screenshot capture uses [`FrameTargetHandle`] / [`GpuBackend::capture_screenshot`]
//! and never a [`VideoSurfaceHandle`]. The two paths do not share framebuffer
//! ABI.

use super::{Extent2D, TextureFormat};

/// Compositor-facing imported or owned texture. Never a GLuint or MTLTexture*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExternalTextureHandle(u64);

impl ExternalTextureHandle {
    pub const fn from_opaque(value: u64) -> Self {
        Self(value)
    }

    pub const fn opaque(self) -> u64 {
        self.0
    }

    pub const fn is_null(self) -> bool {
        self.0 == 0
    }
}

/// Producer-facing video destination or imported frame lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VideoSurfaceHandle(u64);

impl VideoSurfaceHandle {
    pub const fn from_opaque(value: u64) -> Self {
        Self(value)
    }

    pub const fn opaque(self) -> u64 {
        self.0
    }

    pub const fn is_null(self) -> bool {
        self.0 == 0
    }
}

/// Screenshot / offscreen destination. Distinct from video surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameTargetHandle(u64);

impl FrameTargetHandle {
    pub const MAIN: Self = Self(0);

    pub const fn from_opaque(value: u64) -> Self {
        Self(value)
    }

    pub const fn opaque(self) -> u64 {
        self.0
    }
}

/// Who releases the native producer object passed across FFI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceOwnership {
    Borrowed,
    Imported,
    Owned,
}

impl ResourceOwnership {
    pub fn from_ffi(value: i32) -> Result<Self, String> {
        match value {
            0 => Ok(Self::Borrowed),
            1 => Ok(Self::Imported),
            2 => Ok(Self::Owned),
            _ => Err(format!("unsupported resource ownership: {value}")),
        }
    }

    pub const fn ffi_value(self) -> i32 {
        match self {
            Self::Borrowed => 0,
            Self::Imported => 1,
            Self::Owned => 2,
        }
    }
}

/// Native image kinds accepted by [`ExternalImage`].
///
/// These integers are independent of [`crate::backend::NativeSurfaceKind`],
/// which describes the *presentation* surface rather than video frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExternalImageKind {
    /// Host-decoded tightly packed top-left RGBA8. CPU upload fallback.
    CpuRgba,
    /// Apple `CVPixelBufferRef`. Darwin production import.
    CvPixelBuffer,
    /// Apple `MTLTexture` object pointer.
    MetalTexture,
    /// Apple `IOSurfaceRef` (single-plane BGRA8).
    IoSurface,
    /// Android `AHardwareBuffer*` for a future Vulkan import.
    AHardwareBuffer,
    /// Legacy GL framebuffer name. Only the reference GL backend accepts this
    /// as a *query* through the deprecated `video_gl_*` shim, not as import.
    OpenGlFramebuffer,
}

impl ExternalImageKind {
    pub fn from_ffi(value: i32) -> Result<Self, String> {
        match value {
            0 => Ok(Self::CpuRgba),
            1 => Ok(Self::CvPixelBuffer),
            2 => Ok(Self::MetalTexture),
            3 => Ok(Self::IoSurface),
            4 => Ok(Self::AHardwareBuffer),
            5 => Ok(Self::OpenGlFramebuffer),
            _ => Err(format!("unsupported external image kind: {value}")),
        }
    }

    pub const fn ffi_value(self) -> i32 {
        match self {
            Self::CpuRgba => 0,
            Self::CvPixelBuffer => 1,
            Self::MetalTexture => 2,
            Self::IoSurface => 3,
            Self::AHardwareBuffer => 4,
            Self::OpenGlFramebuffer => 5,
        }
    }
}

/// Optional producer fence/event. `None` means the host already synchronized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpuSyncKind {
    None,
    /// `id<MTLSharedEvent>` plus `value`.
    MetalSharedEvent,
    /// Future Vulkan binary/timeline semaphore pointer.
    VulkanSemaphore,
}

impl GpuSyncKind {
    pub fn from_ffi(value: i32) -> Result<Self, String> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::MetalSharedEvent),
            2 => Ok(Self::VulkanSemaphore),
            _ => Err(format!("unsupported GPU sync kind: {value}")),
        }
    }

    pub const fn ffi_value(self) -> i32 {
        match self {
            Self::None => 0,
            Self::MetalSharedEvent => 1,
            Self::VulkanSemaphore => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GpuSyncToken {
    pub kind: GpuSyncKind,
    pub handle: *mut std::ffi::c_void,
    pub value: u64,
}

impl GpuSyncToken {
    pub const NONE: Self = Self {
        kind: GpuSyncKind::None,
        handle: std::ptr::null_mut(),
        value: 0,
    };

    pub fn from_ffi(kind: i32, handle: *mut std::ffi::c_void, value: u64) -> Result<Self, String> {
        let kind = GpuSyncKind::from_ffi(kind)?;
        if kind != GpuSyncKind::None && handle.is_null() {
            return Err("GPU sync handle is required when wait_kind is not None".into());
        }
        Ok(Self {
            kind,
            handle,
            value,
        })
    }

    pub const fn is_none(self) -> bool {
        matches!(self.kind, GpuSyncKind::None)
    }
}

/// Borrowed native image plus CPU fallback payload.
pub struct ExternalImage<'a> {
    pub kind: ExternalImageKind,
    pub handle: *mut std::ffi::c_void,
    pub extent: Extent2D,
    pub format: TextureFormat,
    pub ownership: ResourceOwnership,
    pub wait: GpuSyncToken,
    pub rgba: Option<&'a [u8]>,
}

impl<'a> ExternalImage<'a> {
    pub fn from_ffi_parts(
        kind: i32,
        handle: *mut std::ffi::c_void,
        width: u32,
        height: u32,
        ownership: i32,
        wait_kind: i32,
        wait_handle: *mut std::ffi::c_void,
        wait_value: u64,
        rgba: Option<&'a [u8]>,
    ) -> Result<Self, String> {
        let kind = ExternalImageKind::from_ffi(kind)?;
        let ownership = ResourceOwnership::from_ffi(ownership)?;
        let wait = GpuSyncToken::from_ffi(wait_kind, wait_handle, wait_value)?;
        let extent = Extent2D::new(width, height);
        if extent.is_empty() {
            return Err("external image extent must be non-zero".into());
        }
        match kind {
            ExternalImageKind::CpuRgba => {
                let expected = extent
                    .rgba8_len()
                    .ok_or_else(|| "external image size overflow".to_string())?;
                let rgba =
                    rgba.ok_or_else(|| "CPU RGBA import requires pixel bytes".to_string())?;
                if rgba.len() < expected {
                    return Err(format!(
                        "CPU RGBA import has {} bytes, expected at least {expected}",
                        rgba.len()
                    ));
                }
            }
            ExternalImageKind::OpenGlFramebuffer => {
                return Err(
                    "OpenGL framebuffer is not an import kind; use the deprecated video_gl_* shim"
                        .into(),
                );
            }
            _ if handle.is_null() => {
                return Err(format!(
                    "native handle is required for external image kind {}",
                    kind.ffi_value()
                ));
            }
            _ => {}
        }
        Ok(Self {
            kind,
            handle,
            extent,
            format: TextureFormat::Rgba8Unorm,
            ownership,
            wait,
            rgba,
        })
    }
}

/// What the active backend can accept for layer video.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoImportCapability {
    pub preferred: ExternalImageKind,
    pub cpu_rgba: bool,
    pub cv_pixel_buffer: bool,
    pub metal_texture: bool,
    pub io_surface: bool,
    pub ahardware_buffer: bool,
    pub opengl_framebuffer: bool,
}

impl VideoImportCapability {
    pub fn cpu_only() -> Self {
        Self {
            preferred: ExternalImageKind::CpuRgba,
            cpu_rgba: true,
            cv_pixel_buffer: false,
            metal_texture: false,
            io_surface: false,
            ahardware_buffer: false,
            opengl_framebuffer: false,
        }
    }

    pub const fn ffi_preferred(self) -> i32 {
        self.preferred.ffi_value()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffi_integers_are_stable() {
        assert_eq!(ExternalImageKind::CpuRgba.ffi_value(), 0);
        assert_eq!(ExternalImageKind::CvPixelBuffer.ffi_value(), 1);
        assert_eq!(ExternalImageKind::MetalTexture.ffi_value(), 2);
        assert_eq!(ExternalImageKind::IoSurface.ffi_value(), 3);
        assert_eq!(ExternalImageKind::AHardwareBuffer.ffi_value(), 4);
        assert_eq!(ExternalImageKind::OpenGlFramebuffer.ffi_value(), 5);
        assert_eq!(ResourceOwnership::Borrowed.ffi_value(), 0);
        assert_eq!(ResourceOwnership::Imported.ffi_value(), 1);
        assert_eq!(ResourceOwnership::Owned.ffi_value(), 2);
        assert_eq!(GpuSyncKind::None.ffi_value(), 0);
        assert_eq!(GpuSyncKind::MetalSharedEvent.ffi_value(), 1);
        assert_eq!(GpuSyncKind::VulkanSemaphore.ffi_value(), 2);
        assert_eq!(FrameTargetHandle::MAIN.opaque(), 0);
    }

    #[test]
    fn cpu_rgba_import_validates_payload() {
        let pixels = [1u8; 16];
        let image = ExternalImage::from_ffi_parts(
            0,
            std::ptr::null_mut(),
            2,
            2,
            0,
            0,
            std::ptr::null_mut(),
            0,
            Some(&pixels),
        )
        .unwrap();
        assert_eq!(image.kind, ExternalImageKind::CpuRgba);
        assert!(
            ExternalImage::from_ffi_parts(
                0,
                std::ptr::null_mut(),
                2,
                2,
                0,
                0,
                std::ptr::null_mut(),
                0,
                Some(&[1u8; 8]),
            )
            .is_err()
        );
        assert!(
            ExternalImage::from_ffi_parts(
                1,
                std::ptr::null_mut(),
                2,
                2,
                0,
                0,
                std::ptr::null_mut(),
                0,
                None,
            )
            .is_err()
        );
        assert!(
            ExternalImage::from_ffi_parts(
                5,
                1usize as *mut _,
                2,
                2,
                0,
                0,
                std::ptr::null_mut(),
                0,
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn handles_are_opaque_and_null_zero() {
        assert!(ExternalTextureHandle::from_opaque(0).is_null());
        assert!(!ExternalTextureHandle::from_opaque(7).is_null());
        assert_eq!(VideoSurfaceHandle::from_opaque(9).opaque(), 9);
    }
}
