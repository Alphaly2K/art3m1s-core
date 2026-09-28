//! Versioned KRKR engine ABI exposed by the Art3m1s core library.
//!
//! The native KRKR shim owns the C++ runtime and exports a private
//! `art3m1s_krkr_native_get_api_v1` table. This module validates that table
//! and exposes the public `art3m1s_krkr_get_api_v1` facade. Runtime, frame,
//! and audio stream ownership use opaque integer handles or raw pointer and
//! length payloads.
//!
//! Public runtime handles are generational handles resolved through
//! [`crate::ffi::handles::LockedHandleTable`]: stale, foreign, or
//! double-destroyed handles fail with `ART3M1S_KRKR_STATUS_INVALID_HANDLE`
//! instead of dereferencing raw pointer bits, and facade calls serialize on
//! the table lock rather than aliasing runtime state across threads.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::{Arc, Mutex, OnceLock};

use art3m1s_krkr::abi::KrkrAbiError;
pub use art3m1s_krkr::abi::{Art3M1sKrkrApiV1, Art3m1sKrkrDiagnosticsApiV1};
use art3m1s_krkr::native;
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
use art3m1s_krkr::render_host::set_native_render_host;

use crate::ffi::handles::LockedHandleTable;
use crate::ffi::krkr_renderer::KrkrRenderer;

static NATIVE_API: OnceLock<Result<&'static Art3M1sKrkrApiV1, KrkrAbiError>> = OnceLock::new();

struct ApiRuntime {
    api: &'static Art3M1sKrkrApiV1,
    handle: u64,
    renderer: Arc<Mutex<KrkrRenderer>>,
    pending_frame_pixels: Vec<u8>,
    pending_frame_id: Option<u64>,
    last_frame_generation: u64,
    pending_audio_payload: Vec<u8>,
}

// `LockedHandleTable` serializes API access, and `renderer` has its own mutex
// for same-thread callbacks made by the native KRKR runtime.
unsafe impl Send for ApiRuntime {}

static RUNTIMES: LockedHandleTable<ApiRuntime> = LockedHandleTable::new();

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

/// Optional pull-only diagnostics table. Kept separate from the runtime v1
/// table so older hosts retain its exact layout and size.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_krkr_get_diagnostics_api_v1(
    out_size: *mut usize,
) -> *const Art3m1sKrkrDiagnosticsApiV1 {
    if !out_size.is_null() {
        unsafe { *out_size = std::mem::size_of::<Art3m1sKrkrDiagnosticsApiV1>() };
    }
    std::ptr::addr_of!(DIAGNOSTICS_API_V1)
}

static DIAGNOSTICS_API_V1: Art3m1sKrkrDiagnosticsApiV1 = Art3m1sKrkrDiagnosticsApiV1 {
    struct_size: std::mem::size_of::<Art3m1sKrkrDiagnosticsApiV1>() as u32,
    abi_version: art3m1s_krkr::abi::ART3M1S_KRKR_DIAGNOSTICS_ABI_VERSION,
    magic: art3m1s_krkr::abi::ART3M1S_KRKR_DIAGNOSTICS_ABI_MAGIC,
    log_next_bytes: Some(log_next_bytes),
    poll_log: Some(poll_log),
    runtime_set_debug: Some(runtime_set_debug),
};

unsafe extern "C" fn log_next_bytes() -> usize {
    catch_unwind(AssertUnwindSafe(native::log_next_bytes)).unwrap_or(0)
}

unsafe extern "C" fn poll_log(output: *mut u8, capacity: usize) -> usize {
    catch_unwind(AssertUnwindSafe(|| {
        if output.is_null() || capacity == 0 || capacity > 1024 * 1024 {
            return 0;
        }
        native::poll_log(unsafe { std::slice::from_raw_parts_mut(output, capacity) })
    }))
    .unwrap_or(0)
}

