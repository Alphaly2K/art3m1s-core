//! Versioned POD protocol shared with the native KRKR host shim.
//!
//! Handles are opaque integers. Pixel and PCM data use borrowed pointer plus
//! length pairs and are only valid for the operation that supplied them.

use std::ffi::{c_char, c_void};

pub const ART3M1S_KRKR_STATUS_OK: i32 = 0;
pub const ART3M1S_KRKR_STATUS_NO_FRAME: i32 = 1;
pub const ART3M1S_KRKR_STATUS_NO_COMMAND: i32 = 2;
pub const ART3M1S_KRKR_STATUS_INVALID_ARGUMENT: i32 = -1;
pub const ART3M1S_KRKR_STATUS_INVALID_HANDLE: i32 = -2;
pub const ART3M1S_KRKR_STATUS_ENGINE: i32 = -3;
pub const ART3M1S_KRKR_STATUS_UNSUPPORTED: i32 = -4;
pub const ART3M1S_KRKR_STATUS_OUT_OF_MEMORY: i32 = -5;

pub const ART3M1S_KRKR_PROBE_DATA_XP3: u32 = 1;
pub const ART3M1S_KRKR_PROBE_ROOT_XP3: u32 = 2;
pub const ART3M1S_KRKR_PROBE_STARTUP_TJS: u32 = 3;
pub const ART3M1S_KRKR_PROBE_SYSTEM_INITIALIZE_TJS: u32 = 4;

pub const ART3M1S_KRKR_INPUT_KEY: u32 = 1;
pub const ART3M1S_KRKR_INPUT_TEXT: u32 = 2;
pub const ART3M1S_KRKR_INPUT_POINTER_MOVE: u32 = 3;
pub const ART3M1S_KRKR_INPUT_POINTER_BUTTON: u32 = 4;
pub const ART3M1S_KRKR_INPUT_WHEEL: u32 = 5;
pub const ART3M1S_KRKR_INPUT_FOCUS: u32 = 6;
pub const ART3M1S_KRKR_INPUT_QUIT: u32 = 7;

pub const ART3M1S_KRKR_INPUT_PHASE_DOWN: u32 = 0;
pub const ART3M1S_KRKR_INPUT_PHASE_UP: u32 = 1;
pub const ART3M1S_KRKR_INPUT_PHASE_REPEAT: u32 = 2;
pub const ART3M1S_KRKR_INPUT_PHASE_MOVE: u32 = 3;

pub const ART3M1S_KRKR_POINTER_LEFT: u32 = 1 << 0;
pub const ART3M1S_KRKR_POINTER_RIGHT: u32 = 1 << 1;
pub const ART3M1S_KRKR_POINTER_MIDDLE: u32 = 1 << 2;
pub const ART3M1S_KRKR_POINTER_X1: u32 = 1 << 3;
pub const ART3M1S_KRKR_POINTER_X2: u32 = 1 << 4;

pub const ART3M1S_KRKR_FRAME_FORMAT_RGBA8: u32 = 1;

pub const ART3M1S_KRKR_AUDIO_CREATE_STREAM: u32 = 1;
pub const ART3M1S_KRKR_AUDIO_SUBMIT_PCM: u32 = 2;
pub const ART3M1S_KRKR_AUDIO_PLAY: u32 = 3;
pub const ART3M1S_KRKR_AUDIO_PAUSE: u32 = 4;
pub const ART3M1S_KRKR_AUDIO_STOP: u32 = 5;
pub const ART3M1S_KRKR_AUDIO_SET_PARAMS: u32 = 6;
pub const ART3M1S_KRKR_AUDIO_DESTROY_STREAM: u32 = 7;
pub const ART3M1S_KRKR_AUDIO_MASTER_VOLUME: u32 = 8;

pub const ART3M1S_KRKR_AUDIO_FORMAT_I16: u32 = 1;
pub const ART3M1S_KRKR_AUDIO_FORMAT_F32: u32 = 2;
pub const ART3M1S_KRKR_AUDIO_FORMAT_I8: u32 = 3;
pub const ART3M1S_KRKR_AUDIO_FORMAT_I24: u32 = 4;
pub const ART3M1S_KRKR_AUDIO_FORMAT_I32: u32 = 5;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Art3m1sKrkrProbeV1 {
    pub struct_size: u32,
    pub flags: u32,
    pub preferred_kind: u32,
    pub has_data_xp3: u32,
    pub root_xp3_count: u32,
    pub has_startup_tjs: u32,
    pub has_patch_tjs: u32,
    pub has_system_initialize_tjs: u32,
    pub reserved: [u64; 4],
}

impl Art3m1sKrkrProbeV1 {
    pub fn new() -> Self {
        Self {
            struct_size: std::mem::size_of::<Self>() as u32,
            ..Self::default()
        }
    }

