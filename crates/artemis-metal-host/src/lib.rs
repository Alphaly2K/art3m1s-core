use art3m1s_render::artemis::{FramePacket, SpriteRenderer};
use art3m1s_render::backend::{
    FrameTarget, GpuBackend, RenderRegion, gl::GlBackend, metal::MetalBackend,
};
use art3m1s_render::post_process::{
    PostProcessPass, PostProcessPipeline, UpscaleConfig, UpscaleMode,
};
use art3m1s_render::{Extent2D, NativeSurface, NativeSurfaceKind};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::ptr;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::thread::{self, ThreadId};

const OK: i32 = 0;
const ARGUMENT: i32 = -1;
const HANDLE: i32 = -2;
const THREAD: i32 = -3;
const BUFFER: i32 = -5;
const STATE: i32 = -6;
const LIMIT: i32 = -8;
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

#[repr(C)]
struct ArtMetalHost {
    _opaque: [u8; 0],
}

// Opaque IDs are never dereferenced or reused. Only the creating thread holds the GPU state.
static NEXT_HOST: AtomicUsize = AtomicUsize::new(1);
static OWNERS: LazyLock<Mutex<HashMap<usize, ThreadId>>> = LazyLock::new(Default::default);

thread_local! {
    static HOSTS: RefCell<HashMap<usize, Rc<RefCell<Host>>>> = RefCell::new(HashMap::new());
}

