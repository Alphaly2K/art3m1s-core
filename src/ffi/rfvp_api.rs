//! Versioned RFVP engine ABI exposed by the Art3m1s core library.
//!
//! This table is separate from [`super::api::Art3m1sApiV1`]. RFVP keeps its
//! own runtime/resources/frame handles, but rendering and presentation stay
//! inside `art3m1s-rfvp` and the shared `art3m1s-render` backend.
//!
//! Runtime handles are generational handles resolved through
//! [`super::handles::LockedHandleTable`]: stale, foreign, or double-destroyed
//! handles fail with `ART3M1S_RFVP_STATUS_INVALID_HANDLE` instead of
//! dereferencing raw pointer bits, and facade calls serialize on the table
//! lock rather than aliasing runtime state across threads.
//!
//! Logs are available through the pull-based `log_next_bytes`/`poll_log`
//! pair. `runtime_set_log_callback` remains for migration only: hosts running
//! in environments where native-to-host callbacks are unsafe (for example
//! Dart `NativeCallable` trampolines on modified iOS devices) must not
//! register it and should poll the log queue instead.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::Mutex;

use art3m1s_rfvp::{
    RfvpAudioSampleFormat, RfvpEncodedAudioKind, RfvpHostAudioCommand, RfvpHostAudioCommandKind,
    RfvpHostEvent, RfvpHostRuntime, RfvpNls,
};

use art3m1s_rfvp::ffi_v2::input::convert_input_event;
pub use art3m1s_rfvp::ffi_v2::input::{
    ART3M1S_RFVP_INPUT_FOCUS, ART3M1S_RFVP_INPUT_KEY, ART3M1S_RFVP_INPUT_PHASE_DOWN,
    ART3M1S_RFVP_INPUT_PHASE_MOVE, ART3M1S_RFVP_INPUT_PHASE_REPEAT, ART3M1S_RFVP_INPUT_PHASE_UP,
    ART3M1S_RFVP_INPUT_POINTER_BUTTON, ART3M1S_RFVP_INPUT_POINTER_MOVE, ART3M1S_RFVP_INPUT_QUIT,
    ART3M1S_RFVP_INPUT_TEXT, ART3M1S_RFVP_INPUT_TOUCH, ART3M1S_RFVP_INPUT_WHEEL,
    ART3M1S_RFVP_POINTER_LEFT, ART3M1S_RFVP_POINTER_MIDDLE, ART3M1S_RFVP_POINTER_RIGHT,
    Art3m1sRfvpInputEventV1,
};

use crate::backend::BackendSelection;
use crate::ffi::handles::LockedHandleTable;

pub const ART3M1S_RFVP_API_ABI_VERSION: u32 = 1;
pub const ART3M1S_RFVP_API_ABI_MAGIC: u64 = 0x3156_4652_4d33_4152; // "RA3MRFV1"

pub const ART3M1S_RFVP_STATUS_OK: i32 = 0;
pub const ART3M1S_RFVP_STATUS_NO_FRAME: i32 = 1;
pub const ART3M1S_RFVP_STATUS_NO_COMMAND: i32 = 2;
pub const ART3M1S_RFVP_STATUS_INVALID_ARGUMENT: i32 = -1;
pub const ART3M1S_RFVP_STATUS_INVALID_HANDLE: i32 = -2;
pub const ART3M1S_RFVP_STATUS_ENGINE: i32 = -3;
pub const ART3M1S_RFVP_STATUS_UNSUPPORTED: i32 = -4;
pub const ART3M1S_RFVP_STATUS_OUT_OF_MEMORY: i32 = -5;

pub const ART3M1S_RFVP_NLS_SHIFT_JIS: u32 = 1;
pub const ART3M1S_RFVP_NLS_GBK: u32 = 2;
pub const ART3M1S_RFVP_NLS_UTF8: u32 = 3;

pub const ART3M1S_RFVP_AUDIO_LOAD_ENCODED: u32 = 1;
pub const ART3M1S_RFVP_AUDIO_CREATE_STREAM: u32 = 2;
pub const ART3M1S_RFVP_AUDIO_SUBMIT_I16: u32 = 3;
pub const ART3M1S_RFVP_AUDIO_SUBMIT_F32: u32 = 4;
pub const ART3M1S_RFVP_AUDIO_PLAY: u32 = 5;
pub const ART3M1S_RFVP_AUDIO_STOP: u32 = 6;
pub const ART3M1S_RFVP_AUDIO_PAUSE: u32 = 7;
pub const ART3M1S_RFVP_AUDIO_RESUME: u32 = 8;
pub const ART3M1S_RFVP_AUDIO_SET_PARAMS: u32 = 9;
pub const ART3M1S_RFVP_AUDIO_DESTROY_STREAM: u32 = 10;
pub const ART3M1S_RFVP_AUDIO_MASTER_VOLUME: u32 = 11;

pub const ART3M1S_RFVP_AUDIO_SAMPLE_I16: u32 = 1;
pub const ART3M1S_RFVP_AUDIO_SAMPLE_F32: u32 = 2;

pub const ART3M1S_RFVP_AUDIO_ENCODED_UNKNOWN: u32 = 0;
pub const ART3M1S_RFVP_AUDIO_ENCODED_WAV: u32 = 1;
pub const ART3M1S_RFVP_AUDIO_ENCODED_OGG: u32 = 2;
pub const ART3M1S_RFVP_AUDIO_ENCODED_MP3: u32 = 3;
pub const ART3M1S_RFVP_AUDIO_ENCODED_FLAC: u32 = 4;

/// Deprecated direct log callback. Prefer the pull-based log queue
/// (`log_next_bytes`/`poll_log`); hosts that cannot safely expose native-callable
/// trampolines must leave this unset.
pub type Art3m1sRfvpLogCallbackFn = unsafe extern "C" fn(
    level: u32,
    message: *const u8,
    message_len: usize,
    user_data: *mut c_void,
);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Art3m1sRfvpAudioCommandV1 {
    pub struct_size: u32,
    pub kind: u32,
    pub stream_id: u32,
    pub sample_format: u32,
    pub encoded_kind: u32,
    pub sample_rate: u32,
    pub channels: u32,
    pub repeat: u32,
    pub fade_ms: u32,
    pub volume: f32,
    pub pan: f32,
    pub sample_count: usize,
    pub payload: *const u8,
    pub payload_size: usize,
    pub reserved: [u64; 2],
}

/// Wire size of one pulled log record header: level u32 + length u32, both
/// little-endian, followed by the UTF-8 message bytes.
pub const ART3M1S_RFVP_LOG_HEADER_SIZE: usize = 8;

