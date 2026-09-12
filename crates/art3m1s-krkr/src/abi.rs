//! Versioned C ABI exposed by the native KRKR host shim.

use std::ffi::{c_char, c_void};

use crate::protocol::{
    Art3m1sKrkrAudioCommandV1, Art3m1sKrkrAudioConsumedV1, Art3m1sKrkrFrameV1,
    Art3m1sKrkrInputEventV1, Art3m1sKrkrProbeV1, Art3m1sKrkrRuntimeConfigV1,
    Art3m1sKrkrRuntimeHandle,
};

pub const ART3M1S_KRKR_API_ABI_VERSION: u32 = 1;
pub const ART3M1S_KRKR_API_ABI_MAGIC: u64 = 0x3156_4B52_4D33_4152; // "RA3MKRV1"

pub const ART3M1S_KRKR_API_STATUS_OK: i32 = 0;
pub const ART3M1S_KRKR_API_STATUS_INVALID_ARGUMENT: i32 = -1;
pub const ART3M1S_KRKR_API_STATUS_UNSUPPORTED: i32 = -2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KrkrAbiError {
    Null,
    SizeMismatch { expected: u32, actual: u32 },
    VersionMismatch { expected: u32, actual: u32 },
    MagicMismatch { expected: u64, actual: u64 },
    MissingRequiredFunction(&'static str),
}

impl std::fmt::Display for KrkrAbiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Null => write!(f, "KRKR ABI table is null"),
            Self::SizeMismatch { expected, actual } => {
                write!(
                    f,
                    "KRKR ABI size mismatch: expected {expected}, got {actual}"
                )
            }
            Self::VersionMismatch { expected, actual } => {
                write!(
                    f,
                    "KRKR ABI version mismatch: expected {expected}, got {actual}"
                )
            }
            Self::MagicMismatch { expected, actual } => {
                write!(
                    f,
                    "KRKR ABI magic mismatch: expected {expected:#x}, got {actual:#x}"
                )
            }
            Self::MissingRequiredFunction(name) => {
                write!(f, "KRKR ABI is missing required function {name}")
            }
        }
    }
}

impl std::error::Error for KrkrAbiError {}

type ProbeProjectFn =
    unsafe extern "C" fn(game_root_utf8: *const c_char, out_probe: *mut Art3m1sKrkrProbeV1) -> i32;
type RuntimeCreateFn = unsafe extern "C" fn(
    game_root_utf8: *const c_char,
    save_root_utf8: *const c_char,
    config: *const Art3m1sKrkrRuntimeConfigV1,
    out_runtime: *mut Art3m1sKrkrRuntimeHandle,
) -> i32;
type RuntimeDestroyFn = unsafe extern "C" fn(runtime: Art3m1sKrkrRuntimeHandle);
type RuntimeStageFn = unsafe extern "C" fn(runtime: Art3m1sKrkrRuntimeHandle) -> u32;
type RuntimePixelBufferSizeFn = unsafe extern "C" fn(runtime: Art3m1sKrkrRuntimeHandle) -> u32;
type RuntimePushInputFn = unsafe extern "C" fn(
    runtime: Art3m1sKrkrRuntimeHandle,
    events: *const Art3m1sKrkrInputEventV1,
    event_count: usize,
) -> i32;
type RuntimeTickFn = unsafe extern "C" fn(runtime: Art3m1sKrkrRuntimeHandle) -> i32;
type RuntimeAcquireFrameFn = unsafe extern "C" fn(
    runtime: Art3m1sKrkrRuntimeHandle,
    out_frame: *mut Art3m1sKrkrFrameV1,
) -> i32;
type RuntimeReleaseFrameFn =
    unsafe extern "C" fn(runtime: Art3m1sKrkrRuntimeHandle, frame_id: u64) -> i32;
