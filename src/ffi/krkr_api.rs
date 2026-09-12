//! Versioned KRKR engine ABI exposed by the Art3m1s core library.
//!
//! The native KRKR shim owns the C++ runtime and exports a private
//! `art3m1s_krkr_native_get_api_v1` table. This module validates that table
//! and exposes the public `art3m1s_krkr_get_api_v1` facade. Runtime, frame,
//! and audio stream ownership use opaque integer handles or raw pointer and
//! length payloads.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::OnceLock;

pub use art3m1s_krkr::abi::Art3M1sKrkrApiV1;
use art3m1s_krkr::abi::KrkrAbiError;
use art3m1s_krkr::native::load_api_v1;
pub use art3m1s_krkr::protocol::*;
use art3m1s_krkr::protocol::{
    ART3M1S_KRKR_STATUS_ENGINE as STATUS_ENGINE,
    ART3M1S_KRKR_STATUS_INVALID_ARGUMENT as STATUS_INVALID_ARGUMENT,
    ART3M1S_KRKR_STATUS_INVALID_HANDLE as STATUS_INVALID_HANDLE,
    ART3M1S_KRKR_STATUS_OK as STATUS_OK, Art3m1sKrkrAudioCommandV1 as CoreAudioCommandV1,
    Art3m1sKrkrAudioConsumedV1 as CoreAudioConsumedV1, Art3m1sKrkrFrameV1 as CoreFrameV1,
    Art3m1sKrkrInputEventV1 as CoreInputEventV1, Art3m1sKrkrProbeV1 as CoreProbeV1,
    Art3m1sKrkrRuntimeConfigV1 as CoreRuntimeConfigV1,
};

static NATIVE_API: OnceLock<Result<&'static Art3M1sKrkrApiV1, KrkrAbiError>> = OnceLock::new();

struct ApiRuntime {
    api: &'static Art3M1sKrkrApiV1,
    handle: u64,
    pending_frame_pixels: Vec<u8>,
    pending_frame_id: Option<u64>,
    pending_audio_payload: Vec<u8>,
}

/// Returns the process-lifetime KRKR API table.
///
/// `out_size` receives the exact struct size known to this build. Hosts must
/// reject a mismatch before reading any field.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_krkr_get_api_v1(out_size: *mut usize) -> *const Art3M1sKrkrApiV1 {
    if !out_size.is_null() {
        unsafe { *out_size = std::mem::size_of::<Art3M1sKrkrApiV1>() };
    }
    std::ptr::addr_of!(API_V1)
}

static API_V1: Art3M1sKrkrApiV1 = Art3M1sKrkrApiV1 {
    struct_size: std::mem::size_of::<Art3M1sKrkrApiV1>() as u32,
    abi_version: art3m1s_krkr::ART3M1S_KRKR_API_ABI_VERSION,
    magic: art3m1s_krkr::ART3M1S_KRKR_API_ABI_MAGIC,
    probe_project: Some(probe_project),
    runtime_create: Some(runtime_create),
    runtime_destroy: Some(runtime_destroy),
    runtime_stage_width: Some(runtime_stage_width),
    runtime_stage_height: Some(runtime_stage_height),
    runtime_pixel_buffer_size: Some(runtime_pixel_buffer_size),
    runtime_push_input: Some(runtime_push_input),
    runtime_tick: Some(runtime_tick),
    runtime_acquire_frame: Some(runtime_acquire_frame),
    runtime_release_frame: Some(runtime_release_frame),
    runtime_poll_audio_command: Some(runtime_poll_audio_command),
    runtime_submit_audio_consumed: Some(runtime_submit_audio_consumed),
    runtime_is_exit_requested: Some(runtime_is_exit_requested),
    runtime_set_external_surface: Some(runtime_set_external_surface),
};

fn native_api() -> Result<&'static Art3M1sKrkrApiV1, i32> {
    NATIVE_API
        .get_or_init(load_api_v1)
        .as_ref()
        .map(|api| *api)
        .map_err(|_| STATUS_ENGINE)
}

