//! Versioned RFVP engine ABI exposed by the Art3m1s core library.
//!
//! This table is separate from [`super::api::Art3m1sApiV1`]. RFVP keeps its
//! own runtime/resources/frame handles, but rendering and presentation stay
//! inside `art3m1s-rfvp` and the shared `art3m1s-render` backend.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;

use art3m1s_rfvp::{
    RfvpAudioSampleFormat, RfvpEncodedAudioKind, RfvpHostAudioCommand, RfvpHostAudioCommandKind,
    RfvpHostInputEvent, RfvpHostRuntime, RfvpNls, RfvpPointerButton, RfvpTouchPhase,
};

use crate::backend::BackendSelection;

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

pub const ART3M1S_RFVP_INPUT_KEY: u32 = 1;
pub const ART3M1S_RFVP_INPUT_TEXT: u32 = 2;
pub const ART3M1S_RFVP_INPUT_POINTER_MOVE: u32 = 3;
pub const ART3M1S_RFVP_INPUT_POINTER_BUTTON: u32 = 4;
pub const ART3M1S_RFVP_INPUT_WHEEL: u32 = 5;
pub const ART3M1S_RFVP_INPUT_TOUCH: u32 = 6;
pub const ART3M1S_RFVP_INPUT_FOCUS: u32 = 7;
pub const ART3M1S_RFVP_INPUT_QUIT: u32 = 8;

pub const ART3M1S_RFVP_INPUT_PHASE_DOWN: u32 = 0;
pub const ART3M1S_RFVP_INPUT_PHASE_UP: u32 = 1;
pub const ART3M1S_RFVP_INPUT_PHASE_REPEAT: u32 = 2;
pub const ART3M1S_RFVP_INPUT_PHASE_MOVE: u32 = 3;

pub const ART3M1S_RFVP_POINTER_LEFT: u32 = 1 << 0;
pub const ART3M1S_RFVP_POINTER_RIGHT: u32 = 1 << 1;
pub const ART3M1S_RFVP_POINTER_MIDDLE: u32 = 1 << 2;

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

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Art3m1sRfvpInputEventV1 {
    pub struct_size: u32,
    pub kind: u32,
    pub code: u32,
    pub phase: u32,
    pub x: i32,
    pub y: i32,
    pub value: i32,
    pub modifiers: u32,
    pub id: u64,
}

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
}

struct ApiRuntime {
    runtime: RfvpHostRuntime,
    pending_audio_payload: Vec<u8>,
}

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
        let runtime = Box::new(ApiRuntime {
            runtime,
            pending_audio_payload: Vec::new(),
        });
        unsafe { *out_runtime = Box::into_raw(runtime) as u64 };
        ART3M1S_RFVP_STATUS_OK
    })
}

unsafe extern "C" fn runtime_destroy(runtime: u64) {
    if runtime == 0 {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        drop(unsafe { Box::from_raw(runtime as *mut ApiRuntime) });
    }));
}

unsafe extern "C" fn runtime_step(runtime: u64, delta_ms: u32) -> i32 {
    guard_status(|| {
        let runtime = match unsafe { runtime_mut(runtime) } {
            Ok(runtime) => runtime,
            Err(status) => return status,
        };
        runtime
            .runtime
            .step(delta_ms)
            .map_or(ART3M1S_RFVP_STATUS_ENGINE, |_| ART3M1S_RFVP_STATUS_OK)
    })
}

unsafe extern "C" fn runtime_is_exit_requested(runtime: u64) -> i32 {
    guard_i32(|| {
        let Ok(runtime) = (unsafe { runtime_mut(runtime) }) else {
            return 0;
        };
        i32::from(runtime.runtime.is_exit_requested())
    })
}

unsafe extern "C" fn runtime_stage_width(runtime: u64) -> u32 {
    guard_u32(|| {
        unsafe { runtime_mut(runtime) }
            .map(|runtime| runtime.runtime.width())
            .unwrap_or(0)
    })
}

unsafe extern "C" fn runtime_stage_height(runtime: u64) -> u32 {
    guard_u32(|| {
        unsafe { runtime_mut(runtime) }
            .map(|runtime| runtime.runtime.height())
            .unwrap_or(0)
    })
}

unsafe extern "C" fn runtime_capabilities(runtime: u64) -> u64 {
    catch_unwind(AssertUnwindSafe(|| {
        unsafe { runtime_mut(runtime) }
            .map(|runtime| runtime.runtime.capabilities())
            .unwrap_or(0)
    }))
    .unwrap_or(0)
}