type RuntimePollAudioCommandFn = unsafe extern "C" fn(
    runtime: Art3m1sKrkrRuntimeHandle,
    out_command: *mut Art3m1sKrkrAudioCommandV1,
) -> i32;
type RuntimeSubmitAudioConsumedFn = unsafe extern "C" fn(
    runtime: Art3m1sKrkrRuntimeHandle,
    consumed: *const Art3m1sKrkrAudioConsumedV1,
) -> i32;
type RuntimeIsExitRequestedFn = unsafe extern "C" fn(runtime: Art3m1sKrkrRuntimeHandle) -> i32;
type RuntimeSetExternalSurfaceFn = unsafe extern "C" fn(
    runtime: Art3m1sKrkrRuntimeHandle,
    kind: i32,
    handle: *mut c_void,
    width: u32,
    height: u32,
) -> i32;

#[repr(C)]
pub struct Art3M1sKrkrApiV1 {
    pub struct_size: u32,
    pub abi_version: u32,
    pub magic: u64,

    pub probe_project: Option<ProbeProjectFn>,
    pub runtime_create: Option<RuntimeCreateFn>,
    pub runtime_destroy: Option<RuntimeDestroyFn>,
    pub runtime_stage_width: Option<RuntimeStageFn>,
    pub runtime_stage_height: Option<RuntimeStageFn>,
    pub runtime_pixel_buffer_size: Option<RuntimePixelBufferSizeFn>,
    pub runtime_push_input: Option<RuntimePushInputFn>,
    pub runtime_tick: Option<RuntimeTickFn>,
    pub runtime_acquire_frame: Option<RuntimeAcquireFrameFn>,
    pub runtime_release_frame: Option<RuntimeReleaseFrameFn>,
    pub runtime_poll_audio_command: Option<RuntimePollAudioCommandFn>,
    pub runtime_submit_audio_consumed: Option<RuntimeSubmitAudioConsumedFn>,
    pub runtime_is_exit_requested: Option<RuntimeIsExitRequestedFn>,
    pub runtime_set_external_surface: Option<RuntimeSetExternalSurfaceFn>,
}

