//! Siglus's independent, versioned host ABI. No VM or GPU pointers cross it.
//!
//! The upstream VM uses `Rc` internally. Handles are process-unique, but the
//! runtime table is thread-local: a handle called from another thread fails
//! closed instead of unsafely moving the VM or graphics backend.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use art3m1s_render::{Extent2D, FrameTarget, GpuBackend, NativeSurface};
use art3m1s_siglus::siglus_scene_vm::host::SiglusHostConfig;
use art3m1s_siglus::{SiglusAdapter, is_siglus_project};

pub const ABI_VERSION: u32 = 1;
pub const ABI_MAGIC: u32 = 0x5349_4731; // SIG1
pub const STATUS_OK: i32 = 0;
pub const STATUS_ARGUMENT: i32 = -1;
pub const STATUS_HANDLE: i32 = -2;
pub const STATUS_ENGINE: i32 = -3;

#[repr(C)]
pub struct SiglusApiV1 {
    pub struct_size: u32,
    pub abi_version: u32,
    pub magic: u32,
    pub probe_project: unsafe extern "C" fn(*const c_char) -> i32,
    pub runtime_create: unsafe extern "C" fn(*const c_char, i32, *mut u64) -> i32,
    pub runtime_destroy: unsafe extern "C" fn(u64),
    pub runtime_stage_width: unsafe extern "C" fn(u64) -> u32,
    pub runtime_stage_height: unsafe extern "C" fn(u64) -> u32,
    /// kind 0 detaches; kind 1-4 match the common native surface ABI.
    pub runtime_set_external_surface: unsafe extern "C" fn(u64, i32, *mut c_void, u32, u32) -> i32,
    /// mode: 0 advance, 1 read RGBA, 2 present to attached native surface.
    /// Output pixels are RGBA8 at the logical stage size; written is optional.
    pub runtime_tick: unsafe extern "C" fn(u64, u32, i32, *mut u8, usize, *mut usize) -> i32,
    /// kind: 0 move, 1 mouse button, 2 touch, 3 key, 4 wheel.
    /// x/y are stage coordinates, code is button/key/touch phase, value is
    /// pressed (0/1) or wheel delta. Touch phase follows down/move/up/cancel.
    pub runtime_input: unsafe extern "C" fn(u64, i32, i32, i32, i32, i32) -> i32,
    pub runtime_is_exit_requested: unsafe extern "C" fn(u64) -> i32,
    /// Returns UTF-8 byte length (excluding NUL); copies at most capacity.
    pub last_error: unsafe extern "C" fn(*mut u8, usize) -> usize,
}

struct Runtime {
    adapter: SiglusAdapter,
    gpu: Box<dyn GpuBackend>,
    width: u32,
    height: u32,
    exiting: bool,
    surface_attached: bool,
}

thread_local! {
    static RUNTIMES: RefCell<HashMap<u64, Runtime>> = RefCell::new(HashMap::new());
    static LAST_ERROR: RefCell<String> = RefCell::new(String::new());
}
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

fn fail(message: impl Into<String>) -> i32 {
    let message = message.into();
    LAST_ERROR.with(|error| *error.borrow_mut() = message);
    STATUS_ENGINE
}

fn guarded(f: impl FnOnce() -> i32) -> i32 {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(status) => status,
        Err(_) => fail("Siglus native API panicked"),
    }
}

fn with_runtime(handle: u64, f: impl FnOnce(&mut Runtime) -> i32) -> i32 {
    RUNTIMES.with(|table| match table.borrow_mut().get_mut(&handle) {
        Some(runtime) => f(runtime),
        None => STATUS_HANDLE,
    })
}

fn project_path(ptr: *const c_char) -> Result<PathBuf, i32> {
    if ptr.is_null() {
        return Err(STATUS_ARGUMENT);
    }
    let path = unsafe { CStr::from_ptr(ptr) };
    let path = path.to_str().map_err(|_| STATUS_ARGUMENT)?;
    if path.is_empty() {
        return Err(STATUS_ARGUMENT);
    }
    Ok(PathBuf::from(path))
}

unsafe extern "C" fn probe_project(ptr: *const c_char) -> i32 {
    guarded(|| match project_path(ptr) {
        Ok(path) => i32::from(is_siglus_project(&path)),
        Err(status) => status,
    })
}

