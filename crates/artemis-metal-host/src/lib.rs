use art3m1s_render::artemis::{FramePacket, SpriteRenderer};
use art3m1s_render::backend::{
    FrameTarget, GpuBackend, RenderRegion, gl::GlBackend, metal::MetalBackend,
};
use art3m1s_render::{Extent2D, NativeSurface, NativeSurfaceKind};
use std::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::ptr;
use std::thread::{self, ThreadId};

const OK: i32 = 0;
const ARGUMENT: i32 = -1;
const THREAD: i32 = -3;
const BUFFER: i32 = -5;
const STATE: i32 = -6;
const STALE: i32 = -7;
const PANIC: i32 = -9;
const UNSUPPORTED: i32 = -4;
const CLEAR_COLOR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

unsafe extern "C" {
    fn metal_host_check_frames(table: *const c_void) -> i32;
    fn metal_host_borrow(
        table: *const c_void,
        runtime: u64,
        after: u64,
        token: *mut u64,
        frame_id: *mut u64,
        packet: *mut *const u8,
        size: *mut u64,
    ) -> i32;
    fn metal_host_release(table: *const c_void, runtime: u64, token: u64) -> i32;
    fn metal_host_texture(
        table: *const c_void,
        runtime: u64,
        token: u64,
        index: u32,
        data: *mut *const u8,
        size: *mut u64,
    ) -> i32;
    fn metal_host_surface_extent(surface: *mut c_void, width: *mut u32, height: *mut u32) -> i32;
}

#[derive(Clone, Copy)]
struct Frames {
    table: *const c_void,
    runtime: u64,
}

struct Lease {
    frames: Frames,
    token: u64,
    id: u64,
    packet: *const u8,
    size: u64,
}

impl Frames {
    fn acquire(self, after: u64) -> Result<Lease, i32> {
        let mut frame = Lease {
            frames: self,
            token: 0,
            id: 0,
            packet: ptr::null(),
            size: 0,
        };
        status(unsafe {
            metal_host_borrow(
                self.table,
                self.runtime,
                after,
                &mut frame.token,
                &mut frame.id,
                &mut frame.packet,
                &mut frame.size,
            )
        })?;
        Ok(frame)
    }
}

impl Lease {
    fn decode(&self) -> Result<FramePacket, i32> {
        let packet = FramePacket::decode(unsafe { self.bytes(self.packet, self.size)? })
            .map_err(|_| ARGUMENT)?;
        if packet.render_frame_id != self.id {
            return Err(ARGUMENT);
        }
        Ok(packet)
    }

    fn pixels(&self, index: usize) -> Result<&[u8], i32> {
        let index = u32::try_from(index).map_err(|_| ARGUMENT)?;
        let mut data = ptr::null();
        let mut size = 0;
        status(unsafe {
            metal_host_texture(
                self.frames.table,
                self.frames.runtime,
                self.token,
                index,
                &mut data,
                &mut size,
            )
        })?;
        unsafe { self.bytes(data, size) }
    }

    unsafe fn bytes(&self, data: *const u8, size: u64) -> Result<&[u8], i32> {
        if size == 0 {
            return Ok(&[]);
        }
        if data.is_null() || size > isize::MAX as u64 {
            return Err(ARGUMENT);
        }
        // A successful Core borrow pins the byte storage until this lease releases it.
        Ok(unsafe { std::slice::from_raw_parts(data, size as usize) })
    }

    fn release(mut self) -> Result<(), i32> {
        let token = std::mem::take(&mut self.token);
        status(unsafe { metal_host_release(self.frames.table, self.frames.runtime, token) })
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if self.token != 0 {
            unsafe { metal_host_release(self.frames.table, self.frames.runtime, self.token) };
        }
    }
}

struct ArtMetalHost {
    owner: ThreadId,
    frames: Frames,
    target: Option<NativeSurface>,
    renderer: Option<(Extent2D, Renderer)>,
    failed: bool,
}

enum Renderer {
    Metal(SpriteRenderer<MetalBackend>),
    Angle(SpriteRenderer<GlBackend>),
}

impl Renderer {
    fn backend(&mut self) -> &mut dyn GpuBackend {
        match self {
            Self::Metal(renderer) => renderer.backend_mut(),
            Self::Angle(renderer) => renderer.backend_mut(),
        }
    }

    fn render<'a>(
        &mut self,
        runtime: u64,
        packet: &FramePacket,
        pixels: impl FnMut(usize) -> Result<&'a [u8], String>,
    ) -> Result<RenderRegion, String> {
        match self {
            Self::Metal(renderer) => renderer.render_packet(runtime, packet, pixels, CLEAR_COLOR),
            Self::Angle(renderer) => renderer.render_packet(runtime, packet, pixels, CLEAR_COLOR),
        }
    }

    fn access<T>(&mut self, operation: impl FnOnce(&mut Self) -> Result<T, i32>) -> Result<T, i32> {
        self.backend().begin_access();
        let result = catch_unwind(AssertUnwindSafe(|| operation(self)));
        self.backend().end_access();
        result.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    }
}