pub fn validate_api_v1(api: &Art3M1sKrkrApiV1) -> Result<(), KrkrAbiError> {
    let expected = std::mem::size_of::<Art3M1sKrkrApiV1>() as u32;
    if api.struct_size != expected {
        return Err(KrkrAbiError::SizeMismatch {
            expected,
            actual: api.struct_size,
        });
    }
    if api.abi_version != ART3M1S_KRKR_API_ABI_VERSION {
        return Err(KrkrAbiError::VersionMismatch {
            expected: ART3M1S_KRKR_API_ABI_VERSION,
            actual: api.abi_version,
        });
    }
    if api.magic != ART3M1S_KRKR_API_ABI_MAGIC {
        return Err(KrkrAbiError::MagicMismatch {
            expected: ART3M1S_KRKR_API_ABI_MAGIC,
            actual: api.magic,
        });
    }

    macro_rules! require {
        ($field:ident, $name:literal) => {
            if api.$field.is_none() {
                return Err(KrkrAbiError::MissingRequiredFunction($name));
            }
        };
    }

    require!(probe_project, "probe_project");
    require!(runtime_create, "runtime_create");
    require!(runtime_destroy, "runtime_destroy");
    require!(runtime_stage_width, "runtime_stage_width");
    require!(runtime_stage_height, "runtime_stage_height");
    require!(runtime_pixel_buffer_size, "runtime_pixel_buffer_size");
    require!(runtime_push_input, "runtime_push_input");
    require!(runtime_tick, "runtime_tick");
    require!(runtime_acquire_frame, "runtime_acquire_frame");
    require!(runtime_release_frame, "runtime_release_frame");
    require!(runtime_poll_audio_command, "runtime_poll_audio_command");
    require!(
        runtime_submit_audio_consumed,
        "runtime_submit_audio_consumed"
    );
    require!(runtime_is_exit_requested, "runtime_is_exit_requested");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn probe_project(_root: *const c_char, _out: *mut Art3m1sKrkrProbeV1) -> i32 {
        ART3M1S_KRKR_API_STATUS_UNSUPPORTED
    }

    unsafe extern "C" fn runtime_create(
        _root: *const c_char,
        _save_root: *const c_char,
        _config: *const Art3m1sKrkrRuntimeConfigV1,
        _out: *mut Art3m1sKrkrRuntimeHandle,
    ) -> i32 {
        ART3M1S_KRKR_API_STATUS_UNSUPPORTED
    }

    unsafe extern "C" fn runtime_destroy(_runtime: Art3m1sKrkrRuntimeHandle) {}

    unsafe extern "C" fn runtime_stage(_runtime: Art3m1sKrkrRuntimeHandle) -> u32 {
        0
    }

    unsafe extern "C" fn runtime_pixel_buffer_size(_runtime: Art3m1sKrkrRuntimeHandle) -> u32 {
        0
    }

    unsafe extern "C" fn runtime_push_input(
        _runtime: Art3m1sKrkrRuntimeHandle,
        _events: *const Art3m1sKrkrInputEventV1,
        _event_count: usize,
    ) -> i32 {
        ART3M1S_KRKR_API_STATUS_UNSUPPORTED
    }

    unsafe extern "C" fn runtime_tick(_runtime: Art3m1sKrkrRuntimeHandle) -> i32 {
        ART3M1S_KRKR_API_STATUS_UNSUPPORTED
    }

    unsafe extern "C" fn runtime_acquire_frame(
        _runtime: Art3m1sKrkrRuntimeHandle,
        _frame: *mut Art3m1sKrkrFrameV1,
    ) -> i32 {
        ART3M1S_KRKR_API_STATUS_UNSUPPORTED
    }

    unsafe extern "C" fn runtime_release_frame(
        _runtime: Art3m1sKrkrRuntimeHandle,
        _frame_id: u64,
    ) -> i32 {
        ART3M1S_KRKR_API_STATUS_UNSUPPORTED
    }

    unsafe extern "C" fn runtime_poll_audio_command(
        _runtime: Art3m1sKrkrRuntimeHandle,
        _command: *mut Art3m1sKrkrAudioCommandV1,
    ) -> i32 {
        ART3M1S_KRKR_API_STATUS_UNSUPPORTED
    }

    unsafe extern "C" fn runtime_submit_audio_consumed(
        _runtime: Art3m1sKrkrRuntimeHandle,
        _consumed: *const Art3m1sKrkrAudioConsumedV1,
    ) -> i32 {
        ART3M1S_KRKR_API_STATUS_UNSUPPORTED
    }

    unsafe extern "C" fn runtime_is_exit_requested(_runtime: Art3m1sKrkrRuntimeHandle) -> i32 {
        0
    }

    fn valid_api() -> Art3M1sKrkrApiV1 {
        Art3M1sKrkrApiV1 {
            struct_size: std::mem::size_of::<Art3M1sKrkrApiV1>() as u32,
            abi_version: ART3M1S_KRKR_API_ABI_VERSION,
            magic: ART3M1S_KRKR_API_ABI_MAGIC,
            probe_project: Some(probe_project),
            runtime_create: Some(runtime_create),
            runtime_destroy: Some(runtime_destroy),
            runtime_stage_width: Some(runtime_stage),
            runtime_stage_height: Some(runtime_stage),
            runtime_pixel_buffer_size: Some(runtime_pixel_buffer_size),
            runtime_push_input: Some(runtime_push_input),
            runtime_tick: Some(runtime_tick),
            runtime_acquire_frame: Some(runtime_acquire_frame),
            runtime_release_frame: Some(runtime_release_frame),
            runtime_poll_audio_command: Some(runtime_poll_audio_command),
            runtime_submit_audio_consumed: Some(runtime_submit_audio_consumed),
            runtime_is_exit_requested: Some(runtime_is_exit_requested),
            runtime_set_external_surface: None,
        }
    }

    #[test]
    fn validates_complete_v1_table() {
        assert_eq!(validate_api_v1(&valid_api()), Ok(()));
    }

    #[test]
    fn rejects_missing_required_function() {
        let mut api = valid_api();
        api.runtime_tick = None;
        assert_eq!(
            validate_api_v1(&api),
            Err(KrkrAbiError::MissingRequiredFunction("runtime_tick"))
        );
    }
}
