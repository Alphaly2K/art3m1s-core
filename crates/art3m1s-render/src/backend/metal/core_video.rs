//! CoreVideo helpers used by MetalBackend's zero-copy video import.
//!
//! The compositor never sees `CVPixelBuffer` or `CVMetalTexture`. This module
//! retains native objects and vends `MTLTexture` views.

use crate::backend::{Extent2D, TextureFormat};
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLDevice, MTLPixelFormat, MTLTexture};
use std::ffi::c_void;
use std::ptr::NonNull;

pub type CvPixelBufferRef = *mut c_void;
pub type CvMetalTextureRef = *mut c_void;
pub type CvMetalTextureCacheRef = *mut c_void;

#[allow(non_upper_case_globals)]
const kCVPixelFormatType_32BGRA: u32 = 0x4247_5241; // 'BGRA'
#[allow(non_upper_case_globals)]
const kCVPixelFormatType_32RGBA: u32 = 0x5247_4241; // 'RGBA'
#[allow(non_upper_case_globals)]
const kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange: u32 = 0x3432_3076; // '420v'
#[allow(non_upper_case_globals)]
const kCVPixelFormatType_420YpCbCr8BiPlanarFullRange: u32 = 0x3432_3066; // '420f'

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRetain(cf: *const c_void) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    fn CVMetalTextureCacheCreate(
        allocator: *const c_void,
        cache_attributes: *const c_void,
        metal_device: *const c_void,
        texture_attributes: *const c_void,
        cache_out: *mut CvMetalTextureCacheRef,
    ) -> i32;
    fn CVMetalTextureCacheCreateTextureFromImage(
        allocator: *const c_void,
        texture_cache: CvMetalTextureCacheRef,
        source_image: CvPixelBufferRef,
        texture_attributes: *const c_void,
        pixel_format: usize,
        width: usize,
        height: usize,
        plane_index: usize,
        texture_out: *mut CvMetalTextureRef,
    ) -> i32;
    fn CVMetalTextureGetTexture(image: CvMetalTextureRef) -> *mut ProtocolObject<dyn MTLTexture>;
    fn CVMetalTextureCacheFlush(texture_cache: CvMetalTextureCacheRef, options: u64);
    fn CVPixelBufferGetWidth(pixel_buffer: CvPixelBufferRef) -> usize;
    fn CVPixelBufferGetHeight(pixel_buffer: CvPixelBufferRef) -> usize;
    fn CVPixelBufferGetPixelFormatType(pixel_buffer: CvPixelBufferRef) -> u32;
    fn CVPixelBufferGetPlaneCount(pixel_buffer: CvPixelBufferRef) -> usize;
    fn CVPixelBufferGetIOSurface(pixel_buffer: CvPixelBufferRef) -> *mut c_void;
}

pub struct CfPtr {
    ptr: *mut c_void,
}

impl CfPtr {
    pub unsafe fn retain(ptr: *mut c_void) -> Result<Self, String> {
        if ptr.is_null() {
            return Err("CoreFoundation object pointer is null".into());
        }
        unsafe { CFRetain(ptr) };
        Ok(Self { ptr })
    }

    pub unsafe fn from_created(ptr: *mut c_void) -> Result<Self, String> {
        if ptr.is_null() {
            return Err("CoreFoundation create returned null".into());
        }
        Ok(Self { ptr })
    }

    pub fn as_ptr(&self) -> *mut c_void {
        self.ptr
    }
}

impl Drop for CfPtr {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { CFRelease(self.ptr) };
            self.ptr = std::ptr::null_mut();
        }
    }
}

pub struct CvMetalTextureCache {
    raw: CfPtr,
}

impl CvMetalTextureCache {
    pub fn new(device: &ProtocolObject<dyn MTLDevice>) -> Result<Self, String> {
        let mut cache = std::ptr::null_mut();
        let status = unsafe {
            CVMetalTextureCacheCreate(
                std::ptr::null(),
                std::ptr::null(),
                NonNull::from(device).as_ptr().cast(),
                std::ptr::null(),
                &mut cache,
            )
        };
        if status != 0 || cache.is_null() {
            return Err(format!(
                "CVMetalTextureCacheCreate failed with status {status}"
            ));
        }
        Ok(Self {
            raw: unsafe { CfPtr::from_created(cache) }?,
        })
    }