    pub fn is_krkr(self) -> bool {
        self.preferred_kind != 0
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Art3m1sKrkrRuntimeConfigV1 {
    pub struct_size: u32,
    pub flags: u32,
    pub width: u32,
    pub height: u32,
    pub audio_sample_rate: u32,
    pub audio_channels: u32,
    pub reserved: [u64; 4],
}

impl Art3m1sKrkrRuntimeConfigV1 {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            struct_size: std::mem::size_of::<Self>() as u32,
            width,
            height,
            audio_sample_rate: 48_000,
            audio_channels: 2,
            ..Self::default()
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Art3m1sKrkrInputEventV1 {
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

impl Art3m1sKrkrInputEventV1 {
    pub fn new(kind: u32) -> Self {
        Self {
            struct_size: std::mem::size_of::<Self>() as u32,
            kind,
            ..Self::default()
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Art3m1sKrkrFrameV1 {
    pub struct_size: u32,
    pub format: u32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub flags: u32,
    pub frame_id: u64,
    pub generation: u64,
    /// Native-owned RGBA8 pixels. Valid until `runtime_release_frame`.
    pub pixels: *const u8,
    pub pixels_len: usize,
    pub reserved: [u64; 2],
}

impl Default for Art3m1sKrkrFrameV1 {
    fn default() -> Self {
        Self {
            struct_size: std::mem::size_of::<Self>() as u32,
            format: ART3M1S_KRKR_FRAME_FORMAT_RGBA8,
            width: 0,
            height: 0,
            stride: 0,
            flags: 0,
            frame_id: 0,
            generation: 0,
            pixels: std::ptr::null(),
            pixels_len: 0,
            reserved: [0; 2],
        }
    }
}

impl Art3m1sKrkrFrameV1 {
    pub fn is_empty(self) -> bool {
        self.pixels.is_null() || self.width == 0 || self.height == 0
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Art3m1sKrkrAudioCommandV1 {
    pub struct_size: u32,
    pub kind: u32,
    pub stream_id: u32,
    pub sample_format: u32,
    pub sample_rate: u32,
    pub channels: u32,
    /// Per-channel sample frames. For `SUBMIT_PCM`, payload size must equal
    /// `sample_count * channels * bytes_per_sample`.
    pub sample_count: u64,
    pub volume: f32,
    pub pan: f32,
    pub payload: *const u8,
    pub payload_size: usize,
    pub reserved: [u64; 2],
}

impl Default for Art3m1sKrkrAudioCommandV1 {
    fn default() -> Self {
        Self {
            struct_size: std::mem::size_of::<Self>() as u32,
            kind: 0,
            stream_id: 0,
            sample_format: 0,
            sample_rate: 0,
            channels: 0,
            sample_count: 0,
            volume: 1.0,
            pan: 0.0,
            payload: std::ptr::null(),
            payload_size: 0,
            reserved: [0; 2],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Art3m1sKrkrAudioConsumedV1 {
    pub struct_size: u32,
    pub stream_id: u32,
    /// Absolute number of decoded sample frames consumed since stream creation
    /// or the most recent stop/reset.
    pub consumed_samples: u64,
    pub generation: u64,
    pub reserved: [u64; 2],
}

impl Art3m1sKrkrAudioConsumedV1 {
    pub fn new(stream_id: u32, consumed_samples: u64, generation: u64) -> Self {
        Self {
            struct_size: std::mem::size_of::<Self>() as u32,
            stream_id,
            consumed_samples,
            generation,
            reserved: [0; 2],
        }
    }
}

pub type Art3m1sKrkrRuntimeHandle = u64;
pub type Art3m1sKrkrFrameHandle = u64;

#[allow(dead_code)]
type KeepCCharImported = *const c_char;
#[allow(dead_code)]
type KeepCVoidImported = *mut c_void;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_structs_are_sized_and_null_initialized() {
        let probe = Art3m1sKrkrProbeV1::new();
        assert_eq!(
            probe.struct_size as usize,
            std::mem::size_of::<Art3m1sKrkrProbeV1>()
        );
        assert!(!probe.is_krkr());

        let config = Art3m1sKrkrRuntimeConfigV1::new(1280, 720);
        assert_eq!(config.width, 1280);
        assert_eq!(config.height, 720);
        assert_eq!(config.audio_sample_rate, 48_000);
        assert_eq!(config.audio_channels, 2);

        let event = Art3m1sKrkrInputEventV1::new(ART3M1S_KRKR_INPUT_KEY);
        assert_eq!(event.kind, ART3M1S_KRKR_INPUT_KEY);
        assert_eq!(
            event.struct_size as usize,
            std::mem::size_of::<Art3m1sKrkrInputEventV1>()
        );
    }

    #[test]
    fn frame_uses_native_borrowed_pixels() {
        let frame = Art3m1sKrkrFrameV1::default();
        assert_eq!(frame.format, ART3M1S_KRKR_FRAME_FORMAT_RGBA8);
        assert!(frame.is_empty());
    }
}