/// Wire size of one pulled text-translation event header: total_len u32,
/// serial u64, slot u32, generation u64, source_len u32, ruby_len u32, all
/// little-endian, followed by the source bytes and then the ruby bytes.
pub const ART3M1S_RFVP_TEXT_EVENT_HEADER_SIZE: usize = 32;

type RuntimeCreateFn = unsafe extern "C" fn(
    game_root_utf8: *const u8,
    game_root_len: usize,
    save_root_utf8: *const u8,
    save_root_len: usize,
    width: u32,
    height: u32,
    backend: i32,
    nls: u32,
    out_runtime: *mut u64,
) -> i32;
type RuntimeDestroyFn = unsafe extern "C" fn(runtime: u64);
type RuntimeStepFn = unsafe extern "C" fn(runtime: u64, delta_ms: u32) -> i32;
type RuntimeIsExitRequestedFn = unsafe extern "C" fn(runtime: u64) -> i32;
type RuntimeStageFn = unsafe extern "C" fn(runtime: u64) -> u32;
type RuntimeCapabilitiesFn = unsafe extern "C" fn(runtime: u64) -> u64;
type RuntimePixelBufferSizeFn = unsafe extern "C" fn(runtime: u64) -> u32;
type RuntimeFeedInputFn = unsafe extern "C" fn(
    runtime: u64,
    events: *const Art3m1sRfvpInputEventV1,
    event_count: usize,
) -> i32;
type RuntimePollAudioCommandFn =
    unsafe extern "C" fn(runtime: u64, out_command: *mut Art3m1sRfvpAudioCommandV1) -> i32;
type RuntimeSetExternalSurfaceFn = unsafe extern "C" fn(
    runtime: u64,
    kind: i32,
    handle: *mut c_void,
    width: u32,
    height: u32,
) -> i32;
type RuntimeClearExternalSurfaceFn = unsafe extern "C" fn(runtime: u64);
type RuntimeAdvanceAndPresentFn = unsafe extern "C" fn(runtime: u64, delta_ms: u32) -> i32;
type RuntimeAdvanceAndRenderFn =
    unsafe extern "C" fn(runtime: u64, delta_ms: u32, out_pixels: *mut u8, capacity: u32) -> u32;
type RuntimeSetLogCallbackFn =
    unsafe extern "C" fn(callback: Option<Art3m1sRfvpLogCallbackFn>, user_data: *mut c_void);
type LogNextBytesFn = unsafe extern "C" fn() -> usize;
type PollLogFn = unsafe extern "C" fn(output: *mut u8, capacity: usize) -> usize;
type RuntimeSetTextReplacementsFn =
    unsafe extern "C" fn(runtime: u64, blob: *const u8, blob_size: usize) -> i32;
type RuntimeSetTextTranslationEnabledFn = unsafe extern "C" fn(runtime: u64, enabled: i32) -> i32;
type RuntimeSubmitTextTranslationFn = unsafe extern "C" fn(
    runtime: u64,
    serial: u64,
    translated_utf8: *const u8,
    translated_len: usize,
) -> i32;
type RuntimeNextTextEventSizeFn = unsafe extern "C" fn(runtime: u64) -> usize;
type RuntimePollTextEventsFn =
    unsafe extern "C" fn(runtime: u64, out: *mut u8, capacity: u32) -> u32;
type RuntimeSetFontOverrideFn =
    unsafe extern "C" fn(runtime: u64, data: *const u8, data_size: u32) -> i32;
type RuntimeClearFontOverrideFn = unsafe extern "C" fn(runtime: u64) -> i32;
/// Trace category mask (vm/syscall/prim/prim_tree/motion/render as bits 0-5).
/// `u32::MAX` restores `RFVP_TRACE*` env-var behavior. Process-wide.
type RuntimeSetTraceMaskFn = unsafe extern "C" fn(runtime: u64, mask: u32) -> i32;
type RuntimeSetProfilerEnabledFn = unsafe extern "C" fn(runtime: u64, enabled: i32) -> i32;
type RuntimeSetDamageVisualizationFn = unsafe extern "C" fn(runtime: u64, enabled: i32) -> i32;
/// Length-probe JSON protocol: null/out-of-capacity returns the required
/// byte count (negated when the buffer is too small), success returns the
/// written byte count.
type RuntimeProfilerSnapshotFn =
    unsafe extern "C" fn(runtime: u64, out: *mut u8, capacity: u32) -> i32;

#[repr(C)]
pub struct Art3m1sRfvpApiV1 {
    pub struct_size: u32,
    pub abi_version: u32,
    pub magic: u64,

    pub runtime_create: Option<RuntimeCreateFn>,
    pub runtime_destroy: Option<RuntimeDestroyFn>,
    pub runtime_step: Option<RuntimeStepFn>,
    pub runtime_is_exit_requested: Option<RuntimeIsExitRequestedFn>,
    pub runtime_stage_width: Option<RuntimeStageFn>,
    pub runtime_stage_height: Option<RuntimeStageFn>,
    pub runtime_capabilities: Option<RuntimeCapabilitiesFn>,
    pub runtime_pixel_buffer_size: Option<RuntimePixelBufferSizeFn>,
    pub runtime_feed_input: Option<RuntimeFeedInputFn>,
    pub runtime_poll_audio_command: Option<RuntimePollAudioCommandFn>,
    pub runtime_set_external_surface: Option<RuntimeSetExternalSurfaceFn>,
    pub runtime_clear_external_surface: Option<RuntimeClearExternalSurfaceFn>,
    pub runtime_advance_and_present: Option<RuntimeAdvanceAndPresentFn>,
    pub runtime_advance_and_render: Option<RuntimeAdvanceAndRenderFn>,
    pub runtime_set_log_callback: Option<RuntimeSetLogCallbackFn>,
    pub log_next_bytes: Option<LogNextBytesFn>,
    pub poll_log: Option<PollLogFn>,
    pub runtime_set_text_replacements: Option<RuntimeSetTextReplacementsFn>,
    pub runtime_set_text_translation_enabled: Option<RuntimeSetTextTranslationEnabledFn>,
    pub runtime_submit_text_translation: Option<RuntimeSubmitTextTranslationFn>,
    pub runtime_next_text_event_size: Option<RuntimeNextTextEventSizeFn>,
    pub runtime_poll_text_events: Option<RuntimePollTextEventsFn>,
    pub runtime_set_font_override: Option<RuntimeSetFontOverrideFn>,
    pub runtime_clear_font_override: Option<RuntimeClearFontOverrideFn>,
    pub runtime_set_trace_mask: Option<RuntimeSetTraceMaskFn>,
    pub runtime_set_profiler_enabled: Option<RuntimeSetProfilerEnabledFn>,
    pub runtime_profiler_snapshot: Option<RuntimeProfilerSnapshotFn>,
    pub runtime_set_damage_visualization: Option<RuntimeSetDamageVisualizationFn>,
}