unsafe extern "C" fn runtime_create(ptr: *const c_char, backend: i32, out: *mut u64) -> i32 {
    guarded(|| {
        if out.is_null() {
            return STATUS_ARGUMENT;
        }
        unsafe { *out = 0 };
        let path = match project_path(ptr) {
            Ok(path) => path,
            Err(status) => return status,
        };
        if !is_siglus_project(&path) {
            return fail("directory has no Siglus Scene.pck and Gameexe.dat/ini");
        }
        let adapter = match SiglusAdapter::open(SiglusHostConfig::new(path)) {
            Ok(adapter) => adapter,
            Err(error) => return fail(format!("Siglus VM initialization: {error:#}")),
        };
        let (width, height) = adapter.logical_size();
        if width == 0
            || height == 0
            || (width as usize)
                .checked_mul(height as usize)
                .and_then(|n| n.checked_mul(4))
                .is_none()
        {
            return fail("Siglus stage dimensions are invalid");
        }
        let selection = match crate::backend::BackendSelection::try_from_legacy_int(backend) {
            Ok(selection) => selection,
            Err(error) => return fail(error),
        };
        let gpu = match crate::backend::create_backend(selection, width, height) {
            Ok(gpu) => gpu,
            Err(error) => return fail(format!("Siglus GPU initialization: {error}")),
        };
        let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
        if handle == 0 || handle == u64::MAX {
            return fail("Siglus runtime handles exhausted");
        }
        RUNTIMES.with(|table| {
            table.borrow_mut().insert(
                handle,
                Runtime {
                    adapter,
                    gpu,
                    width,
                    height,
                    exiting: false,
                    surface_attached: false,
                },
            );
        });
        unsafe { *out = handle };
        STATUS_OK
    })
}

unsafe extern "C" fn runtime_destroy(handle: u64) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        RUNTIMES.with(|table| {
            if let Some(mut runtime) = table.borrow_mut().remove(&handle) {
                runtime.adapter.release_textures(runtime.gpu.as_mut());
                runtime.gpu.clear_native_surface();
            }
        });
    }));
}

unsafe extern "C" fn runtime_stage_width(handle: u64) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        RUNTIMES.with(|table| table.borrow().get(&handle).map_or(0, |rt| rt.width))
    }))
    .unwrap_or(0)
}

unsafe extern "C" fn runtime_stage_height(handle: u64) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        RUNTIMES.with(|table| table.borrow().get(&handle).map_or(0, |rt| rt.height))
    }))
    .unwrap_or(0)
}

unsafe extern "C" fn runtime_set_external_surface(
    handle: u64,
    kind: i32,
    ptr: *mut c_void,
    width: u32,
    height: u32,
) -> i32 {
    guarded(|| {
        with_runtime(handle, |rt| {
            if kind == 0 {
                rt.gpu.clear_native_surface();
                rt.surface_attached = false;
                return STATUS_OK;
            }
            let surface = match NativeSurface::from_legacy_parts(kind, ptr, width, height) {
                Ok(surface) => surface,
                Err(error) => return fail(error),
            };
            match rt.gpu.set_native_surface(surface) {
                Ok(()) => {
                    rt.surface_attached = true;
                    STATUS_OK
                }
                Err(error) => fail(error),
            }
        })
    })
}

unsafe extern "C" fn runtime_tick(
    handle: u64,
    dt_ms: u32,
    mode: i32,
    out: *mut u8,
    capacity: usize,
    written: *mut usize,
) -> i32 {
    guarded(|| {
        with_runtime(handle, |rt| {
            if mode == 1 && (out.is_null() || capacity < rt.width as usize * rt.height as usize * 4)
            {
                return STATUS_ARGUMENT;
            }
            if mode == 2 && !rt.surface_attached {
                return STATUS_ARGUMENT;
            }
            if !matches!(mode, 0..=2) {
                return STATUS_ARGUMENT;
            }
            let step = if mode == 2 {
                rt.adapter.step(dt_ms, rt.gpu.as_mut())
            } else {
                rt.adapter.step_without_present(dt_ms, rt.gpu.as_mut())
            };
            match step {
                Ok(exiting) => rt.exiting = exiting,
                Err(error) => return fail(format!("Siglus frame: {error:#}")),
            }
            if mode == 1 {
                rt.gpu.begin_access();
                let pixels = rt
                    .gpu
                    .readback_owned(FrameTarget::Main, Extent2D::new(rt.width, rt.height));
                rt.gpu.end_access();
                let pixels = match pixels {
                    Ok(pixels) => pixels,
                    Err(error) => return fail(error),
                };
                if pixels.len() > capacity {
                    return STATUS_ARGUMENT;
                }
                unsafe {
                    std::ptr::copy_nonoverlapping(pixels.as_ptr(), out, pixels.len());
                    if !written.is_null() {
                        *written = pixels.len();
                    }
                }
            } else if !written.is_null() {
                unsafe { *written = 0 };
            }
            STATUS_OK
        })
    })
}

