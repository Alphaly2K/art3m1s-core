use crate::render_pipeline::draw::TextureId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Extent2D {
    pub width: u32,
    pub height: u32,
}

impl Extent2D {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub fn rgba8_len(self) -> Option<usize> {
        (self.width as usize)
            .checked_mul(self.height as usize)?
            .checked_mul(4)
    }

    pub fn block_4x4_len(self) -> Option<usize> {
        let blocks_w = self.width.checked_add(3)? / 4;
        let blocks_h = self.height.checked_add(3)? / 4;
        (blocks_w as usize)
            .checked_mul(blocks_h as usize)?
            .checked_mul(16)
    }
}

/// Formats currently consumed by the Artemis render path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureFormat {
    Rgba8Unorm,
    Bgra8Unorm,
    Bc3RgbaUnorm,
    Astc4x4RgbaUnorm,
}

/// Backend-neutral texture capabilities. Backends translate these flags into
/// API-specific image/texture usage bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureUsage(u8);

impl TextureUsage {
    pub const SAMPLED: Self = Self(1 << 0);
    pub const RENDER_TARGET: Self = Self(1 << 1);
    pub const TRANSFER_SRC: Self = Self(1 << 2);
    pub const TRANSFER_DST: Self = Self(1 << 3);
    pub const CPU_READABLE: Self = Self(1 << 4);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for TextureUsage {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for TextureUsage {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureDesc {
    pub extent: Extent2D,
    pub format: TextureFormat,
    pub usage: TextureUsage,
}

impl TextureDesc {
    pub const fn sampled_rgba8(width: u32, height: u32) -> Self {
        Self {
            extent: Extent2D::new(width, height),
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsage::SAMPLED.union(TextureUsage::TRANSFER_DST),
        }
    }
}

impl TextureUsage {
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Initial texture payload. The variant must agree with [`TextureDesc::format`].
pub enum TextureData<'a> {
    Uninitialized,
    Rgba8(&'a [u8]),
    Bc3(&'a [u8]),
    Astc4x4(&'a [u8]),
}

/// A subresource update. Compressed partial updates may be rejected by a
/// backend when their block alignment is invalid.
pub struct TextureUpdate<'a> {
    pub origin: [u32; 2],
    pub extent: Extent2D,
    pub data: TextureData<'a>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderTargetId(u64);

impl RenderTargetId {
    #[allow(dead_code)]
    pub(crate) const fn from_opaque(value: u64) -> Self {
        Self(value)
    }

    #[allow(dead_code)]
    pub(crate) const fn opaque(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderTargetDesc {
    pub extent: Extent2D,
    pub color_format: TextureFormat,
    pub sampled: bool,
    /// Requests a stencil attachment. The current GL renderer implements its
    /// Artemis stencil groups through mask passes and therefore leaves this false.
    pub stencil: bool,
}

impl RenderTargetDesc {
    pub const fn sampled_rgba8(width: u32, height: u32) -> Self {
        Self {
            extent: Extent2D::new(width, height),
            color_format: TextureFormat::Rgba8Unorm,
            sampled: true,
            stencil: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenderTarget {
    pub id: RenderTargetId,
    pub color: TextureId,
    pub desc: RenderTargetDesc,
}

/// Target selected for one backend frame. Native command buffers/encoders stay
/// private to the backend and are never represented here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FrameTarget {
    #[default]
    Main,
    Offscreen(RenderTargetId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NativeSurfaceKind {
    AndroidNativeWindow,
    AppleIoSurface,
    AppleMetalTexture,
    /// Host-owned CAMetalLayer. MetalBackend retains the layer and obtains a
    /// fresh drawable for each presentation.
    AppleMetalLayer,
}

impl NativeSurfaceKind {
    pub fn from_legacy_int(value: i32) -> Result<Self, String> {
        match value {
            1 => Ok(Self::AndroidNativeWindow),
            2 => Ok(Self::AppleIoSurface),
            3 => Ok(Self::AppleMetalTexture),
            4 => Ok(Self::AppleMetalLayer),
            _ => Err(format!("unsupported native surface kind: {value}")),
        }
    }

    #[cfg(feature = "gl-backend")]
    pub(crate) fn legacy_int(self) -> i32 {
        match self {
            Self::AndroidNativeWindow => 1,
            Self::AppleIoSurface => 2,
            Self::AppleMetalTexture => 3,
            Self::AppleMetalLayer => 4,
        }
    }
}

/// Borrowed ownership contract with the host: the host keeps the native object
/// alive until it replaces or clears the surface.
#[derive(Debug, Clone, Copy)]
pub struct NativeSurface {
    pub kind: NativeSurfaceKind,
    pub handle: *mut std::ffi::c_void,
    pub extent: Extent2D,
}

impl NativeSurface {
    pub fn from_legacy_parts(
        kind: i32,
        handle: *mut std::ffi::c_void,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        let extent = Extent2D::new(width, height);
        if handle.is_null() || extent.is_empty() {
            return Err("invalid native surface".into());
        }
        Ok(Self {
            kind: NativeSurfaceKind::from_legacy_int(kind)?,
            handle,
            extent,
        })
    }
}

/// Opaque backend shader allocation. Draw commands continue to use semantic
/// shader names until the runtime shader registry is made backend-neutral.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShaderId(u64);

impl ShaderId {
    #[allow(dead_code)]
    pub(crate) const fn from_opaque(value: u64) -> Self {
        Self(value)
    }
}

/// Opaque cached graphics-pipeline identity. It intentionally does not appear
/// in `DrawCommand`; each backend derives/caches pipelines from semantic state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PipelineId(u64);

impl PipelineId {
    #[allow(dead_code)]
    pub(crate) const fn from_opaque(value: u64) -> Self {
        Self(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_usage_combines_without_exposing_api_flags() {
        let usage = TextureUsage::SAMPLED | TextureUsage::TRANSFER_DST;
        assert!(usage.contains(TextureUsage::SAMPLED));
        assert!(usage.contains(TextureUsage::TRANSFER_DST));
        assert!(!usage.contains(TextureUsage::RENDER_TARGET));
    }

    #[test]
    fn native_surface_validates_legacy_ffi_parts() {
        assert!(NativeSurface::from_legacy_parts(1, 1usize as *mut _, 16, 9).is_ok());
        assert!(NativeSurface::from_legacy_parts(0, 1usize as *mut _, 16, 9).is_err());
        assert!(NativeSurface::from_legacy_parts(1, std::ptr::null_mut(), 16, 9).is_err());
    }

    #[test]
    fn rgba_length_is_checked() {
        assert_eq!(Extent2D::new(4, 3).rgba8_len(), Some(48));
        assert_eq!(Extent2D::new(5, 7).block_4x4_len(), Some(64));
    }
}