static RFVP_LOG_CALLBACK: Mutex<Option<(Art3m1sRfvpLogCallbackFn, usize)>> = Mutex::new(None);

const MAX_LOG_RECORDS: usize = 1024;
const MAX_LOG_MESSAGE: usize = 16 * 1024;

struct LogRecord {
    level: u32,
    message: Vec<u8>,
}

struct ApiRuntime {
    runtime: RfvpHostRuntime,
    pending_audio_payload: Vec<u8>,
    pending_text_events: VecDeque<RfvpHostEvent>,
}

// Safety: `RfvpHostRuntime` holds raw engine pointers. All access goes through
// `RUNTIMES`, whose table lock serializes every facade call, and the ABI
// contract still requires a single host owner thread.
unsafe impl Send for ApiRuntime {}

static RUNTIMES: LockedHandleTable<ApiRuntime> = LockedHandleTable::new();
static LOG_QUEUE: Mutex<VecDeque<LogRecord>> = Mutex::new(VecDeque::new());

static API_V1: Art3m1sRfvpApiV1 = Art3m1sRfvpApiV1 {
    struct_size: std::mem::size_of::<Art3m1sRfvpApiV1>() as u32,
    abi_version: ART3M1S_RFVP_API_ABI_VERSION,
    magic: ART3M1S_RFVP_API_ABI_MAGIC,
    runtime_create: Some(runtime_create),
    runtime_destroy: Some(runtime_destroy),
    runtime_step: Some(runtime_step),
    runtime_is_exit_requested: Some(runtime_is_exit_requested),
    runtime_stage_width: Some(runtime_stage_width),
    runtime_stage_height: Some(runtime_stage_height),
    runtime_capabilities: Some(runtime_capabilities),
    runtime_pixel_buffer_size: Some(runtime_pixel_buffer_size),
    runtime_feed_input: Some(runtime_feed_input),
    runtime_poll_audio_command: Some(runtime_poll_audio_command),
    runtime_set_external_surface: Some(runtime_set_external_surface),
    runtime_clear_external_surface: Some(runtime_clear_external_surface),
    runtime_advance_and_present: Some(runtime_advance_and_present),
    runtime_advance_and_render: Some(runtime_advance_and_render),
    runtime_set_log_callback: Some(runtime_set_log_callback),
    log_next_bytes: Some(log_next_bytes),
    poll_log: Some(poll_log),
    runtime_set_text_replacements: Some(runtime_set_text_replacements),
    runtime_set_text_translation_enabled: Some(runtime_set_text_translation_enabled),
    runtime_submit_text_translation: Some(runtime_submit_text_translation),
    runtime_next_text_event_size: Some(runtime_next_text_event_size),
    runtime_poll_text_events: Some(runtime_poll_text_events),
    runtime_set_font_override: Some(runtime_set_font_override),
    runtime_clear_font_override: Some(runtime_clear_font_override),
    runtime_set_trace_mask: Some(runtime_set_trace_mask),
    runtime_set_profiler_enabled: Some(runtime_set_profiler_enabled),
    runtime_profiler_snapshot: Some(runtime_profiler_snapshot),
    runtime_set_damage_visualization: Some(runtime_set_damage_visualization),
};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_rfvp_get_api_v1(out_size: *mut usize) -> *const Art3m1sRfvpApiV1 {
    if !out_size.is_null() {
        unsafe { *out_size = std::mem::size_of::<Art3m1sRfvpApiV1>() };
    }
    &API_V1
}

unsafe extern "C" fn runtime_create(
    game_root_utf8: *const u8,
    game_root_len: usize,
    save_root_utf8: *const u8,
    save_root_len: usize,
    width: u32,
    height: u32,
    backend: i32,
    nls: u32,
    out_runtime: *mut u64,
) -> i32 {
    guard_status(|| {
        if out_runtime.is_null()
            || game_root_utf8.is_null()
            || game_root_len == 0
            || width == 0
            || height == 0
            || (save_root_utf8.is_null() && save_root_len != 0)
        {
            return ART3M1S_RFVP_STATUS_INVALID_ARGUMENT;
        }
        let game_root = match std::str::from_utf8(unsafe {
            std::slice::from_raw_parts(game_root_utf8, game_root_len)
        }) {
            Ok(path) => path,
            Err(_) => return ART3M1S_RFVP_STATUS_INVALID_ARGUMENT,
        };
        let save_root = if save_root_len == 0 {
            None
        } else {
            match std::str::from_utf8(unsafe {
                std::slice::from_raw_parts(save_root_utf8, save_root_len)
            }) {
                Ok(path) => Some(path),
                Err(_) => return ART3M1S_RFVP_STATUS_INVALID_ARGUMENT,
            }
        };
        let nls = match nls {
            ART3M1S_RFVP_NLS_SHIFT_JIS => RfvpNls::ShiftJis,
            ART3M1S_RFVP_NLS_GBK => RfvpNls::Gbk,
            ART3M1S_RFVP_NLS_UTF8 => RfvpNls::Utf8,
            _ => return ART3M1S_RFVP_STATUS_INVALID_ARGUMENT,
        };
        let selection = match BackendSelection::try_from_legacy_int(backend) {
            Ok(selection) => selection,
            Err(_) => return ART3M1S_RFVP_STATUS_UNSUPPORTED,
        };
        let backend = match crate::backend::create_backend(selection, width, height) {
            Ok(backend) => backend,
            Err(_) => return ART3M1S_RFVP_STATUS_ENGINE,
        };
        let runtime = match RfvpHostRuntime::new_directory(
            game_root,
            save_root.map(std::path::Path::new),
            width,
            height,
            nls,
            backend,
            [0.0, 0.0, 0.0, 1.0],
        ) {
            Ok(runtime) => runtime,
            Err(_) => return ART3M1S_RFVP_STATUS_ENGINE,
        };
        let handle = RUNTIMES.insert(ApiRuntime {
            runtime,
            pending_audio_payload: Vec::new(),
            pending_text_events: VecDeque::new(),
        });
        if handle == 0 {
            return ART3M1S_RFVP_STATUS_ENGINE;
        }
        unsafe { *out_runtime = handle };
        ART3M1S_RFVP_STATUS_OK
    })
}