unsafe extern "C" fn probe_project(
    game_root_utf8: *const std::ffi::c_char,
    out_probe: *mut CoreProbeV1,
) -> i32 {
    guard_status(|| {
        let api = match native_api() {
            Ok(api) => api,
            Err(status) => return status,
        };
        if out_probe.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        unsafe {
            (api.probe_project.expect("probe_project is required"))(game_root_utf8, out_probe)
        }
    })
}

unsafe extern "C" fn runtime_create(
    game_root_utf8: *const std::ffi::c_char,
    save_root_utf8: *const std::ffi::c_char,
    config: *const CoreRuntimeConfigV1,
    out_runtime: *mut u64,
) -> i32 {
    guard_status(|| {
        let api = match native_api() {
            Ok(api) => api,
            Err(status) => return status,
        };
        if out_runtime.is_null() || config.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        unsafe { *out_runtime = 0 };

        let mut native_runtime = 0u64;
        let status = unsafe {
            (api.runtime_create.expect("runtime_create is required"))(
                game_root_utf8,
                save_root_utf8,
                config,
                &mut native_runtime,
            )
        };
        if status != STATUS_OK {
            return status;
        }
        if native_runtime == 0 {
            return STATUS_ENGINE;
        }

        let runtime = Box::new(ApiRuntime {
            api,
            handle: native_runtime,
            pending_frame_pixels: Vec::new(),
            pending_frame_id: None,
            pending_audio_payload: Vec::new(),
        });
        unsafe { *out_runtime = Box::into_raw(runtime) as u64 };
        STATUS_OK
    })
}

unsafe extern "C" fn runtime_destroy(runtime: u64) {
    if runtime == 0 {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let runtime = unsafe { Box::from_raw(runtime as *mut ApiRuntime) };
        unsafe {
            (runtime
                .api
                .runtime_destroy
                .expect("runtime_destroy is required"))(runtime.handle);
        }
    }));
}

unsafe extern "C" fn runtime_stage_width(runtime: u64) -> u32 {
    guard_u32(|| {
        let Ok(runtime) = (unsafe { runtime_ref(runtime) }) else {
            return 0;
        };
        unsafe {
            (runtime
                .api
                .runtime_stage_width
                .expect("runtime_stage_width is required"))(runtime.handle)
        }
    })
}

unsafe extern "C" fn runtime_stage_height(runtime: u64) -> u32 {
    guard_u32(|| {
        let Ok(runtime) = (unsafe { runtime_ref(runtime) }) else {
            return 0;
        };
        unsafe {
            (runtime
                .api
                .runtime_stage_height
                .expect("runtime_stage_height is required"))(runtime.handle)
        }
    })
}

unsafe extern "C" fn runtime_pixel_buffer_size(runtime: u64) -> u32 {
    guard_u32(|| {
        let Ok(runtime) = (unsafe { runtime_ref(runtime) }) else {
            return 0;
        };
        unsafe {
            (runtime
                .api
                .runtime_pixel_buffer_size
                .expect("runtime_pixel_buffer_size is required"))(runtime.handle)
        }
    })
}

unsafe extern "C" fn runtime_push_input(
    runtime: u64,
    events: *const CoreInputEventV1,
    event_count: usize,
) -> i32 {
    guard_status(|| {
        if events.is_null() && event_count != 0 {
            return STATUS_INVALID_ARGUMENT;
        }
        if event_count > 4096 {
            return STATUS_INVALID_ARGUMENT;
        }
        let Ok(runtime) = (unsafe { runtime_ref(runtime) }) else {
            return STATUS_INVALID_HANDLE;
        };
        unsafe {
            (runtime
                .api
                .runtime_push_input
                .expect("runtime_push_input is required"))(
                runtime.handle, events, event_count
            )
        }
    })
}

unsafe extern "C" fn runtime_tick(runtime: u64) -> i32 {
    guard_status(|| {
        let Ok(runtime) = (unsafe { runtime_ref(runtime) }) else {
            return STATUS_INVALID_HANDLE;
        };
        unsafe { (runtime.api.runtime_tick.expect("runtime_tick is required"))(runtime.handle) }
    })
}