unsafe extern "C" fn runtime_set_debug(runtime: u64, enabled: i32) -> i32 {
    guard_status(|| {
        RUNTIMES.with(runtime, STATUS_INVALID_HANDLE, |_| {
            native::set_debug(enabled != 0)
        })
    })
}

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

        let config_ref = unsafe { &*config };
        if config_ref.struct_size != std::mem::size_of::<CoreRuntimeConfigV1>() as u32 {
            return STATUS_INVALID_ARGUMENT;
        }
        let backend_value = (config_ref.flags & ART3M1S_KRKR_CONFIG_BACKEND_MASK) as i32;
        let selection = match crate::backend::BackendSelection::try_from_legacy_int(backend_value) {
            Ok(selection) => selection,
            Err(_) => return ART3M1S_KRKR_STATUS_UNSUPPORTED,
        };
        let backend =
            match crate::backend::create_backend(selection, config_ref.width, config_ref.height) {
                Ok(backend) => backend,
                Err(_) => return STATUS_ENGINE,
            };
        let renderer = Arc::new(Mutex::new(KrkrRenderer::new(
            backend,
            config_ref.width,
            config_ref.height,
        )));
        let render_host = KrkrRenderer::host_v1(&renderer);
        if let Err(status) = set_native_render_host(Some(&render_host)) {
            return status;
        }

        let mut native_runtime = 0u64;
        let status = unsafe {
            (api.runtime_create.expect("runtime_create is required"))(
                game_root_utf8,
                save_root_utf8,
                config,
                &mut native_runtime,
            )
        };
        let _ = set_native_render_host(None);
        if status != STATUS_OK {
            return status;
        }
        if native_runtime == 0 {
            return STATUS_ENGINE;
        }

        let handle = RUNTIMES.insert(ApiRuntime {
            api,
            handle: native_runtime,
            renderer,
            pending_frame_pixels: Vec::new(),
            pending_frame_id: None,
            last_frame_generation: 0,
            pending_audio_payload: Vec::new(),
        });
        if handle == 0 {
            unsafe {
                (api.runtime_destroy.expect("runtime_destroy is required"))(native_runtime);
            }
            return STATUS_ENGINE;
        }
        unsafe { *out_runtime = handle };
        STATUS_OK
    })
}

unsafe extern "C" fn runtime_destroy(runtime: u64) {
    if runtime == 0 {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let Some(runtime) = RUNTIMES.take(runtime) else {
            return;
        };
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
        RUNTIMES.with(runtime, 0, |runtime| unsafe {
            (runtime
                .api
                .runtime_stage_width
                .expect("runtime_stage_width is required"))(runtime.handle)
        })
    })
}

unsafe extern "C" fn runtime_stage_height(runtime: u64) -> u32 {
    guard_u32(|| {
        RUNTIMES.with(runtime, 0, |runtime| unsafe {
            (runtime
                .api
                .runtime_stage_height
                .expect("runtime_stage_height is required"))(runtime.handle)
        })
    })
}

unsafe extern "C" fn runtime_pixel_buffer_size(runtime: u64) -> u32 {
    guard_u32(|| {
        RUNTIMES.with(runtime, 0, |runtime| unsafe {
            (runtime
                .api
                .runtime_pixel_buffer_size
                .expect("runtime_pixel_buffer_size is required"))(runtime.handle)
        })
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
        RUNTIMES.with(runtime, STATUS_INVALID_HANDLE, |runtime| unsafe {
            (runtime
                .api
                .runtime_push_input
                .expect("runtime_push_input is required"))(
                runtime.handle, events, event_count
            )
        })
    })
}

unsafe extern "C" fn runtime_tick(runtime: u64) -> i32 {
    guard_status(|| {
        RUNTIMES.with(runtime, STATUS_INVALID_HANDLE, |runtime| unsafe {
            (runtime.api.runtime_tick.expect("runtime_tick is required"))(runtime.handle)
        })
    })
}