unsafe extern "C" fn runtime_destroy(runtime: u64) {
    if runtime == 0 {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        RUNTIMES.destroy(runtime);
    }));
}

unsafe extern "C" fn runtime_step(runtime: u64, delta_ms: u32) -> i32 {
    guard_status(|| {
        RUNTIMES.with_mut(runtime, ART3M1S_RFVP_STATUS_INVALID_HANDLE, |runtime| {
            runtime
                .runtime
                .step(delta_ms)
                .map_or(ART3M1S_RFVP_STATUS_ENGINE, |_| ART3M1S_RFVP_STATUS_OK)
        })
    })
}

unsafe extern "C" fn runtime_is_exit_requested(runtime: u64) -> i32 {
    guard_i32(|| {
        RUNTIMES.with_mut(runtime, 0, |runtime| {
            i32::from(runtime.runtime.is_exit_requested())
        })
    })
}

unsafe extern "C" fn runtime_stage_width(runtime: u64) -> u32 {
    guard_u32(|| RUNTIMES.with(runtime, 0, |runtime| runtime.runtime.width()))
}

unsafe extern "C" fn runtime_stage_height(runtime: u64) -> u32 {
    guard_u32(|| RUNTIMES.with(runtime, 0, |runtime| runtime.runtime.height()))
}

unsafe extern "C" fn runtime_capabilities(runtime: u64) -> u64 {
    catch_unwind(AssertUnwindSafe(|| {
        RUNTIMES.with_mut(runtime, 0, |runtime| runtime.runtime.capabilities())
    }))
    .unwrap_or(0)
}

unsafe extern "C" fn runtime_pixel_buffer_size(runtime: u64) -> u32 {
    guard_u32(|| {
        RUNTIMES.with(runtime, 0, |runtime| {
            runtime
                .runtime
                .width()
                .saturating_mul(runtime.runtime.height())
                .saturating_mul(4)
        })
    })
}

unsafe extern "C" fn runtime_feed_input(
    runtime: u64,
    events: *const Art3m1sRfvpInputEventV1,
    event_count: usize,
) -> i32 {
    guard_status(|| {
        if events.is_null() && event_count != 0 {
            return ART3M1S_RFVP_STATUS_INVALID_ARGUMENT;
        }
        if event_count > 4096 {
            return ART3M1S_RFVP_STATUS_INVALID_ARGUMENT;
        }
        let native_events: &[Art3m1sRfvpInputEventV1] = if event_count == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(events, event_count) }
        };
        let mut host_events = Vec::with_capacity(native_events.len());
        for event in native_events {
            let Ok(event) = convert_input_event(event) else {
                return ART3M1S_RFVP_STATUS_INVALID_ARGUMENT;
            };
            host_events.push(event);
        }
        RUNTIMES.with_mut(
            runtime,
            ART3M1S_RFVP_STATUS_INVALID_HANDLE,
            |runtime| match runtime.runtime.push_input(&host_events) {
                Ok(()) => ART3M1S_RFVP_STATUS_OK,
                Err(_) => ART3M1S_RFVP_STATUS_ENGINE,
            },
        )
    })
}

unsafe extern "C" fn runtime_poll_audio_command(
    runtime: u64,
    out_command: *mut Art3m1sRfvpAudioCommandV1,
) -> i32 {
    guard_status(|| {
        if out_command.is_null() {
            return ART3M1S_RFVP_STATUS_INVALID_ARGUMENT;
        }
        RUNTIMES.with_mut(runtime, ART3M1S_RFVP_STATUS_INVALID_HANDLE, |runtime| {
            let command = match runtime.runtime.poll_audio_command() {
                Ok(Some(command)) => command,
                Ok(None) => return ART3M1S_RFVP_STATUS_NO_COMMAND,
                Err(_) => return ART3M1S_RFVP_STATUS_ENGINE,
            };
            runtime.pending_audio_payload = command.payload.clone();
            unsafe {
                *out_command = audio_command_v1(&command, &runtime.pending_audio_payload);
            }
            ART3M1S_RFVP_STATUS_OK
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
        if handle.is_null() || width == 0 || height == 0 {
            return ART3M1S_RFVP_STATUS_INVALID_ARGUMENT;
        }
        RUNTIMES.with_mut(runtime, ART3M1S_RFVP_STATUS_INVALID_HANDLE, |runtime| {
            runtime
                .runtime
                .set_native_surface(kind, handle, width, height)
                .map_or(ART3M1S_RFVP_STATUS_UNSUPPORTED, |_| ART3M1S_RFVP_STATUS_OK)
        })
    })
}

unsafe extern "C" fn runtime_clear_external_surface(runtime: u64) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        RUNTIMES.with_mut(runtime, (), |runtime| {
            runtime.runtime.clear_native_surface();
        });
    }));
}

unsafe extern "C" fn runtime_advance_and_present(runtime: u64, delta_ms: u32) -> i32 {
    guard_status(|| {
        RUNTIMES.with_mut(
            runtime,
            ART3M1S_RFVP_STATUS_INVALID_HANDLE,
            |runtime| match runtime.runtime.advance_and_present(delta_ms) {
                Ok(changed) => i32::from(changed),
                Err(_) => ART3M1S_RFVP_STATUS_ENGINE,
            },
        )
    })
}

unsafe extern "C" fn runtime_advance_and_render(
    runtime: u64,
    delta_ms: u32,
    out_pixels: *mut u8,
    capacity: u32,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        if out_pixels.is_null() {
            return 0;
        }
        RUNTIMES.with_mut(runtime, 0, |runtime| {
            let required = runtime
                .runtime
                .width()
                .saturating_mul(runtime.runtime.height())
                .saturating_mul(4) as usize;
            if (capacity as usize) < required {
                return 0;
            }
            if runtime.runtime.step(delta_ms).is_err()
                || runtime
                    .runtime
                    .render_pending_frame()
                    .ok()
                    .flatten()
                    .is_none()
            {
                return 0;
            }
            let Ok(pixels) = runtime.runtime.readback_rgba() else {
                return 0;
            };
            if pixels.len() < required {
                return 0;
            }
            unsafe {
                ptr::copy_nonoverlapping(pixels.as_ptr(), out_pixels, required);
            }
            required as u32
        })
    }))
    .unwrap_or(0)
}