impl ArtMetalHost {
    fn set_surface(&mut self, surface: *mut c_void) -> Result<(), i32> {
        if surface.is_null() {
            return self.set_target(None);
        }
        let mut width = 0;
        let mut height = 0;
        if let Err(error) =
            status(unsafe { metal_host_surface_extent(surface, &mut width, &mut height) })
        {
            self.set_target(None)?;
            return Err(error);
        }
        self.set_target(Some(NativeSurface {
            kind: NativeSurfaceKind::AppleIoSurface,
            handle: surface,
            extent: Extent2D::new(width, height),
        }))
    }

    fn set_target(&mut self, target: Option<NativeSurface>) -> Result<(), i32> {
        self.target = None;
        if let Some((_, renderer)) = &mut self.renderer {
            renderer.access(|renderer| {
                let backend = renderer.backend();
                if let Some(target) = target {
                    if backend.set_native_surface(target).is_err() {
                        backend.clear_native_surface();
                        return Err(STATE);
                    }
                } else {
                    backend.clear_native_surface();
                }
                Ok(())
            })?;
        }
        self.target = target;
        Ok(())
    }

    fn renderer(&mut self, extent: Extent2D) -> Result<&mut Renderer, i32> {
        if self.renderer.is_none() {
            let mut backend = MetalBackend::new(extent.width, extent.height).map_err(|_| STATE)?;
            if let Some(target) = self.target {
                backend.set_native_surface(target).map_err(|_| STATE)?;
            }
            self.renderer = Some((extent, Renderer::Metal(SpriteRenderer::new(backend))));
        }
        let (stage, renderer) = self.renderer.as_mut().ok_or(STATE)?;
        if *stage != extent {
            renderer.access(|renderer| renderer.backend().resize(extent).map_err(|_| STATE))?;
            *stage = extent;
        }
        Ok(renderer)
    }

    fn draw(&mut self, lease: Lease, capture_extent: Option<Extent2D>) -> Result<u64, i32> {
        let packet = lease.decode()?;
        let stage = Extent2D::new(packet.stage_size[0], packet.stage_size[1]);
        if capture_extent.is_some_and(|extent| extent != stage) {
            return Err(ARGUMENT);
        }
        let renderer = self.renderer(stage)?;
        renderer.access(|renderer| {
            let backend = renderer.backend();
            if capture_extent.is_some()
                && backend
                    .render_dimensions()
                    .is_some_and(|dims| dims.render_size != stage)
            {
                backend.resize(stage).map_err(|_| STATE)?;
            }
            backend.begin_frame(FrameTarget::Main).map_err(|_| STATE)?;
            let mut pixel_status = OK;
            let drawn = renderer.render(lease.frames.runtime, &packet, |index| {
                lease.pixels(index).map_err(|code| {
                    pixel_status = code;
                    format!("Core texture borrow returned {code}")
                })
            });
            renderer.backend().end_frame();
            status(pixel_status)?;
            drawn.map_err(|_| STATE)?;
            Ok(())
        })?;
        let id = lease.id;
        lease.release()?;
        Ok(id)
    }

    fn capture(&mut self, frame: Lease, extent: Extent2D, rgba: &mut [u8]) -> Result<(), i32> {
        // A larger surface can enlarge Metal's main target. Capture at the logical stage size.
        let target = self
            .target
            .filter(|_| !matches!(self.renderer, Some((_, Renderer::Angle(_)))));
        if target.is_some() {
            self.set_target(None)?;
        }
        let captured = (|| {
            self.draw(frame, Some(extent))?;
            self.renderer.as_mut().ok_or(STATE)?.1.access(|renderer| {
                let written = renderer
                    .backend()
                    .readback(FrameTarget::Main, extent, rgba)
                    .map_err(|_| STATE)?;
                if written != rgba.len() {
                    return Err(STATE);
                }
                Ok(())
            })?;
            Ok(())
        })();
        if target.is_some() {
            let restored = self.set_target(target);
            captured.and(restored)
        } else {
            captured
        }
    }
}

fn status(code: i32) -> Result<(), i32> {
    if code == OK { Ok(()) } else { Err(code) }
}

fn boundary(f: impl FnOnce() -> Result<(), i32>) -> i32 {
    catch_unwind(AssertUnwindSafe(f))
        .unwrap_or(Err(PANIC))
        .err()
        .unwrap_or(OK)
}