unsafe extern "C" fn runtime_acquire_frame(runtime: u64, out_frame: *mut CoreFrameV1) -> i32 {
    guard_status(|| {
        if out_frame.is_null()
            || unsafe { (*out_frame).struct_size != std::mem::size_of::<CoreFrameV1>() as u32 }
        {
            return STATUS_INVALID_ARGUMENT;
        }
        let Ok(runtime) = (unsafe { runtime_mut(runtime) }) else {
            return STATUS_INVALID_HANDLE;
        };

        let mut native_frame = CoreFrameV1::default();
        let status = unsafe {
            (runtime
                .api
                .runtime_acquire_frame
                .expect("runtime_acquire_frame is required"))(
                runtime.handle, &mut native_frame
            )
        };
        if status != STATUS_OK {
            return status;
        }

        let copied = copy_native_frame(&native_frame, &mut runtime.pending_frame_pixels);
        unsafe {
            (runtime
                .api
                .runtime_release_frame
                .expect("runtime_release_frame is required"))(
                runtime.handle,
                native_frame.frame_id,
            );
        }
        if !copied {
            return STATUS_ENGINE;
        }

        runtime.pending_frame_id = Some(native_frame.frame_id);
        let frame = CoreFrameV1 {
            pixels: runtime.pending_frame_pixels.as_ptr(),
            pixels_len: runtime.pending_frame_pixels.len(),
            ..native_frame
        };
        unsafe { *out_frame = frame };
        STATUS_OK
    })
}

unsafe extern "C" fn runtime_release_frame(runtime: u64, frame_id: u64) -> i32 {
    guard_status(|| {
        let Ok(runtime) = (unsafe { runtime_mut(runtime) }) else {
            return STATUS_INVALID_HANDLE;
        };
        if runtime.pending_frame_id != Some(frame_id) {
            return STATUS_INVALID_ARGUMENT;
        }
        runtime.pending_frame_id = None;
        runtime.pending_frame_pixels.clear();
        STATUS_OK
    })
}

unsafe extern "C" fn runtime_poll_audio_command(
    runtime: u64,
    out_command: *mut CoreAudioCommandV1,
) -> i32 {
    guard_status(|| {
        if out_command.is_null()
            || unsafe {
                (*out_command).struct_size != std::mem::size_of::<CoreAudioCommandV1>() as u32
            }
        {
            return STATUS_INVALID_ARGUMENT;
        }
        let Ok(runtime) = (unsafe { runtime_mut(runtime) }) else {
            return STATUS_INVALID_HANDLE;
        };

        let mut command = CoreAudioCommandV1::default();
        let status = unsafe {
            (runtime
                .api
                .runtime_poll_audio_command
                .expect("runtime_poll_audio_command is required"))(
                runtime.handle, &mut command
            )
        };
        if status != STATUS_OK {
            return status;
        }
        if command.payload_size != 0 && command.payload.is_null() {
            return STATUS_ENGINE;
        }

        runtime.pending_audio_payload.clear();
        if command.payload_size != 0 {
            runtime.pending_audio_payload.extend_from_slice(unsafe {
                std::slice::from_raw_parts(command.payload, command.payload_size)
            });
        }
        command.payload = if runtime.pending_audio_payload.is_empty() {
            ptr::null()
        } else {
            runtime.pending_audio_payload.as_ptr()
        };
        command.payload_size = runtime.pending_audio_payload.len();
        command.struct_size = std::mem::size_of::<CoreAudioCommandV1>() as u32;
        unsafe { *out_command = command };
        STATUS_OK
    })
}

unsafe extern "C" fn runtime_submit_audio_consumed(
    runtime: u64,
    consumed: *const CoreAudioConsumedV1,
) -> i32 {
    guard_status(|| {
        if consumed.is_null()
            || unsafe {
                (*consumed).struct_size != std::mem::size_of::<CoreAudioConsumedV1>() as u32
            }
        {
            return STATUS_INVALID_ARGUMENT;
        }
        let Ok(runtime) = (unsafe { runtime_ref(runtime) }) else {
            return STATUS_INVALID_HANDLE;
        };
        unsafe {
            (runtime
                .api
                .runtime_submit_audio_consumed
                .expect("runtime_submit_audio_consumed is required"))(
                runtime.handle, consumed
            )
        }
    })
}