unsafe extern "C" fn runtime_set_log_callback(
    callback: Option<Art3m1sRfvpLogCallbackFn>,
    user_data: *mut c_void,
) {
    let callback = callback.map(|callback| (callback, user_data as usize));
    if let Ok(mut slot) = RFVP_LOG_CALLBACK.lock() {
        *slot = callback;
    }
}

/// Bytes required to pull the oldest queued log record (header + message),
/// or 0 when the queue is empty.
unsafe extern "C" fn log_next_bytes() -> usize {
    catch_unwind(AssertUnwindSafe(|| {
        LOG_QUEUE
            .lock()
            .ok()
            .and_then(|queue| {
                queue
                    .front()
                    .map(|record| ART3M1S_RFVP_LOG_HEADER_SIZE.saturating_add(record.message.len()))
            })
            .unwrap_or(0)
    }))
    .unwrap_or(0)
}

/// Drains complete log records into `output`, oldest first, for as long as
/// whole records fit in `capacity`. Record layout is fixed little-endian:
/// `level: u32`, `message_len: u32`, then `message_len` bytes of UTF-8.
/// Returns the number of bytes written.
unsafe extern "C" fn poll_log(output: *mut u8, capacity: usize) -> usize {
    catch_unwind(AssertUnwindSafe(|| {
        if output.is_null() || capacity == 0 {
            return 0;
        }
        let Ok(mut queue) = LOG_QUEUE.lock() else {
            return 0;
        };
        let mut written = 0usize;
        while let Some(record) = queue.front() {
            let required = ART3M1S_RFVP_LOG_HEADER_SIZE.saturating_add(record.message.len());
            if written.saturating_add(required) > capacity {
                break;
            }
            let record = queue.pop_front().expect("front record exists");
            let mut header = [0u8; ART3M1S_RFVP_LOG_HEADER_SIZE];
            header[0..4].copy_from_slice(&record.level.to_le_bytes());
            header[4..8].copy_from_slice(&(record.message.len() as u32).to_le_bytes());
            unsafe {
                ptr::copy_nonoverlapping(header.as_ptr(), output.add(written), header.len());
            }
            written += ART3M1S_RFVP_LOG_HEADER_SIZE;
            unsafe {
                ptr::copy_nonoverlapping(
                    record.message.as_ptr(),
                    output.add(written),
                    record.message.len(),
                );
            }
            written += record.message.len();
        }
        written
    }))
    .unwrap_or(0)
}

pub(crate) fn dispatch_log(level: &str, message: &str) {
    let level = level.as_bytes().first().copied().unwrap_or(b'I') as u32;
    if let Ok(mut queue) = LOG_QUEUE.lock() {
        while queue.len() >= MAX_LOG_RECORDS {
            queue.pop_front();
        }
        let message = if message.len() > MAX_LOG_MESSAGE {
            &message[..MAX_LOG_MESSAGE]
        } else {
            message
        };
        queue.push_back(LogRecord {
            level,
            message: message.as_bytes().to_vec(),
        });
    }
    let callback = RFVP_LOG_CALLBACK.lock().ok().and_then(|slot| *slot);
    let Some((callback, user_data)) = callback else {
        return;
    };
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        callback(
            level,
            message.as_ptr(),
            message.len(),
            user_data as *mut c_void,
        );
    }));
}


fn audio_command_v1(command: &RfvpHostAudioCommand, payload: &[u8]) -> Art3m1sRfvpAudioCommandV1 {
    Art3m1sRfvpAudioCommandV1 {
        struct_size: std::mem::size_of::<Art3m1sRfvpAudioCommandV1>() as u32,
        kind: match command.kind {
            RfvpHostAudioCommandKind::LoadEncoded => ART3M1S_RFVP_AUDIO_LOAD_ENCODED,
            RfvpHostAudioCommandKind::CreateStream => ART3M1S_RFVP_AUDIO_CREATE_STREAM,
            RfvpHostAudioCommandKind::SubmitI16 => ART3M1S_RFVP_AUDIO_SUBMIT_I16,
            RfvpHostAudioCommandKind::SubmitF32 => ART3M1S_RFVP_AUDIO_SUBMIT_F32,
            RfvpHostAudioCommandKind::Play => ART3M1S_RFVP_AUDIO_PLAY,
            RfvpHostAudioCommandKind::Stop => ART3M1S_RFVP_AUDIO_STOP,
            RfvpHostAudioCommandKind::Pause => ART3M1S_RFVP_AUDIO_PAUSE,
            RfvpHostAudioCommandKind::Resume => ART3M1S_RFVP_AUDIO_RESUME,
            RfvpHostAudioCommandKind::SetParams => ART3M1S_RFVP_AUDIO_SET_PARAMS,
            RfvpHostAudioCommandKind::DestroyStream => ART3M1S_RFVP_AUDIO_DESTROY_STREAM,
            RfvpHostAudioCommandKind::MasterVolume => ART3M1S_RFVP_AUDIO_MASTER_VOLUME,
        },
        stream_id: command.stream_id,
        sample_format: match command.sample_format {
            Some(RfvpAudioSampleFormat::I16) => ART3M1S_RFVP_AUDIO_SAMPLE_I16,
            Some(RfvpAudioSampleFormat::F32) => ART3M1S_RFVP_AUDIO_SAMPLE_F32,
            None => 0,
        },
        encoded_kind: match command.encoded_kind {
            Some(RfvpEncodedAudioKind::Unknown) => ART3M1S_RFVP_AUDIO_ENCODED_UNKNOWN,
            Some(RfvpEncodedAudioKind::Wav) => ART3M1S_RFVP_AUDIO_ENCODED_WAV,
            Some(RfvpEncodedAudioKind::Ogg) => ART3M1S_RFVP_AUDIO_ENCODED_OGG,
            Some(RfvpEncodedAudioKind::Mp3) => ART3M1S_RFVP_AUDIO_ENCODED_MP3,
            Some(RfvpEncodedAudioKind::Flac) => ART3M1S_RFVP_AUDIO_ENCODED_FLAC,
            None => 0,
        },
        sample_rate: command.sample_rate,
        channels: command.channels,
        repeat: u32::from(command.repeat),
        fade_ms: command.fade_ms,
        volume: command.volume,
        pan: command.pan,
        sample_count: command.sample_count,
        payload: if payload.is_empty() {
            ptr::null()
        } else {
            payload.as_ptr()
        },
        payload_size: payload.len(),
        reserved: [0; 2],
    }
}