unsafe extern "C" fn runtime_pixel_buffer_size(runtime: u64) -> u32 {
    guard_u32(|| {
        let Ok(runtime) = (unsafe { runtime_mut(runtime) }) else {
            return 0;
        };
        runtime
            .runtime
            .width()
            .saturating_mul(runtime.runtime.height())
            .saturating_mul(4)
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
        let Ok(runtime) = (unsafe { runtime_mut(runtime) }) else {
            return ART3M1S_RFVP_STATUS_INVALID_HANDLE;
        };
        if event_count > 4096 {
            return ART3M1S_RFVP_STATUS_INVALID_ARGUMENT;
        }
        let native_events = unsafe { std::slice::from_raw_parts(events, event_count) };
        let mut host_events = Vec::with_capacity(native_events.len());
        for event in native_events {
            let Ok(event) = convert_input_event(event) else {
                return ART3M1S_RFVP_STATUS_INVALID_ARGUMENT;
            };
            host_events.push(event);
        }
        match runtime.runtime.push_input(&host_events) {
            Ok(()) => ART3M1S_RFVP_STATUS_OK,
            Err(_) => ART3M1S_RFVP_STATUS_ENGINE,
        }
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
        let Ok(runtime) = (unsafe { runtime_mut(runtime) }) else {
            return ART3M1S_RFVP_STATUS_INVALID_HANDLE;
        };
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
        let Ok(runtime) = (unsafe { runtime_mut(runtime) }) else {
            return ART3M1S_RFVP_STATUS_INVALID_HANDLE;
        };
        runtime
            .runtime
            .set_native_surface(kind, handle, width, height)
            .map_or(ART3M1S_RFVP_STATUS_UNSUPPORTED, |_| ART3M1S_RFVP_STATUS_OK)
    })
}

unsafe extern "C" fn runtime_clear_external_surface(runtime: u64) {
    if let Ok(runtime) = unsafe { runtime_mut(runtime) } {
        runtime.runtime.clear_native_surface();
    }
}

unsafe extern "C" fn runtime_advance_and_present(runtime: u64, delta_ms: u32) -> i32 {
    guard_status(|| {
        let Ok(runtime) = (unsafe { runtime_mut(runtime) }) else {
            return ART3M1S_RFVP_STATUS_INVALID_HANDLE;
        };
        match runtime.runtime.advance_and_present(delta_ms) {
            Ok(changed) => i32::from(changed),
            Err(_) => ART3M1S_RFVP_STATUS_ENGINE,
        }
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
        let Ok(runtime) = (unsafe { runtime_mut(runtime) }) else {
            return 0;
        };
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
    }))
    .unwrap_or(0)
}

unsafe fn runtime_mut(runtime: u64) -> Result<&'static mut ApiRuntime, i32> {
    if runtime == 0 {
        return Err(ART3M1S_RFVP_STATUS_INVALID_HANDLE);
    }
    Ok(unsafe { &mut *(runtime as *mut ApiRuntime) })
}

fn convert_input_event(event: &Art3m1sRfvpInputEventV1) -> Result<RfvpHostInputEvent, i32> {
    if (event.struct_size as usize) < std::mem::size_of::<Art3m1sRfvpInputEventV1>() {
        return Err(ART3M1S_RFVP_STATUS_INVALID_ARGUMENT);
    }
    let event = match event.kind {
        ART3M1S_RFVP_INPUT_KEY => RfvpHostInputEvent::Key {
            code: event.code,
            pressed: event.phase != ART3M1S_RFVP_INPUT_PHASE_UP,
            repeat: event.phase == ART3M1S_RFVP_INPUT_PHASE_REPEAT,
            modifiers: event.modifiers,
        },
        ART3M1S_RFVP_INPUT_TEXT => {
            let Some(character) = char::from_u32(event.code) else {
                return Err(ART3M1S_RFVP_STATUS_INVALID_ARGUMENT);
            };
            RfvpHostInputEvent::Text { character }
        }
        ART3M1S_RFVP_INPUT_POINTER_MOVE => RfvpHostInputEvent::PointerMove {
            x: event.x,
            y: event.y,
        },
        ART3M1S_RFVP_INPUT_POINTER_BUTTON => RfvpHostInputEvent::PointerButton {
            button: match event.code {
                ART3M1S_RFVP_POINTER_LEFT => RfvpPointerButton::Left,
                ART3M1S_RFVP_POINTER_RIGHT => RfvpPointerButton::Right,
                ART3M1S_RFVP_POINTER_MIDDLE => RfvpPointerButton::Middle,
                _ => return Err(ART3M1S_RFVP_STATUS_INVALID_ARGUMENT),
            },
            pressed: event.phase == ART3M1S_RFVP_INPUT_PHASE_DOWN,
            x: event.x,
            y: event.y,
        },
        ART3M1S_RFVP_INPUT_WHEEL => RfvpHostInputEvent::Wheel {
            delta_x: event.x,
            delta_y: event.y,
        },
        ART3M1S_RFVP_INPUT_TOUCH => RfvpHostInputEvent::Touch {
            id: event.id,
            phase: match event.phase {
                ART3M1S_RFVP_INPUT_PHASE_DOWN => RfvpTouchPhase::Down,
                ART3M1S_RFVP_INPUT_PHASE_MOVE => RfvpTouchPhase::Move,
                ART3M1S_RFVP_INPUT_PHASE_UP => RfvpTouchPhase::Up,
                _ => return Err(ART3M1S_RFVP_STATUS_INVALID_ARGUMENT),
            },
            x: event.x,
            y: event.y,
        },
        ART3M1S_RFVP_INPUT_FOCUS => RfvpHostInputEvent::Focus {
            focused: event.phase != 0,
        },
        ART3M1S_RFVP_INPUT_QUIT => RfvpHostInputEvent::Quit,
        _ => return Err(ART3M1S_RFVP_STATUS_INVALID_ARGUMENT),
    };
    Ok(event)
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