struct Host {
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

impl Host {
    fn register(self) -> Result<*mut ArtMetalHost, i32> {
        let id = NEXT_HOST
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| LIMIT)?;
        HOSTS.with(|hosts| hosts.borrow_mut().insert(id, Rc::new(RefCell::new(self))));
        OWNERS.lock().unwrap().insert(id, thread::current().id());
        Ok(ptr::without_provenance_mut(id))
    }

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

fn owned_id(host: *mut ArtMetalHost) -> Result<usize, i32> {
    let id = host.addr();
    if id == 0 {
        return Err(ARGUMENT);
    }
    match OWNERS.lock().unwrap().get(&id) {
        Some(owner) if *owner == thread::current().id() => Ok(id),
        Some(_) => Err(THREAD),
        None => Err(HANDLE),
    }
}

fn with_host(host: *mut ArtMetalHost, f: impl FnOnce(&mut Host) -> Result<(), i32>) -> i32 {
    boundary(|| {
        let id = owned_id(host)?;
        let host = HOSTS
            .with(|hosts| hosts.borrow().get(&id).cloned())
            .ok_or(HANDLE)?;
        let mut host = host.try_borrow_mut().map_err(|_| STATE)?;
        if host.failed {
            return Err(STATE);
        }
        catch_unwind(AssertUnwindSafe(|| f(&mut host))).unwrap_or_else(|_| {
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

#[unsafe(no_mangle)]
unsafe extern "C" fn art3m1s_metal_host_v1_create_dxvk(
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
            let directory = Path::new(CStr::from_ptr(directory).to_str().map_err(|_| ARGUMENT)?);
            if !directory.is_absolute() {
                return Err(ARGUMENT);
            }
            let mut backend = MetalBackend::new(1, 1).map_err(|_| STATE)?;
            backend.enable_dxvk(directory).map_err(|error| {
                eprintln!("Artemis DXVK initialization failed: {error}");
                UNSUPPORTED
            })?;
            Ok(Some((
                Extent2D::new(1, 1),
                Renderer::Metal(SpriteRenderer::new(backend)),
            )))
        })
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn art3m1s_metal_host_v1_create_mixed(
    table: *const c_void,
    runtime: u64,
    dxvk_directory: *const c_char,
    angle_directory: *const c_char,
    out: *mut *mut ArtMetalHost,
) -> i32 {
    unsafe {
        create_host(table, runtime, out, || {
            let directory = |value: *const c_char| {
                if value.is_null() {
                    return Err(ARGUMENT);
                }
                let path = Path::new(CStr::from_ptr(value).to_str().map_err(|_| ARGUMENT)?);
                path.is_absolute().then_some(path).ok_or(ARGUMENT)
            };
            let dxvk = (!dxvk_directory.is_null())
                .then(|| directory(dxvk_directory))
                .transpose()?;
            let angle = directory(angle_directory)?;
            let mut backend = MetalBackend::new(1, 1).map_err(|_| STATE)?;
            let mut setup = || -> Result<(), String> {
                if let Some(dxvk) = dxvk {
                    backend.enable_dxvk(dxvk)?;
                }
                backend.enable_angle_effects(angle)
            };
            setup().map_err(|error| {
                eprintln!("Artemis mixed renderer initialization failed: {error}");
                UNSUPPORTED
            })?;
            Ok(Some((
                Extent2D::new(1, 1),
                Renderer::Metal(SpriteRenderer::new(backend)),
            )))
        })
    }
}

#[repr(C)]
struct ArtBytes {
    data: *const u8,
    size: u64,
}

impl ArtBytes {
    unsafe fn as_slice(&self) -> Result<&[u8], i32> {
        if self.data.is_null() || self.size == 0 || self.size > isize::MAX as u64 {
            return Err(ARGUMENT);
        }
        Ok(unsafe { std::slice::from_raw_parts(self.data, self.size as usize) })
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn art3m1s_metal_host_v1_prepare_ade2(
    host: *mut ArtMetalHost,
    source: ArtBytes,
    artifact: ArtBytes,
) -> i32 {
    unsafe {
        with_host(host, |host| {
            let (source, artifact) = (source.as_slice()?, artifact.as_slice()?);
            let Some((_, Renderer::Metal(renderer))) = &mut host.renderer else {
                return Err(UNSUPPORTED);
            };
            renderer
                .backend_mut()
                .prepare_ade2(source, artifact)
                .map_err(|error| {
                    eprintln!("Artemis ADE2 preparation failed: {error}");
                    ARGUMENT
                })?;
            renderer.retry_failed_shader_bindings();
            Ok(())
        })
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn art3m1s_metal_host_v1_set_glsl_cache(
    host: *mut ArtMetalHost,
    directory: *const c_char,
) -> i32 {
    unsafe {
        with_host(host, |host| {
            if directory.is_null() {
                return Err(ARGUMENT);
            }
            let directory = Path::new(CStr::from_ptr(directory).to_str().map_err(|_| ARGUMENT)?);
            if !directory.is_absolute() {
                return Err(ARGUMENT);
            }
            let (_, renderer) = host.renderer.as_mut().ok_or(UNSUPPORTED)?;
            renderer.access(|renderer| {
                match renderer {
                    Renderer::Angle(renderer) => renderer.backend_mut().set_glsl_cache(directory),
                    Renderer::Metal(renderer) => renderer.backend_mut().set_glsl_cache(directory),
                }
                .then_some(())
                .ok_or(UNSUPPORTED)
            })
        })
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn art3m1s_metal_host_v1_prepare_glsl(
    host: *mut ArtMetalHost,
    source: ArtBytes,
) -> i32 {
    unsafe {
        with_host(host, |host| {
            let source = source.as_slice()?;
            if source.len() > 256 * 1024 {
                return Err(ARGUMENT);
            }
            let (_, renderer) = host.renderer.as_mut().ok_or(UNSUPPORTED)?;
            renderer.access(|renderer| {
                let result = match renderer {
                    Renderer::Angle(renderer) => renderer.backend_mut().prepare_glsl(source),
                    Renderer::Metal(renderer) => renderer
                        .backend_mut()
                        .prepare_glsl(source)
                        .ok_or(UNSUPPORTED)?,
                };
                result.map_err(|error| {
                    eprintln!("Artemis GLSL preparation failed: {error}");
                    ARGUMENT
                })
            })
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
        let host = Host {
            frames: Frames { table, runtime },
            target: None,
            renderer: renderer()?,
            failed: false,
        };
        unsafe { out.write(host.register()?) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
unsafe extern "C" fn art3m1s_metal_host_v1_destroy(host: *mut ArtMetalHost) -> i32 {
    boundary(|| {
        let id = owned_id(host)?;
        let host = HOSTS.with(|hosts| {
            let mut hosts = hosts.borrow_mut();
            let host = hosts.get(&id).ok_or(HANDLE)?;
            if Rc::strong_count(host) != 1 {
                return Err(STATE);
            }
            Ok(hosts.remove(&id).unwrap())
        })?;
        OWNERS.lock().unwrap().remove(&id);
        drop(host);
        Ok(())
    })
}

#[unsafe(no_mangle)]
unsafe extern "C" fn art3m1s_metal_host_v1_set_surface(
    host: *mut ArtMetalHost,
    surface: *mut c_void,
) -> i32 {
    with_host(host, |host| host.set_surface(surface))
}

#[unsafe(no_mangle)]
unsafe extern "C" fn art3m1s_metal_host_v1_supports_spatial_upscaling(
    host: *mut ArtMetalHost,
    out_supported: *mut u8,
) -> i32 {
    if out_supported.is_null() {
        return ARGUMENT;
    }
    unsafe { out_supported.write(0) };
    unsafe {
        with_host(host, |host| {
            if host.renderer.is_none() {
                host.renderer(Extent2D::new(1, 1))?;
            }
            if let Some((_, Renderer::Metal(renderer))) = &mut host.renderer {
                let supported = renderer
                    .backend_mut()
                    .backend_info()
                    .capabilities
                    .spatial_upscaling;
                out_supported.write(u8::from(supported));
            }
            Ok(())
        })
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn art3m1s_metal_host_v1_set_spatial_upscale(
    host: *mut ArtMetalHost,
    render_scale: f32,
) -> i32 {
    with_host(host, |host| {
        if render_scale != 0.0 && !(0.1..=1.0).contains(&render_scale) {
            return Err(ARGUMENT);
        }
        if host.renderer.is_none() {
            if render_scale == 0.0 {
                return Ok(());
            }
            host.renderer(Extent2D::new(1, 1))?;
        }
        let Some((_, Renderer::Metal(renderer))) = &mut host.renderer else {
            return if render_scale == 0.0 {
                Ok(())
            } else {
                Err(UNSUPPORTED)
            };
        };
        let backend = renderer.backend_mut();
        let pipeline = if render_scale == 0.0 {
            PostProcessPipeline::default()
        } else {
            if !backend.backend_info().capabilities.spatial_upscaling {
                return Err(UNSUPPORTED);
            }
            PostProcessPipeline {
                render_scale,
                passes: vec![PostProcessPass::Upscale(UpscaleConfig {
                    mode: UpscaleMode::Spatial,
                    ..UpscaleConfig::default()
                })],
                ..PostProcessPipeline::default()
            }
        };
        backend.configure_post_process(pipeline).map_err(|_| STATE)
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    #[test]
    fn rejects_reentry_foreign_calls_and_destroyed_handles() {
        let handle = Host {
            frames: Frames {
                table: ptr::null(),
                runtime: 0,
            },
            target: None,
            renderer: None,
            failed: false,
        }
        .register()
        .unwrap();
        let detach = |handle| unsafe { art3m1s_metal_host_v1_set_surface(handle, ptr::null_mut()) };
        let destroy = |handle| unsafe { art3m1s_metal_host_v1_destroy(handle) };
        assert_eq!(
            with_host(handle, |_| {
                assert_eq!(detach(handle), STATE);
                assert_eq!(destroy(handle), STATE);
                Ok(())
            }),
            OK
        );

        let ready = Barrier::new(2);
        thread::scope(|scope| {
            let id = handle.addr();
            let ready = &ready;
            scope.spawn(move || {
                let handle = ptr::without_provenance_mut(id);
                let first = (detach(handle), destroy(handle));
                ready.wait();
                assert_eq!(first, (THREAD, THREAD));
                assert!(matches!(detach(handle), THREAD | HANDLE));
                assert!(matches!(destroy(handle), THREAD | HANDLE));
            });
            ready.wait();
            assert_eq!(destroy(handle), OK);
        });
        assert_eq!(detach(handle), HANDLE);
        assert_eq!(destroy(handle), HANDLE);
    }
}