unsafe extern "C" fn runtime_set_text_replacements(
    runtime: u64,
    blob: *const u8,
    blob_size: usize,
) -> i32 {
    guard_status(|| {
        let json = if blob.is_null() || blob_size == 0 {
            ""
        } else {
            match std::str::from_utf8(unsafe { std::slice::from_raw_parts(blob, blob_size) }) {
                Ok(json) => json,
                Err(_) => return ART3M1S_RFVP_STATUS_INVALID_ARGUMENT,
            }
        };
        RUNTIMES.with_mut(
            runtime,
            ART3M1S_RFVP_STATUS_INVALID_HANDLE,
            |runtime| match runtime.runtime.set_text_replacements(json) {
                Ok(()) => ART3M1S_RFVP_STATUS_OK,
                Err(_) => ART3M1S_RFVP_STATUS_ENGINE,
            },
        )
    })
}

unsafe extern "C" fn runtime_set_text_translation_enabled(runtime: u64, enabled: i32) -> i32 {
    guard_status(|| {
        let enabled = enabled != 0;
        RUNTIMES.with_mut(runtime, ART3M1S_RFVP_STATUS_INVALID_HANDLE, |runtime| {
            if runtime
                .runtime
                .set_text_translation_enabled(enabled)
                .is_err()
            {
                return ART3M1S_RFVP_STATUS_ENGINE;
            }
            // The event queue is how the host observes translation requests,
            // so it tracks the translation toggle.
            match runtime.runtime.set_events_enabled(enabled) {
                Ok(()) => ART3M1S_RFVP_STATUS_OK,
                Err(_) => ART3M1S_RFVP_STATUS_ENGINE,
            }
        })
    })
}

unsafe extern "C" fn runtime_submit_text_translation(
    runtime: u64,
    serial: u64,
    translated_utf8: *const u8,
    translated_len: usize,
) -> i32 {
    guard_status(|| {
        let translated = if translated_utf8.is_null() || translated_len == 0 {
            None
        } else {
            match std::str::from_utf8(unsafe {
                std::slice::from_raw_parts(translated_utf8, translated_len)
            }) {
                Ok(text) => Some(text),
                Err(_) => return ART3M1S_RFVP_STATUS_INVALID_ARGUMENT,
            }
        };
        RUNTIMES.with_mut(
            runtime,
            ART3M1S_RFVP_STATUS_INVALID_HANDLE,
            |runtime| match runtime.runtime.submit_text_translation(serial, translated) {
                Ok(()) => ART3M1S_RFVP_STATUS_OK,
                Err(_) => ART3M1S_RFVP_STATUS_ENGINE,
            },
        )
    })
}

fn drain_host_text_events(runtime: &mut ApiRuntime) {
    if let Ok(events) = runtime.runtime.poll_events() {
        for event in events {
            runtime.pending_text_events.push_back(event);
        }
    }
}

fn text_event_record_len(event: &RfvpHostEvent) -> usize {
    match event {
        RfvpHostEvent::TextTranslation { source, ruby, .. } => {
            ART3M1S_RFVP_TEXT_EVENT_HEADER_SIZE + source.len() + ruby.as_deref().map_or(0, str::len)
        }
    }
}

fn encode_text_event(event: &RfvpHostEvent, out: *mut u8, capacity: usize) -> Option<usize> {
    match event {
        RfvpHostEvent::TextTranslation {
            serial,
            slot,
            generation,
            source,
            ruby,
        } => {
            let ruby = ruby.as_deref().unwrap_or("");
            if source.len() > u32::MAX as usize || ruby.len() > u32::MAX as usize {
                return None;
            }
            let total_len = ART3M1S_RFVP_TEXT_EVENT_HEADER_SIZE + source.len() + ruby.len();
            if total_len > capacity {
                return None;
            }
            let mut header = [0u8; ART3M1S_RFVP_TEXT_EVENT_HEADER_SIZE];
            header[0..4].copy_from_slice(&(total_len as u32).to_le_bytes());
            header[4..12].copy_from_slice(&serial.to_le_bytes());
            header[12..16].copy_from_slice(&slot.to_le_bytes());
            header[16..24].copy_from_slice(&generation.to_le_bytes());
            header[24..28].copy_from_slice(&(source.len() as u32).to_le_bytes());
            header[28..32].copy_from_slice(&(ruby.len() as u32).to_le_bytes());
            unsafe {
                ptr::copy_nonoverlapping(header.as_ptr(), out, header.len());
                ptr::copy_nonoverlapping(source.as_ptr(), out.add(header.len()), source.len());
                ptr::copy_nonoverlapping(
                    ruby.as_ptr(),
                    out.add(header.len() + source.len()),
                    ruby.len(),
                );
            }
            Some(total_len)
        }
    }
}

unsafe extern "C" fn runtime_next_text_event_size(runtime: u64) -> usize {
    catch_unwind(AssertUnwindSafe(|| {
        RUNTIMES.with_mut(runtime, 0, |runtime| {
            drain_host_text_events(runtime);
            runtime
                .pending_text_events
                .front()
                .map_or(0, text_event_record_len)
        })
    }))
    .unwrap_or(0)
}

unsafe extern "C" fn runtime_poll_text_events(runtime: u64, out: *mut u8, capacity: u32) -> u32 {
    guard_u32(|| {
        if out.is_null() {
            return 0;
        }
        RUNTIMES.with_mut(runtime, 0, |runtime| {
            drain_host_text_events(runtime);
            let capacity = capacity as usize;
            let mut written = 0usize;
            while let Some(event) = runtime.pending_text_events.front() {
                let Some(record_len) =
                    encode_text_event(event, out.wrapping_add(written), capacity - written)
                else {
                    break;
                };
                written += record_len;
                runtime.pending_text_events.pop_front();
            }
            written as u32
        })
    })
}

/// Forces a replacement font face (raw TrueType/OpenType bytes) for every
/// text draw. Empty or null data is rejected with INVALID_ARGUMENT; font data
/// the engine cannot parse is also rejected and leaves the current override
/// untouched.
unsafe extern "C" fn runtime_set_font_override(
    runtime: u64,
    data: *const u8,
    data_size: u32,
) -> i32 {
    guard_status(|| {
        if data.is_null() || data_size == 0 {
            return ART3M1S_RFVP_STATUS_INVALID_ARGUMENT;
        }
        let bytes = unsafe { std::slice::from_raw_parts(data, data_size as usize) };
        RUNTIMES.with_mut(
            runtime,
            ART3M1S_RFVP_STATUS_INVALID_HANDLE,
            |runtime| match runtime.runtime.set_font_override(bytes) {
                Ok(()) => ART3M1S_RFVP_STATUS_OK,
                Err(_) => ART3M1S_RFVP_STATUS_INVALID_ARGUMENT,
            },
        )
    })
}