unsafe extern "C" fn runtime_acquire_frame(runtime: u64, out_frame: *mut CoreFrameV1) -> i32 {
    guard_status(|| {
        if out_frame.is_null()
            || unsafe { (*out_frame).struct_size != std::mem::size_of::<CoreFrameV1>() as u32 }
        {
            return STATUS_INVALID_ARGUMENT;
        }
        RUNTIMES.with_mut(runtime, STATUS_INVALID_HANDLE, |runtime| {
            let mut renderer = match runtime.renderer.lock() {
                Ok(renderer) => renderer,
                Err(_) => return STATUS_ENGINE,
            };
            let generation = renderer.generation();
            if renderer.frame_in_progress()
                || generation == 0
                || generation == runtime.last_frame_generation
            {
                return ART3M1S_KRKR_STATUS_NO_FRAME;
            }
            runtime.pending_frame_pixels = match renderer.readback_rgba() {
                Ok(pixels) => pixels,
                Err(error) => {
                    eprintln!("[KRKR] frame readback failed: {error}");
                    return STATUS_ENGINE;
                }
            };
            let extent = renderer.extent();
            drop(renderer);
            runtime.last_frame_generation = generation;
            runtime.pending_frame_id = Some(generation);
            let frame = CoreFrameV1 {
                struct_size: std::mem::size_of::<CoreFrameV1>() as u32,
                format: ART3M1S_KRKR_FRAME_FORMAT_RGBA8,
                width: extent.width,
                height: extent.height,
                stride: extent.width.saturating_mul(4),
                flags: 0,
                frame_id: generation,
                generation,
                pixels: runtime.pending_frame_pixels.as_ptr(),
                pixels_len: runtime.pending_frame_pixels.len(),
                reserved: [0; 2],
            };
            unsafe { *out_frame = frame };
            STATUS_OK
        })
    })
}

unsafe extern "C" fn runtime_release_frame(runtime: u64, frame_id: u64) -> i32 {
    guard_status(|| {
        RUNTIMES.with_mut(runtime, STATUS_INVALID_HANDLE, |runtime| {
            if runtime.pending_frame_id != Some(frame_id) {
                return STATUS_INVALID_ARGUMENT;
            }
            runtime.pending_frame_id = None;
            runtime.pending_frame_pixels.clear();
            STATUS_OK
        })
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
        RUNTIMES.with_mut(runtime, STATUS_INVALID_HANDLE, |runtime| {
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
        RUNTIMES.with(runtime, STATUS_INVALID_HANDLE, |runtime| unsafe {
            (runtime
                .api
                .runtime_submit_audio_consumed
                .expect("runtime_submit_audio_consumed is required"))(
                runtime.handle, consumed
            )
        })
    })
}

unsafe extern "C" fn runtime_is_exit_requested(runtime: u64) -> i32 {
    guard_i32(|| {
        RUNTIMES.with(runtime, 0, |runtime| unsafe {
            (runtime
                .api
                .runtime_is_exit_requested
                .expect("runtime_is_exit_requested is required"))(runtime.handle)
        })
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
        RUNTIMES.with_mut(runtime, STATUS_INVALID_HANDLE, |runtime| {
            let mut renderer = match runtime.renderer.lock() {
                Ok(renderer) => renderer,
                Err(_) => return STATUS_ENGINE,
            };
            if handle.is_null() && kind == 0 && width == 0 && height == 0 {
                renderer.clear_native_surface();
                return STATUS_OK;
            }
            if handle.is_null() || width == 0 || height == 0 {
                return STATUS_INVALID_ARGUMENT;
            }
            renderer
                .set_native_surface(kind, handle, width, height)
                .map_or(ART3M1S_KRKR_STATUS_UNSUPPORTED, |_| STATUS_OK)
        })
    })
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
    fn invalid_handles_are_rejected_without_dereferencing() {
        for garbage in [u64::MAX, 1 << 32, 0xDEAD_BEEF_CAFE] {
            assert_eq!(unsafe { runtime_tick(garbage) }, STATUS_INVALID_HANDLE);
            assert_eq!(unsafe { runtime_stage_width(garbage) }, 0);
            assert_eq!(unsafe { runtime_stage_height(garbage) }, 0);
            assert_eq!(unsafe { runtime_pixel_buffer_size(garbage) }, 0);
            assert_eq!(unsafe { runtime_is_exit_requested(garbage) }, 0);
            assert_eq!(
                unsafe { runtime_push_input(garbage, ptr::null(), 0) },
                STATUS_INVALID_HANDLE
            );
            assert_eq!(
                unsafe { runtime_release_frame(garbage, 1) },
                STATUS_INVALID_HANDLE
            );
            // Double destroy and garbage destroy are safe no-ops.
            unsafe { runtime_destroy(garbage) };
            unsafe { runtime_destroy(garbage) };
        }
    }

    #[test]
    fn frame_acquire_validates_the_public_output_layout() {
        let mut frame = CoreFrameV1::default();
        frame.struct_size = 0;
        assert_eq!(
            unsafe { runtime_acquire_frame(u64::MAX, &mut frame) },
            STATUS_INVALID_ARGUMENT
        );
    }
}