unsafe extern "C" fn runtime_is_exit_requested(runtime: u64) -> i32 {
    guard_i32(|| {
        let Ok(runtime) = (unsafe { runtime_ref(runtime) }) else {
            return 0;
        };
        unsafe {
            (runtime
                .api
                .runtime_is_exit_requested
                .expect("runtime_is_exit_requested is required"))(runtime.handle)
        }
    })
}

unsafe extern "C" fn runtime_set_external_surface(
    runtime: u64,
    kind: i32,
    handle: *mut c_void,
    width: u32,
    height: u32,
) -> i32 {
    guard_status(|| {
        let Ok(runtime) = (unsafe { runtime_ref(runtime) }) else {
            return STATUS_INVALID_HANDLE;
        };
        unsafe {
            (runtime
                .api
                .runtime_set_external_surface
                .expect("runtime_set_external_surface is required"))(
                runtime.handle,
                kind,
                handle,
                width,
                height,
            )
        }
    })
}

fn copy_native_frame(frame: &CoreFrameV1, destination: &mut Vec<u8>) -> bool {
    if frame.pixels.is_null() || frame.width == 0 || frame.height == 0 {
        return false;
    }
    let Ok(stride) = usize::try_from(frame.stride) else {
        return false;
    };
    let Some(row_bytes) = usize::try_from(frame.width)
        .ok()
        .and_then(|width| width.checked_mul(4))
    else {
        return false;
    };
    let Some(height) = usize::try_from(frame.height).ok() else {
        return false;
    };
    let Some(required) = stride.checked_mul(height) else {
        return false;
    };
    if stride < row_bytes || required == 0 || frame.pixels_len < required {
        return false;
    }

    destination.resize(required, 0);
    unsafe {
        ptr::copy_nonoverlapping(frame.pixels, destination.as_mut_ptr(), required);
    }
    true
}

unsafe fn runtime_ref(runtime: u64) -> Result<&'static ApiRuntime, i32> {
    if runtime == 0 {
        return Err(STATUS_INVALID_HANDLE);
    }
    Ok(unsafe { &*(runtime as *const ApiRuntime) })
}

unsafe fn runtime_mut(runtime: u64) -> Result<&'static mut ApiRuntime, i32> {
    if runtime == 0 {
        return Err(STATUS_INVALID_HANDLE);
    }
    Ok(unsafe { &mut *(runtime as *mut ApiRuntime) })
}

fn guard_status(callback: impl FnOnce() -> i32) -> i32 {
    catch_unwind(AssertUnwindSafe(callback)).unwrap_or(STATUS_ENGINE)
}

fn guard_i32(callback: impl FnOnce() -> i32) -> i32 {
    catch_unwind(AssertUnwindSafe(callback)).unwrap_or(0)
}

fn guard_u32(callback: impl FnOnce() -> u32) -> u32 {
    catch_unwind(AssertUnwindSafe(callback)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use art3m1s_krkr::validate_api_v1;

    #[test]
    fn public_table_is_versioned_and_complete() {
        let mut size = 0usize;
        let table = unsafe { art3m1s_krkr_get_api_v1(&mut size) };
        assert!(!table.is_null());
        assert_eq!(size, std::mem::size_of::<Art3M1sKrkrApiV1>());
        let table = unsafe { &*table };
        assert_eq!(table.struct_size as usize, size);
        assert_eq!(validate_api_v1(table), Ok(()));
    }

    #[test]
    fn copies_native_frame_into_core_owned_storage() {
        let pixels = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let frame = CoreFrameV1 {
            struct_size: std::mem::size_of::<CoreFrameV1>() as u32,
            width: 1,
            height: 2,
            stride: 4,
            pixels: pixels.as_ptr(),
            pixels_len: pixels.len(),
            ..Default::default()
        };
        let mut destination = Vec::new();
        assert!(copy_native_frame(&frame, &mut destination));
        assert_eq!(destination, pixels);
    }
}