unsafe extern "C" fn runtime_clear_font_override(runtime: u64) -> i32 {
    guard_status(|| {
        RUNTIMES.with_mut(
            runtime,
            ART3M1S_RFVP_STATUS_INVALID_HANDLE,
            |runtime| match runtime.runtime.clear_font_override() {
                Ok(()) => ART3M1S_RFVP_STATUS_OK,
                Err(_) => ART3M1S_RFVP_STATUS_ENGINE,
            },
        )
    })
}

unsafe extern "C" fn runtime_set_trace_mask(runtime: u64, mask: u32) -> i32 {
    guard_status(|| {
        RUNTIMES.with_mut(
            runtime,
            ART3M1S_RFVP_STATUS_INVALID_HANDLE,
            |runtime| match runtime.runtime.set_trace_mask(Some(mask)) {
                Ok(()) => ART3M1S_RFVP_STATUS_OK,
                Err(_) => ART3M1S_RFVP_STATUS_ENGINE,
            },
        )
    })
}

unsafe extern "C" fn runtime_set_profiler_enabled(runtime: u64, enabled: i32) -> i32 {
    guard_status(|| {
        RUNTIMES.with_mut(runtime, ART3M1S_RFVP_STATUS_INVALID_HANDLE, |runtime| {
            runtime.runtime.set_profiler_enabled(enabled != 0);
            ART3M1S_RFVP_STATUS_OK
        })
    })
}

unsafe extern "C" fn runtime_set_damage_visualization(runtime: u64, enabled: i32) -> i32 {
    guard_status(|| {
        RUNTIMES.with_mut(runtime, ART3M1S_RFVP_STATUS_INVALID_HANDLE, |runtime| {
            runtime.runtime.set_damage_visualization(enabled != 0);
            ART3M1S_RFVP_STATUS_OK
        })
    })
}

unsafe extern "C" fn runtime_profiler_snapshot(runtime: u64, out: *mut u8, capacity: u32) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        RUNTIMES.with_mut(runtime, 0, |runtime| {
            let json = runtime.runtime.profiler_snapshot_json();
            let required = i32::try_from(json.len()).unwrap_or(i32::MAX);
            if out.is_null() || capacity == 0 {
                return required;
            }
            if (capacity as usize) < json.len() {
                return -required;
            }
            unsafe {
                ptr::copy_nonoverlapping(json.as_ptr(), out, json.len());
            }
            required
        })
    }))
    .unwrap_or(0)
}