unsafe extern "C" fn runtime_input(
    handle: u64,
    kind: i32,
    code: i32,
    x: i32,
    y: i32,
    value: i32,
) -> i32 {
    guarded(|| {
        with_runtime(handle, |rt| {
            match kind {
                0 => rt.adapter.pointer_move(x, y),
                1 => {
                    rt.adapter.pointer_move(x, y);
                    rt.adapter.pointer_button(code, value != 0);
                }
                2 => rt.adapter.touch(code, x, y),
                3 => rt.adapter.key(code, value != 0),
                4 => rt.adapter.wheel(value),
                _ => return STATUS_ARGUMENT,
            }
            STATUS_OK
        })
    })
}

unsafe extern "C" fn runtime_is_exit_requested(handle: u64) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        RUNTIMES.with(|table| {
            table
                .borrow()
                .get(&handle)
                .map_or(STATUS_HANDLE, |rt| i32::from(rt.exiting))
        })
    }))
    .unwrap_or(STATUS_ENGINE)
}

unsafe extern "C" fn last_error(out: *mut u8, capacity: usize) -> usize {
    catch_unwind(AssertUnwindSafe(|| {
        LAST_ERROR.with(|error| {
            let error = error.borrow();
            let bytes = error.as_bytes();
            if !out.is_null() && capacity > 0 {
                unsafe {
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len().min(capacity));
                }
            }
            bytes.len()
        })
    }))
    .unwrap_or(0)
}

static API: SiglusApiV1 = SiglusApiV1 {
    struct_size: std::mem::size_of::<SiglusApiV1>() as u32,
    abi_version: ABI_VERSION,
    magic: ABI_MAGIC,
    probe_project,
    runtime_create,
    runtime_destroy,
    runtime_stage_width,
    runtime_stage_height,
    runtime_set_external_surface,
    runtime_tick,
    runtime_input,
    runtime_is_exit_requested,
    last_error,
};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_siglus_get_api_v1(out_size: *mut usize) -> *const SiglusApiV1 {
    if !out_size.is_null() {
        unsafe { *out_size = std::mem::size_of::<SiglusApiV1>() };
    }
    &API
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn optional_real_game_host_abi_smoke() {
        let Some(path) = std::env::var_os("ART3M1S_SIGLUS_TEST_GAME") else {
            return;
        };
        let path = CString::new(path.to_string_lossy().as_bytes()).unwrap();
        let mut size = 0usize;
        let api = unsafe { &*art3m1s_siglus_get_api_v1(&mut size) };
        assert_eq!(size, std::mem::size_of::<SiglusApiV1>());
        assert_eq!(unsafe { (api.probe_project)(path.as_ptr()) }, 1);
        let mut handle = 0;
        let status = unsafe { (api.runtime_create)(path.as_ptr(), 0, &mut handle) };
        let mut error = vec![0; 1024];
        let length = unsafe { (api.last_error)(error.as_mut_ptr(), error.len()) };
        assert_eq!(
            status,
            STATUS_OK,
            "{}",
            String::from_utf8_lossy(&error[..length.min(error.len())])
        );
        assert_ne!(handle, 0);
        let width = unsafe { (api.runtime_stage_width)(handle) };
        let height = unsafe { (api.runtime_stage_height)(handle) };
        let mut pixels = vec![0; width as usize * height as usize * 4];
        for _ in 0..150 {
            let status = unsafe {
                (api.runtime_tick)(
                    handle,
                    16,
                    1,
                    pixels.as_mut_ptr(),
                    pixels.len(),
                    std::ptr::null_mut(),
                )
            };
            assert_eq!(status, STATUS_OK);
            if pixels.chunks_exact(4).any(|pixel| pixel[..3] != [0, 0, 0]) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        assert!(pixels.chunks_exact(4).any(|pixel| pixel[..3] != [0, 0, 0]));
        assert_eq!(
            unsafe { (api.runtime_input)(handle, 0, 0, 100, 100, 0) },
            STATUS_OK
        );
        unsafe { (api.runtime_destroy)(handle) };
        assert_eq!(
            unsafe { (api.runtime_is_exit_requested)(handle) },
            STATUS_HANDLE
        );
    }
}