    pub fn flush(&self) {
        unsafe { CVMetalTextureCacheFlush(self.raw.as_ptr(), 0) };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CvColor {
    Bgra,
    Rgba,
    Nv12Video,
    Nv12Full,
}

impl CvColor {
    pub fn from_pixel_format(format: u32) -> Result<Self, String> {
        #[allow(non_upper_case_globals)]
        match format {
            kCVPixelFormatType_32BGRA => Ok(Self::Bgra),
            kCVPixelFormatType_32RGBA => Ok(Self::Rgba),
            kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange => Ok(Self::Nv12Video),
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange => Ok(Self::Nv12Full),
            other => Err(format!(
                "unsupported CVPixelBuffer format {other:#010x}; request 32BGRA or biplanar 420"
            )),
        }
    }

    pub fn metal_format(self) -> TextureFormat {
        match self {
            Self::Bgra => TextureFormat::Bgra8Unorm,
            Self::Rgba | Self::Nv12Video | Self::Nv12Full => TextureFormat::Rgba8Unorm,
        }
    }

    pub fn is_yuv(self) -> bool {
        matches!(self, Self::Nv12Video | Self::Nv12Full)
    }
}

pub struct CvBufferInfo {
    pub extent: Extent2D,
    pub color: CvColor,
    pub plane_count: usize,
    #[allow(dead_code)]
    pub iosurface: *mut c_void,
}

pub fn inspect_pixel_buffer(pixel_buffer: CvPixelBufferRef) -> Result<CvBufferInfo, String> {
    if pixel_buffer.is_null() {
        return Err("CVPixelBuffer pointer is null".into());
    }
    let width = unsafe { CVPixelBufferGetWidth(pixel_buffer) };
    let height = unsafe { CVPixelBufferGetHeight(pixel_buffer) };
    if width == 0 || height == 0 {
        return Err("CVPixelBuffer extent must be non-zero".into());
    }
    let format = unsafe { CVPixelBufferGetPixelFormatType(pixel_buffer) };
    Ok(CvBufferInfo {
        extent: Extent2D::new(width as u32, height as u32),
        color: CvColor::from_pixel_format(format)?,
        plane_count: unsafe { CVPixelBufferGetPlaneCount(pixel_buffer) },
        iosurface: unsafe { CVPixelBufferGetIOSurface(pixel_buffer) },
    })
}

pub struct CvMetalPlane {
    pub cv_texture: CfPtr,
    pub metal: objc2::rc::Retained<ProtocolObject<dyn MTLTexture>>,
}

pub fn create_plane_texture(
    cache: &CvMetalTextureCache,
    pixel_buffer: CvPixelBufferRef,
    pixel_format: MTLPixelFormat,
    width: usize,
    height: usize,
    plane: usize,
) -> Result<CvMetalPlane, String> {
    let mut cv_texture = std::ptr::null_mut();
    let status = unsafe {
        CVMetalTextureCacheCreateTextureFromImage(
            std::ptr::null(),
            cache.raw.as_ptr(),
            pixel_buffer,
            std::ptr::null(),
            pixel_format.0,
            width,
            height,
            plane,
            &mut cv_texture,
        )
    };
    if status != 0 || cv_texture.is_null() {
        return Err(format!(
            "CVMetalTextureCacheCreateTextureFromImage plane {plane} failed with status {status}"
        ));
    }
    let cv_texture = unsafe { CfPtr::from_created(cv_texture) }?;
    let metal_ptr = unsafe { CVMetalTextureGetTexture(cv_texture.as_ptr()) };
    let metal = unsafe { objc2::rc::Retained::retain(metal_ptr) }
        .ok_or_else(|| "CVMetalTextureGetTexture returned null".to_string())?;
    Ok(CvMetalPlane { cv_texture, metal })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_format_mapping_covers_production_decoder_output() {
        assert_eq!(
            CvColor::from_pixel_format(kCVPixelFormatType_32BGRA).unwrap(),
            CvColor::Bgra
        );
        assert_eq!(
            CvColor::from_pixel_format(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange).unwrap(),
            CvColor::Nv12Video
        );
        assert!(CvColor::from_pixel_format(0x4152_4742).is_err());
    }
}