fn guard_status(callback: impl FnOnce() -> i32) -> i32 {
    catch_unwind(AssertUnwindSafe(callback)).unwrap_or(ART3M1S_RFVP_STATUS_ENGINE)
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
    use art3m1s_rfvp::RfvpHostInputEvent;

    #[test]
    fn api_table_is_versioned_and_complete() {
        let mut size = 0usize;
        let api = unsafe { art3m1s_rfvp_get_api_v1(&mut size) };
        assert!(!api.is_null());
        assert_eq!(size, std::mem::size_of::<Art3m1sRfvpApiV1>());
        let api = unsafe { &*api };
        assert_eq!(api.abi_version, ART3M1S_RFVP_API_ABI_VERSION);
        assert_eq!(api.magic, ART3M1S_RFVP_API_ABI_MAGIC);
        assert!(api.runtime_create.is_some());
        assert!(api.runtime_advance_and_render.is_some());
        assert!(api.runtime_poll_audio_command.is_some());
        assert!(api.runtime_set_log_callback.is_some());
        assert!(api.log_next_bytes.is_some());
        assert!(api.poll_log.is_some());
        assert!(api.runtime_set_text_replacements.is_some());
        assert!(api.runtime_set_text_translation_enabled.is_some());
        assert!(api.runtime_submit_text_translation.is_some());
        assert!(api.runtime_next_text_event_size.is_some());
        assert!(api.runtime_poll_text_events.is_some());
        assert!(api.runtime_set_font_override.is_some());
        assert!(api.runtime_clear_font_override.is_some());
        assert!(api.runtime_set_trace_mask.is_some());
        assert!(api.runtime_set_profiler_enabled.is_some());
        assert!(api.runtime_profiler_snapshot.is_some());
        assert!(api.runtime_set_damage_visualization.is_some());
    }

    #[test]
    fn invalid_handles_are_rejected_without_dereferencing() {
        for garbage in [u64::MAX, 1 << 32, 0xDEAD_BEEF_CAFE] {
            assert_eq!(
                unsafe { runtime_step(garbage, 16) },
                ART3M1S_RFVP_STATUS_INVALID_HANDLE
            );
            assert_eq!(unsafe { runtime_stage_width(garbage) }, 0);
            assert_eq!(unsafe { runtime_is_exit_requested(garbage) }, 0);
            assert_eq!(unsafe { runtime_capabilities(garbage) }, 0);
            assert_eq!(unsafe { runtime_pixel_buffer_size(garbage) }, 0);
            assert_eq!(
                unsafe { runtime_feed_input(garbage, ptr::null(), 0) },
                ART3M1S_RFVP_STATUS_INVALID_HANDLE
            );
            let mut command = std::mem::MaybeUninit::<Art3m1sRfvpAudioCommandV1>::uninit();
            assert_eq!(
                unsafe { runtime_poll_audio_command(garbage, command.as_mut_ptr()) },
                ART3M1S_RFVP_STATUS_INVALID_HANDLE
            );
            assert_eq!(
                unsafe { runtime_advance_and_present(garbage, 16) },
                ART3M1S_RFVP_STATUS_INVALID_HANDLE
            );
            assert_eq!(
                unsafe { runtime_advance_and_render(garbage, 16, ptr::null_mut(), 0) },
                0
            );
            assert_eq!(
                unsafe { runtime_set_text_replacements(garbage, ptr::null(), 0) },
                ART3M1S_RFVP_STATUS_INVALID_HANDLE
            );
            assert_eq!(
                unsafe { runtime_set_text_translation_enabled(garbage, 1) },
                ART3M1S_RFVP_STATUS_INVALID_HANDLE
            );
            assert_eq!(
                unsafe { runtime_submit_text_translation(garbage, 1, ptr::null(), 0) },
                ART3M1S_RFVP_STATUS_INVALID_HANDLE
            );
            assert_eq!(unsafe { runtime_next_text_event_size(garbage) }, 0);
            assert_eq!(
                unsafe { runtime_poll_text_events(garbage, ptr::null_mut(), 0) },
                0
            );
            assert_eq!(
                unsafe { runtime_set_font_override(garbage, ptr::null(), 0) },
                ART3M1S_RFVP_STATUS_INVALID_ARGUMENT
            );
            let font_byte = [0u8; 1];
            assert_eq!(
                unsafe { runtime_set_font_override(garbage, font_byte.as_ptr(), 1) },
                ART3M1S_RFVP_STATUS_INVALID_HANDLE
            );
            assert_eq!(
                unsafe { runtime_clear_font_override(garbage) },
                ART3M1S_RFVP_STATUS_INVALID_HANDLE
            );
            assert_eq!(
                unsafe { runtime_set_trace_mask(garbage, 0) },
                ART3M1S_RFVP_STATUS_INVALID_HANDLE
            );
            assert_eq!(
                unsafe { runtime_set_profiler_enabled(garbage, 1) },
                ART3M1S_RFVP_STATUS_INVALID_HANDLE
            );
            assert_eq!(
                unsafe { runtime_profiler_snapshot(garbage, ptr::null_mut(), 0) },
                0
            );
            assert_eq!(
                unsafe { runtime_set_damage_visualization(garbage, 1) },
                ART3M1S_RFVP_STATUS_INVALID_HANDLE
            );
            // Double destroy and garbage destroy are safe no-ops.
            unsafe { runtime_destroy(garbage) };
            unsafe { runtime_destroy(garbage) };
        }
    }

    static LOG_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn logs_are_pulled_in_order_with_little_endian_headers() {
        let _guard = LOG_TEST_LOCK.lock().unwrap();
        {
            let mut queue = LOG_QUEUE.lock().unwrap();
            queue.clear();
        }
        dispatch_log("W", "warning");
        dispatch_log("E", "error");

        let first = unsafe { log_next_bytes() };
        assert_eq!(first, ART3M1S_RFVP_LOG_HEADER_SIZE + "warning".len());

        let mut output = vec![0u8; first + ART3M1S_RFVP_LOG_HEADER_SIZE + "error".len()];
        let written = unsafe { poll_log(output.as_mut_ptr(), output.len()) };
        assert_eq!(written, output.len());

        let level = u32::from_le_bytes(output[0..4].try_into().unwrap());
        let len = u32::from_le_bytes(output[4..8].try_into().unwrap()) as usize;
        assert_eq!(level, b'W' as u32);
        assert_eq!(&output[8..8 + len], b"warning");
        let offset = 8 + len;
        let level = u32::from_le_bytes(output[offset..offset + 4].try_into().unwrap());
        assert_eq!(level, b'E' as u32);
        assert_eq!(unsafe { log_next_bytes() }, 0);
    }

    #[test]
    fn poll_log_keeps_records_that_do_not_fit() {
        let _guard = LOG_TEST_LOCK.lock().unwrap();
        {
            let mut queue = LOG_QUEUE.lock().unwrap();
            queue.clear();
        }
        dispatch_log("I", "first");
        dispatch_log("I", "second");

        let one_record = ART3M1S_RFVP_LOG_HEADER_SIZE + "first".len();
        let mut output = vec![0u8; one_record];
        let written = unsafe { poll_log(output.as_mut_ptr(), output.len()) };
        assert_eq!(written, one_record);
        assert_eq!(
            unsafe { log_next_bytes() },
            ART3M1S_RFVP_LOG_HEADER_SIZE + "second".len()
        );
        {
            let mut queue = LOG_QUEUE.lock().unwrap();
            queue.clear();
        }
    }

    #[test]
    fn text_events_encode_as_length_prefixed_records() {
        let event = RfvpHostEvent::TextTranslation {
            serial: 0x1122_3344_5566_7788,
            slot: 7,
            generation: 42,
            source: "こんにちは".to_string(),
            ruby: Some("コンにちは".to_string()),
        };
        let record_len = text_event_record_len(&event);
        assert_eq!(
            record_len,
            ART3M1S_RFVP_TEXT_EVENT_HEADER_SIZE + "こんにちは".len() + "コンにちは".len()
        );

        let mut output = vec![0u8; record_len + 16];
        assert_eq!(encode_text_event(&event, output.as_mut_ptr(), 3), None);
        assert_eq!(
            encode_text_event(&event, output.as_mut_ptr(), output.len()),
            Some(record_len)
        );

        let total = u32::from_le_bytes(output[0..4].try_into().unwrap()) as usize;
        assert_eq!(total, record_len);
        let serial = u64::from_le_bytes(output[4..12].try_into().unwrap());
        let slot = u32::from_le_bytes(output[12..16].try_into().unwrap());
        let generation = u64::from_le_bytes(output[16..24].try_into().unwrap());
        let source_len = u32::from_le_bytes(output[24..28].try_into().unwrap()) as usize;
        let ruby_len = u32::from_le_bytes(output[28..32].try_into().unwrap()) as usize;
        assert_eq!(serial, 0x1122_3344_5566_7788);
        assert_eq!(slot, 7);
        assert_eq!(generation, 42);
        let source_start = ART3M1S_RFVP_TEXT_EVENT_HEADER_SIZE;
        assert_eq!(
            &output[source_start..source_start + source_len],
            "こんにちは".as_bytes()
        );
        let ruby_start = source_start + source_len;
        assert_eq!(
            &output[ruby_start..ruby_start + ruby_len],
            "コンにちは".as_bytes()
        );
        assert_eq!(source_start + source_len + ruby_len, record_len);

        let no_ruby = RfvpHostEvent::TextTranslation {
            serial: 1,
            slot: 0,
            generation: 0,
            source: "a".to_string(),
            ruby: None,
        };
        assert_eq!(
            text_event_record_len(&no_ruby),
            ART3M1S_RFVP_TEXT_EVENT_HEADER_SIZE + 1
        );
    }

    #[test]
    fn input_events_map_to_the_host_adapter() {
        let key = Art3m1sRfvpInputEventV1 {
            struct_size: std::mem::size_of::<Art3m1sRfvpInputEventV1>() as u32,
            kind: ART3M1S_RFVP_INPUT_KEY,
            code: 13,
            phase: ART3M1S_RFVP_INPUT_PHASE_DOWN,
            x: 0,
            y: 0,
            value: 0,
            modifiers: 0,
            id: 0,
        };
        assert!(matches!(
            convert_input_event(&key),
            Ok(RfvpHostInputEvent::Key { code: 13, .. })
        ));
    }
}
