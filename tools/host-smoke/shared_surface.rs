use art3m1s_render::Extent2D;
use std::{ffi::c_void, ptr::NonNull};

unsafe extern "C" {
    fn metal_create_pixel_buffer(width: u32, height: u32) -> *mut c_void;
    fn metal_pixel_buffer_surface(buffer: *mut c_void) -> *mut c_void;
    fn metal_free_pixel_buffer(buffer: *mut c_void);
    fn metal_read_pixel_buffer(buffer: *mut c_void, rgba: *mut u8, size: u64) -> i32;
}

pub struct SharedSurface {
    buffer: NonNull<c_void>,
    extent: Extent2D,
}

impl SharedSurface {
    pub fn new(extent: Extent2D) -> Result<Self, String> {
        let buffer =
            NonNull::new(unsafe { metal_create_pixel_buffer(extent.width, extent.height) })
                .ok_or("cannot allocate shared pixel buffer")?;
        Ok(Self { buffer, extent })
    }

    pub fn handle(&self) -> Result<NonNull<c_void>, String> {
        NonNull::new(unsafe { metal_pixel_buffer_surface(self.buffer.as_ptr()) })
            .ok_or_else(|| "pixel buffer has no IOSurface".into())
    }

    pub fn pixels(&self) -> Result<Vec<u8>, String> {
        let mut rgba = vec![0; self.extent.rgba8_len().ok_or("invalid buffer extent")?];
        let status = unsafe {
            metal_read_pixel_buffer(self.buffer.as_ptr(), rgba.as_mut_ptr(), rgba.len() as u64)
        };
        if status != 0 {
            return Err(format!("read shared pixel buffer returned {status}"));
        }
        Ok(rgba)
    }
}

impl Drop for SharedSurface {
    fn drop(&mut self) {
        unsafe { metal_free_pixel_buffer(self.buffer.as_ptr()) };
    }
}