unsafe fn with_host(
    host: *mut ArtMetalHost,
    f: impl FnOnce(&mut ArtMetalHost) -> Result<(), i32>,
) -> i32 {
    boundary(|| {
        if host.is_null() {
            return Err(ARGUMENT);
        }
        if unsafe { (*host).owner } != thread::current().id() {
            return Err(THREAD);
        }
        let host = unsafe { &mut *host };
        if host.failed {
            return Err(STATE);
        }
        catch_unwind(AssertUnwindSafe(|| f(host))).unwrap_or_else(|_| {
            host.failed = true;
            Err(PANIC)
        })
    })
}

#[unsafe(no_mangle)]
unsafe extern "C" fn art3m1s_metal_host_v1_create(
    table: *const c_void,
    runtime: u64,
    out: *mut *mut ArtMetalHost,
) -> i32 {
    unsafe { create_host(table, runtime, out, || Ok(None)) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn art3m1s_metal_host_v1_create_angle(
    table: *const c_void,
    runtime: u64,
    directory: *const c_char,
    out: *mut *mut ArtMetalHost,
) -> i32 {
    unsafe {
        create_host(table, runtime, out, || {
            if directory.is_null() {
                return Err(ARGUMENT);
            }
            let directory = CStr::from_ptr(directory).to_str().map_err(|_| ARGUMENT)?;
            let directory = Path::new(directory);
            if !directory.is_absolute() {
                return Err(ARGUMENT);
            }
            let backend = GlBackend::new_host_angle(directory, 1, 1).map_err(|error| {
                eprintln!("Artemis ANGLE/Metal initialization failed: {error}");
                UNSUPPORTED
            })?;
            Ok(Some((
                Extent2D::new(1, 1),
                Renderer::Angle(SpriteRenderer::new(backend)),
            )))
        })
    }
}

unsafe fn create_host(
    table: *const c_void,
    runtime: u64,
    out: *mut *mut ArtMetalHost,
    renderer: impl FnOnce() -> Result<Option<(Extent2D, Renderer)>, i32>,
) -> i32 {
    boundary(|| {
        if out.is_null() {
            return Err(ARGUMENT);
        }
        unsafe { out.write(ptr::null_mut()) };
        if table.is_null() || runtime == 0 {
            return Err(ARGUMENT);
        }
        status(unsafe { metal_host_check_frames(table) })?;
        let host = ArtMetalHost {
            owner: thread::current().id(),
            frames: Frames { table, runtime },
            target: None,
            renderer: renderer()?,
            failed: false,
        };
        unsafe { out.write(Box::into_raw(Box::new(host))) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
unsafe extern "C" fn art3m1s_metal_host_v1_destroy(host: *mut ArtMetalHost) -> i32 {
    boundary(|| {
        if host.is_null() {
            return Err(ARGUMENT);
        }
        if unsafe { (*host).owner } != thread::current().id() {
            return Err(THREAD);
        }
        drop(unsafe { Box::from_raw(host) });
        Ok(())
    })
}

#[unsafe(no_mangle)]
unsafe extern "C" fn art3m1s_metal_host_v1_set_surface(
    host: *mut ArtMetalHost,
    surface: *mut c_void,
) -> i32 {
    unsafe { with_host(host, |host| host.set_surface(surface)) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn art3m1s_metal_host_v1_present(
    host: *mut ArtMetalHost,
    after: u64,
    out_id: *mut u64,
) -> i32 {
    if out_id.is_null() {
        return ARGUMENT;
    }
    unsafe { out_id.write(0) };
    unsafe {
        with_host(host, |host| {
            if host.target.is_none() {
                return Err(STATE);
            }
            let frame = host.frames.acquire(after)?;
            let id = host.draw(frame, None)?;
            host.renderer
                .as_mut()
                .ok_or(STATE)?
                .1
                .access(|renderer| renderer.backend().present(None).map_err(|_| STATE))?;
            out_id.write(id);
            Ok(())
        })
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn art3m1s_metal_host_v1_capture(
    host: *mut ArtMetalHost,
    frame_id: u64,
    width: u32,
    height: u32,
    output: *mut u8,
    capacity: u64,
) -> i32 {
    unsafe {
        with_host(host, |host| {
            let extent = Extent2D::new(width, height);
            let size = extent
                .rgba8_len()
                .filter(|&n| n > 0 && n <= isize::MAX as usize)
                .ok_or(ARGUMENT)?;
            if frame_id == 0 || output.is_null() {
                return Err(ARGUMENT);
            }
            if capacity < size as u64 {
                return Err(BUFFER);
            }
            let frame = host.frames.acquire(frame_id - 1)?;
            if frame.id != frame_id {
                return Err(STALE);
            }
            let rgba = std::slice::from_raw_parts_mut(output, size);
            host.capture(frame, extent, rgba)
        })
    }
}
