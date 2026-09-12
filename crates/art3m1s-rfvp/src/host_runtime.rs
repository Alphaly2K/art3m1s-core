//! Rust-side RFVP host runtime.
//!
//! This module owns the native resources/runtime handles and submits frames to
//! an `art3m1s-render` backend. No draw-command or texture data crosses into
//! Dart, and no FFI type is exposed by this API.

use std::ffi::c_void;
use std::fmt;
use std::path::Path;
use std::ptr;

use art3m1s_render::{Extent2D, GpuBackend, NativeSurface};
use rfvp::host_abi::runtime::{
    rfvp_frame_get_commands, rfvp_frame_get_hit_proxies, rfvp_frame_get_size,
    rfvp_frame_get_textures, rfvp_frame_release, rfvp_resources_create, rfvp_resources_destroy,
    rfvp_resources_mount_directory, rfvp_resources_set_save_root, rfvp_runtime_acquire_frame,
    rfvp_runtime_capabilities, rfvp_runtime_create, rfvp_runtime_destroy,
    rfvp_runtime_is_exit_requested, rfvp_runtime_poll_audio_command, rfvp_runtime_push_input,
    rfvp_runtime_step,
};
use rfvp::host_abi::v1::{
    RFVP_AUDIO_CREATE_STREAM, RFVP_AUDIO_DESTROY_STREAM, RFVP_AUDIO_ENCODED_FLAC,
    RFVP_AUDIO_ENCODED_MP3, RFVP_AUDIO_ENCODED_OGG, RFVP_AUDIO_ENCODED_WAV,
    RFVP_AUDIO_LOAD_ENCODED, RFVP_AUDIO_MASTER_VOLUME, RFVP_AUDIO_PAUSE, RFVP_AUDIO_PLAY,
    RFVP_AUDIO_RESUME, RFVP_AUDIO_SAMPLE_F32, RFVP_AUDIO_SAMPLE_I16, RFVP_AUDIO_SET_PARAMS,
    RFVP_AUDIO_STOP, RFVP_AUDIO_SUBMIT_F32, RFVP_AUDIO_SUBMIT_I16, RFVP_BLEND_ADD,
    RFVP_BLEND_MULTIPLY, RFVP_BLEND_NORMAL, RFVP_BLEND_REVERSE_SUBTRACT, RFVP_DRAW_FLAG_HAS_EFFECT,
    RFVP_DRAW_FLAG_HAS_MESH, RFVP_DRAW_GLYPH, RFVP_DRAW_IMAGE, RFVP_DRAW_SOLID,
    RFVP_HIT_PROXY_ENABLED, RFVP_HIT_PROXY_VISIBLE, RFVP_INPUT_FOCUS, RFVP_INPUT_KEY,
    RFVP_INPUT_PHASE_DOWN, RFVP_INPUT_PHASE_MOVE, RFVP_INPUT_PHASE_REPEAT, RFVP_INPUT_PHASE_UP,
    RFVP_INPUT_POINTER_BUTTON, RFVP_INPUT_POINTER_MOVE, RFVP_INPUT_QUIT, RFVP_INPUT_TEXT,
    RFVP_INPUT_TOUCH, RFVP_INPUT_WHEEL, RFVP_NLS_GBK, RFVP_NLS_SHIFT_JIS, RFVP_NLS_UTF8,
    RFVP_POINTER_LEFT, RFVP_POINTER_MIDDLE, RFVP_POINTER_RIGHT, RFVP_STATUS_NO_COMMAND,
    RFVP_STATUS_NO_FRAME, RFVP_STATUS_OK, RFVP_TEXTURE_CREATE, RFVP_TEXTURE_DESTROY,
    RFVP_TEXTURE_FORMAT_LUMA_A8, RFVP_TEXTURE_FORMAT_RGBA8, RFVP_TEXTURE_UPDATE,
    RfvpAudioCommandV1, RfvpDrawCommandV1, RfvpHitProxyV1, RfvpInputEventV1, RfvpResourcesConfigV1,
    RfvpRuntimeConfigV1, RfvpTextureCommandV1,
};
use rfvp::host_api::{
    CommandBlendMode, DrawGlyphCmd, DrawImageCmd, HitProxy, HitProxyTable, PortableTextureDesc,
    RectI16, RectU16, RenderCommand, RenderFrame, Rgba8, TextureFormat, TextureHandle, Vertex2D,
};
use rfvp::rendering::external::{
    ExternalFrame, RecordedTextureCommand, RecordedTextureCreate, RecordedTextureDestroy,
    RecordedTextureUpdate,
};

use crate::{ExternalRenderer, ExternalRendererError, RfvpRenderResult};

/// Native NLS used while mounting an RFVP project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RfvpNls {
    ShiftJis,
    Gbk,
    Utf8,
}

impl RfvpNls {
    const fn abi_value(self) -> u32 {
        match self {
            Self::ShiftJis => RFVP_NLS_SHIFT_JIS,
            Self::Gbk => RFVP_NLS_GBK,
            Self::Utf8 => RFVP_NLS_UTF8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RfvpPointerButton {
    Left,
    Right,
    Middle,
}

impl RfvpPointerButton {
    const fn abi_value(self) -> u32 {
        match self {
            Self::Left => RFVP_POINTER_LEFT,
            Self::Right => RFVP_POINTER_RIGHT,
            Self::Middle => RFVP_POINTER_MIDDLE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RfvpTouchPhase {
    Down,
    Move,
    Up,
}

impl RfvpTouchPhase {
    const fn abi_value(self) -> u32 {
        match self {
            Self::Down => RFVP_INPUT_PHASE_DOWN,
            Self::Move => RFVP_INPUT_PHASE_MOVE,
            Self::Up => RFVP_INPUT_PHASE_UP,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RfvpHostInputEvent {
    Key {
        code: u32,
        pressed: bool,
        repeat: bool,
        modifiers: u32,
    },
    Text {
        character: char,
    },
    PointerMove {
        x: i32,
        y: i32,
    },
    PointerButton {
        button: RfvpPointerButton,
        pressed: bool,
        x: i32,
        y: i32,
    },
    Wheel {
        delta_x: i32,
        delta_y: i32,
    },
    Touch {
        id: u64,
        phase: RfvpTouchPhase,
        x: i32,
        y: i32,
    },
    Focus {
        focused: bool,
    },
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RfvpHostAudioCommandKind {
    LoadEncoded,
    CreateStream,
    SubmitI16,
    SubmitF32,
    Play,
    Stop,
    Pause,
    Resume,
    SetParams,
    DestroyStream,
    MasterVolume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RfvpAudioSampleFormat {
    I16,
    F32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RfvpEncodedAudioKind {
    Unknown,
    Wav,
    Ogg,
    Mp3,
    Flac,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RfvpHostAudioCommand {
    pub kind: RfvpHostAudioCommandKind,
    pub stream_id: u32,
    pub sample_format: Option<RfvpAudioSampleFormat>,
    pub encoded_kind: Option<RfvpEncodedAudioKind>,
    pub sample_rate: u32,
    pub channels: u32,
    pub repeat: bool,
    pub fade_ms: u32,
    pub volume: f32,
    pub pan: f32,
    pub sample_count: usize,
    pub payload: Vec<u8>,
}

#[derive(Debug)]
pub enum RfvpHostRuntimeError {
    InvalidPath,
    ResourcesCreate(i32),
    MountDirectory(i32),
    SetSaveRoot(i32),
    RuntimeCreate(i32),
    Step(i32),
    FrameAcquire(i32),
    FrameRead(i32),
    UnsupportedDrawKind(u32),
    UnsupportedBlend(u32),
    UnsupportedTextureKind(u32),
    UnsupportedTextureFormat(u32),
    UnsupportedEffect(u32),
    UnsupportedMesh,
    RectOverflow,
    HitProxyOverflow,
    InputRejected(i32),
    AudioCommandRead(i32),
    UnsupportedAudioCommandKind(u32),
    InvalidAudioPayload,
    Surface(String),
    Present(String),
    Render(ExternalRendererError),
}

impl fmt::Display for RfvpHostRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath => write!(f, "RFVP path is not valid UTF-8"),
            Self::ResourcesCreate(status) => {
                write!(f, "RFVP resources create failed with status {status}")
            }
            Self::MountDirectory(status) => {
                write!(f, "RFVP directory mount failed with status {status}")
            }
            Self::SetSaveRoot(status) => {
                write!(f, "RFVP save root update failed with status {status}")
            }
            Self::RuntimeCreate(status) => {
                write!(f, "RFVP runtime create failed with status {status}")
            }
            Self::Step(status) => write!(f, "RFVP runtime step failed with status {status}"),
            Self::FrameAcquire(status) => {
                write!(f, "RFVP frame acquire failed with status {status}")
            }
            Self::FrameRead(status) => write!(f, "RFVP frame read failed with status {status}"),
            Self::UnsupportedDrawKind(kind) => write!(f, "unsupported RFVP draw kind {kind}"),
            Self::UnsupportedBlend(blend) => write!(f, "unsupported RFVP blend mode {blend}"),
            Self::UnsupportedTextureKind(kind) => {
                write!(f, "unsupported RFVP texture command kind {kind}")
            }
            Self::UnsupportedTextureFormat(format) => {
                write!(f, "unsupported RFVP texture format {format}")
            }
            Self::UnsupportedEffect(effect) => {
                write!(f, "unsupported RFVP effect id {effect}")
            }
            Self::UnsupportedMesh => write!(f, "RFVP mesh commands are not supported yet"),
            Self::RectOverflow => write!(f, "RFVP rectangle exceeds the native ABI range"),
            Self::HitProxyOverflow => write!(f, "RFVP hit proxy exceeds the native ABI range"),
            Self::InputRejected(status) => {
                write!(f, "RFVP input was rejected with status {status}")
            }
            Self::AudioCommandRead(status) => {
                write!(f, "RFVP audio command read failed with status {status}")
            }
            Self::UnsupportedAudioCommandKind(kind) => {
                write!(f, "unsupported RFVP audio command kind {kind}")
            }
            Self::InvalidAudioPayload => write!(f, "RFVP audio command has an invalid payload"),
            Self::Surface(error) => write!(f, "RFVP native surface failed: {error}"),
            Self::Present(error) => write!(f, "RFVP frame presentation failed: {error}"),
            Self::Render(error) => fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for RfvpHostRuntimeError {}

impl From<ExternalRendererError> for RfvpHostRuntimeError {
    fn from(error: ExternalRendererError) -> Self {
        Self::Render(error)
    }
}

/// Owns one RFVP resources/runtime pair and renders its frames in Rust.
///
/// The type is intentionally not `Send`: RFVP's v1 ABI state is thread-local
/// and requires resources, runtime, and frame operations to stay on the
/// creating thread.
pub struct RfvpHostRuntime {
    resources: u64,
    runtime: u64,
    width: u32,
    height: u32,
    last_command_count: usize,
    last_texture_count: usize,
    exit_requested: bool,
    renderer: ExternalRenderer,
}

impl RfvpHostRuntime {
    /// Creates a runtime and mounts a directory-backed project.
    pub fn new_directory(
        game_root: impl AsRef<Path>,
        save_root: Option<&Path>,
        width: u32,
        height: u32,
        nls: RfvpNls,
        backend: Box<dyn GpuBackend>,
        clear_color: [f32; 4],
    ) -> Result<Self, RfvpHostRuntimeError> {
        let game_root = game_root
            .as_ref()
            .to_str()
            .ok_or(RfvpHostRuntimeError::InvalidPath)?;
        let save_root = save_root
            .map(|path| path.to_str().ok_or(RfvpHostRuntimeError::InvalidPath))
            .transpose()?;
        let save_root_bytes = save_root.map(str::as_bytes);

        let resources_config = RfvpResourcesConfigV1 {
            struct_size: std::mem::size_of::<RfvpResourcesConfigV1>() as u32,
            flags: 0,
            nls: nls.abi_value(),
            reserved0: 0,
            save_root_utf8: save_root_bytes.map_or(ptr::null(), |bytes| bytes.as_ptr()),
            save_root_len: save_root_bytes.map_or(0, <[u8]>::len),
            reserved: [0; 4],
        };

        let mut resources = 0u64;
        let status = unsafe { rfvp_resources_create(&resources_config, &mut resources) };
        if status != RFVP_STATUS_OK {
            return Err(RfvpHostRuntimeError::ResourcesCreate(status));
        }

        let result = (|| {
            let path = game_root.as_bytes();
            let status =
                unsafe { rfvp_resources_mount_directory(resources, path.as_ptr(), path.len()) };
            if status != RFVP_STATUS_OK {
                return Err(RfvpHostRuntimeError::MountDirectory(status));
            }
            if let Some(save_root) = save_root_bytes {
                let status = unsafe {
                    rfvp_resources_set_save_root(resources, save_root.as_ptr(), save_root.len())
                };
                if status != RFVP_STATUS_OK {
                    return Err(RfvpHostRuntimeError::SetSaveRoot(status));
                }
            }

            let runtime_config = RfvpRuntimeConfigV1 {
                struct_size: std::mem::size_of::<RfvpRuntimeConfigV1>() as u32,
                flags: 0,
                resources,
                requested_width: width,
                requested_height: height,
                reserved: [0; 4],
            };
            let mut runtime = 0u64;
            let status = unsafe { rfvp_runtime_create(&runtime_config, &mut runtime) };
            if status != RFVP_STATUS_OK {
                return Err(RfvpHostRuntimeError::RuntimeCreate(status));
            }
            Ok((runtime, width, height))
        })();

        let (runtime, width, height) = match result {
            Ok(result) => result,
            Err(error) => {
                unsafe { rfvp_resources_destroy(resources) };
                return Err(error);
            }
        };

        Ok(Self {
            resources,
            runtime,
            width,
            height,
            last_command_count: 0,
            last_texture_count: 0,
            exit_requested: false,
            renderer: ExternalRenderer::new(backend, clear_color),
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn last_command_count(&self) -> usize {
        self.last_command_count
    }

    pub fn last_texture_count(&self) -> usize {
        self.last_texture_count
    }

    pub fn capabilities(&mut self) -> u64 {
        unsafe { rfvp_runtime_capabilities(self.runtime) }
    }

    pub fn is_exit_requested(&mut self) -> bool {
        if self.exit_requested {
            return true;
        }
        self.exit_requested = unsafe { rfvp_runtime_is_exit_requested(self.runtime) != 0 };
        self.exit_requested
    }

    pub fn step(&mut self, delta_ms: u32) -> Result<(), RfvpHostRuntimeError> {
        let status = unsafe { rfvp_runtime_step(self.runtime, delta_ms.max(1)) };
        if status != RFVP_STATUS_OK {
            return Err(RfvpHostRuntimeError::Step(status));
        }
        self.is_exit_requested();
        Ok(())
    }

    pub fn push_input(
        &mut self,
        events: &[RfvpHostInputEvent],
    ) -> Result<(), RfvpHostRuntimeError> {
        if events.is_empty() {
            return Ok(());
        }
        let events: Vec<RfvpInputEventV1> = events.iter().copied().map(input_event).collect();
        let status =
            unsafe { rfvp_runtime_push_input(self.runtime, events.as_ptr(), events.len()) };
        if status != RFVP_STATUS_OK {
            return Err(RfvpHostRuntimeError::InputRejected(status));
        }
        Ok(())
    }

    pub fn poll_audio_command(
        &mut self,
    ) -> Result<Option<RfvpHostAudioCommand>, RfvpHostRuntimeError> {
        let mut command = RfvpAudioCommandV1 {
            struct_size: std::mem::size_of::<RfvpAudioCommandV1>() as u32,
            kind: 0,
            stream_id: 0,
            sample_format: 0,
            encoded_kind: 0,
            sample_rate: 0,
            channels: 0,
            repeat: 0,
            fade_ms: 0,
            volume: 1.0,
            pan: 0.0,
            sample_count: 0,
            payload: ptr::null(),
            payload_size: 0,
            reserved: [0; 2],
        };
        let status = unsafe { rfvp_runtime_poll_audio_command(self.runtime, &mut command) };
        if status == RFVP_STATUS_NO_COMMAND {
            return Ok(None);
        }
        if status != RFVP_STATUS_OK {
            return Err(RfvpHostRuntimeError::AudioCommandRead(status));
        }
        Ok(Some(convert_audio_command(&command)?))
    }

    pub fn drain_audio_commands(
        &mut self,
        max_commands: usize,
    ) -> Result<Vec<RfvpHostAudioCommand>, RfvpHostRuntimeError> {
        let mut commands = Vec::new();
        while commands.len() < max_commands {
            let Some(command) = self.poll_audio_command()? else {
                break;
            };
            commands.push(command);
        }
        Ok(commands)
    }

    pub fn set_native_surface(
        &mut self,
        kind: i32,
        handle: *mut c_void,
        width: u32,
        height: u32,
    ) -> Result<(), RfvpHostRuntimeError> {
        let surface = NativeSurface::from_legacy_parts(kind, handle, width, height)
            .map_err(RfvpHostRuntimeError::Surface)?;
        self.renderer
            .backend_mut()
            .set_native_surface(surface)
            .map_err(RfvpHostRuntimeError::Surface)
    }

    pub fn clear_native_surface(&mut self) {
        self.renderer.backend_mut().clear_native_surface();
    }

    /// Acquires the pending frame, renders it through `GpuBackend`, and
    /// returns the submitted damage region. Returns `None` when no frame is
    /// pending.
    pub fn render_pending_frame(
        &mut self,
    ) -> Result<Option<RfvpRenderResult>, RfvpHostRuntimeError> {
        let mut native_frame = 0u64;
        let status = unsafe { rfvp_runtime_acquire_frame(self.runtime, &mut native_frame) };
        if status == RFVP_STATUS_NO_FRAME {
            return Ok(None);
        }
        if status != RFVP_STATUS_OK {
            return Err(RfvpHostRuntimeError::FrameAcquire(status));
        }

        let frame = self.read_frame(native_frame);
        unsafe { rfvp_frame_release(native_frame) };
        let frame = frame?;
        let result = self.renderer.render_frame(&frame)?;
        Ok(Some(result))
    }

    pub fn advance_and_present(&mut self, delta_ms: u32) -> Result<bool, RfvpHostRuntimeError> {
        self.step(delta_ms)?;
        let Some(result) = self.render_pending_frame()? else {
            return Ok(false);
        };
        self.renderer
            .backend_mut()
            .present(result.region.damage())
            .map_err(RfvpHostRuntimeError::Present)?;
        Ok(true)
    }

    pub fn readback_rgba(&mut self) -> Result<Vec<u8>, RfvpHostRuntimeError> {
        self.renderer
            .readback_rgba(Extent2D::new(self.width, self.height))
            .map_err(Into::into)
    }

    fn read_frame(&mut self, native_frame: u64) -> Result<ExternalFrame, RfvpHostRuntimeError> {
        let mut width = 0u32;
        let mut height = 0u32;
        let status = unsafe { rfvp_frame_get_size(native_frame, &mut width, &mut height) };
        if status != RFVP_STATUS_OK {
            return Err(RfvpHostRuntimeError::FrameRead(status));
        }

        let mut commands_ptr = ptr::null::<RfvpDrawCommandV1>();
        let mut command_count = 0usize;
        let status =
            unsafe { rfvp_frame_get_commands(native_frame, &mut commands_ptr, &mut command_count) };
        if status != RFVP_STATUS_OK {
            return Err(RfvpHostRuntimeError::FrameRead(status));
        }

        let mut textures_ptr = ptr::null::<RfvpTextureCommandV1>();
        let mut texture_count = 0usize;
        let status =
            unsafe { rfvp_frame_get_textures(native_frame, &mut textures_ptr, &mut texture_count) };
        if status != RFVP_STATUS_OK {
            return Err(RfvpHostRuntimeError::FrameRead(status));
        }

        let mut hit_proxies_ptr = ptr::null::<RfvpHitProxyV1>();
        let mut hit_proxy_count = 0usize;
        let status = unsafe {
            rfvp_frame_get_hit_proxies(native_frame, &mut hit_proxies_ptr, &mut hit_proxy_count)
        };
        if status != RFVP_STATUS_OK {
            return Err(RfvpHostRuntimeError::FrameRead(status));
        }

        let commands = if command_count == 0 {
            Vec::new()
        } else {
            let commands = unsafe { std::slice::from_raw_parts(commands_ptr, command_count) };
            commands
                .iter()
                .map(convert_command)
                .collect::<Result<Vec<_>, _>>()?
        };

        let texture_commands = if texture_count == 0 {
            Vec::new()
        } else {
            let textures = unsafe { std::slice::from_raw_parts(textures_ptr, texture_count) };
            textures
                .iter()
                .map(convert_texture)
                .collect::<Result<Vec<_>, _>>()?
        };

        let hit_proxies = if hit_proxy_count == 0 {
            Vec::new()
        } else {
            let proxies = unsafe { std::slice::from_raw_parts(hit_proxies_ptr, hit_proxy_count) };
            proxies
                .iter()
                .map(convert_hit_proxy)
                .collect::<Result<Vec<_>, _>>()?
        };

        self.last_command_count = commands.len();
        self.last_texture_count = texture_commands.len();
        self.width = width;
        self.height = height;
        let textures = texture_commands
            .iter()
            .filter_map(|command| match command {
                RecordedTextureCommand::Create(texture) => Some(texture.clone()),
                RecordedTextureCommand::Update(_) | RecordedTextureCommand::Destroy(_) => None,
            })
            .collect();
        Ok(ExternalFrame {
            frame: RenderFrame {
                commands,
                hit_proxies: HitProxyTable {
                    proxies: hit_proxies,
                },
            },
            textures,
            texture_commands,
        })
    }
}

impl Drop for RfvpHostRuntime {
    fn drop(&mut self) {
        unsafe {
            rfvp_runtime_destroy(self.runtime);
            rfvp_resources_destroy(self.resources);
        }
    }
}

fn input_event(event: RfvpHostInputEvent) -> RfvpInputEventV1 {
    let mut native = RfvpInputEventV1 {
        struct_size: std::mem::size_of::<RfvpInputEventV1>() as u32,
        kind: 0,
        code: 0,
        phase: 0,
        x: 0,
        y: 0,
        value: 0,
        modifiers: 0,
        id: 0,
    };
    match event {
        RfvpHostInputEvent::Key {
            code,
            pressed,
            repeat,
            modifiers,
        } => {
            native.kind = RFVP_INPUT_KEY;
            native.code = code;
            native.phase = if !pressed {
                RFVP_INPUT_PHASE_UP
            } else if repeat {
                RFVP_INPUT_PHASE_REPEAT
            } else {
                RFVP_INPUT_PHASE_DOWN
            };
            native.modifiers = modifiers;
        }
        RfvpHostInputEvent::Text { character } => {
            native.kind = RFVP_INPUT_TEXT;
            native.code = u32::from(character);
        }
        RfvpHostInputEvent::PointerMove { x, y } => {
            native.kind = RFVP_INPUT_POINTER_MOVE;
            native.phase = RFVP_INPUT_PHASE_MOVE;
            native.x = x;
            native.y = y;
        }
        RfvpHostInputEvent::PointerButton {
            button,
            pressed,
            x,
            y,
        } => {
            native.kind = RFVP_INPUT_POINTER_BUTTON;
            native.code = button.abi_value();
            native.phase = if pressed {
                RFVP_INPUT_PHASE_DOWN
            } else {
                RFVP_INPUT_PHASE_UP
            };
            native.x = x;
            native.y = y;
        }
        RfvpHostInputEvent::Wheel { delta_x, delta_y } => {
            native.kind = RFVP_INPUT_WHEEL;
            native.x = delta_x;
            native.y = delta_y;
        }
        RfvpHostInputEvent::Touch { id, phase, x, y } => {
            native.kind = RFVP_INPUT_TOUCH;
            native.phase = phase.abi_value();
            native.x = x;
            native.y = y;
            native.id = id;
        }
        RfvpHostInputEvent::Focus { focused } => {
            native.kind = RFVP_INPUT_FOCUS;
            native.phase = u32::from(focused);
        }
        RfvpHostInputEvent::Quit => {
            native.kind = RFVP_INPUT_QUIT;
        }
    }
    native
}

fn convert_audio_command(
    command: &RfvpAudioCommandV1,
) -> Result<RfvpHostAudioCommand, RfvpHostRuntimeError> {
    if (command.struct_size as usize) < std::mem::size_of::<RfvpAudioCommandV1>() {
        return Err(RfvpHostRuntimeError::InvalidAudioPayload);
    }
    let payload = if command.payload_size == 0 {
        Vec::new()
    } else {
        if command.payload.is_null() {
            return Err(RfvpHostRuntimeError::InvalidAudioPayload);
        }
        unsafe { std::slice::from_raw_parts(command.payload, command.payload_size).to_vec() }
    };
    let kind = match command.kind {
        RFVP_AUDIO_LOAD_ENCODED => RfvpHostAudioCommandKind::LoadEncoded,
        RFVP_AUDIO_CREATE_STREAM => RfvpHostAudioCommandKind::CreateStream,
        RFVP_AUDIO_SUBMIT_I16 => RfvpHostAudioCommandKind::SubmitI16,
        RFVP_AUDIO_SUBMIT_F32 => RfvpHostAudioCommandKind::SubmitF32,
        RFVP_AUDIO_PLAY => RfvpHostAudioCommandKind::Play,
        RFVP_AUDIO_STOP => RfvpHostAudioCommandKind::Stop,
        RFVP_AUDIO_PAUSE => RfvpHostAudioCommandKind::Pause,
        RFVP_AUDIO_RESUME => RfvpHostAudioCommandKind::Resume,
        RFVP_AUDIO_SET_PARAMS => RfvpHostAudioCommandKind::SetParams,
        RFVP_AUDIO_DESTROY_STREAM => RfvpHostAudioCommandKind::DestroyStream,
        RFVP_AUDIO_MASTER_VOLUME => RfvpHostAudioCommandKind::MasterVolume,
        kind => return Err(RfvpHostRuntimeError::UnsupportedAudioCommandKind(kind)),
    };
    let sample_format = match command.sample_format {
        0 => None,
        RFVP_AUDIO_SAMPLE_I16 => Some(RfvpAudioSampleFormat::I16),
        RFVP_AUDIO_SAMPLE_F32 => Some(RfvpAudioSampleFormat::F32),
        _ => None,
    };
    let encoded_kind = match command.encoded_kind {
        0 => None,
        RFVP_AUDIO_ENCODED_WAV => Some(RfvpEncodedAudioKind::Wav),
        RFVP_AUDIO_ENCODED_OGG => Some(RfvpEncodedAudioKind::Ogg),
        RFVP_AUDIO_ENCODED_MP3 => Some(RfvpEncodedAudioKind::Mp3),
        RFVP_AUDIO_ENCODED_FLAC => Some(RfvpEncodedAudioKind::Flac),
        _ => None,
    };
    Ok(RfvpHostAudioCommand {
        kind,
        stream_id: command.stream_id,
        sample_format,
        encoded_kind,
        sample_rate: command.sample_rate,
        channels: command.channels,
        repeat: command.repeat != 0,
        fade_ms: command.fade_ms,
        volume: command.volume,
        pan: command.pan,
        sample_count: command.sample_count,
        payload,
    })
}

fn convert_command(command: &RfvpDrawCommandV1) -> Result<RenderCommand, RfvpHostRuntimeError> {
    if command.flags & RFVP_DRAW_FLAG_HAS_MESH != 0 || !command.mesh.is_null() {
        return Err(RfvpHostRuntimeError::UnsupportedMesh);
    }
    if command.flags & RFVP_DRAW_FLAG_HAS_EFFECT != 0 || command.effect_id != 0 {
        return Err(RfvpHostRuntimeError::UnsupportedEffect(command.effect_id));
    }

    let vertices = std::array::from_fn(|index| convert_vertex(&command.vertices[index]));
    match command.kind {
        RFVP_DRAW_IMAGE | RFVP_DRAW_SOLID => Ok(RenderCommand::DrawImage(DrawImageCmd {
            texture: TextureHandle(command.texture_id),
            src: RectU16 {
                x: command.src_rect.x,
                y: command.src_rect.y,
                w: command.src_rect.width,
                h: command.src_rect.height,
            },
            dst: convert_rect_i16(command.dst_rect)?,
            color: convert_rgba8(command.color),
            blend: convert_blend(command.blend)?,
            effect_id: 0,
            clip: if command.flags & 1 != 0 {
                Some(convert_rect_i16(command.clip_rect)?)
            } else {
                None
            },
            vertices,
        })),
        RFVP_DRAW_GLYPH => Ok(RenderCommand::DrawGlyph(DrawGlyphCmd {
            texture: TextureHandle(command.texture_id),
            src: RectU16 {
                x: command.src_rect.x,
                y: command.src_rect.y,
                w: command.src_rect.width,
                h: command.src_rect.height,
            },
            dst: convert_rect_i16(command.dst_rect)?,
            color: convert_rgba8(command.color),
            clip: if command.flags & 1 != 0 {
                Some(convert_rect_i16(command.clip_rect)?)
            } else {
                None
            },
        })),
        kind => Err(RfvpHostRuntimeError::UnsupportedDrawKind(kind)),
    }
}

fn convert_texture(
    texture: &RfvpTextureCommandV1,
) -> Result<RecordedTextureCommand, RfvpHostRuntimeError> {
    match texture.kind {
        RFVP_TEXTURE_CREATE => convert_texture_create(texture),
        RFVP_TEXTURE_UPDATE => convert_texture_update(texture),
        RFVP_TEXTURE_DESTROY => Ok(RecordedTextureCommand::Destroy(RecordedTextureDestroy {
            handle: TextureHandle(texture.texture_id),
        })),
        kind => return Err(RfvpHostRuntimeError::UnsupportedTextureKind(kind)),
    }
}

fn convert_texture_create(
    texture: &RfvpTextureCommandV1,
) -> Result<RecordedTextureCommand, RfvpHostRuntimeError> {
    let format = match texture.format {
        RFVP_TEXTURE_FORMAT_RGBA8 => TextureFormat::Rgba8,
        RFVP_TEXTURE_FORMAT_LUMA_A8 => TextureFormat::LumaA8,
        format => return Err(RfvpHostRuntimeError::UnsupportedTextureFormat(format)),
    };
    let width = u16::try_from(texture.width).map_err(|_| RfvpHostRuntimeError::RectOverflow)?;
    let height = u16::try_from(texture.height).map_err(|_| RfvpHostRuntimeError::RectOverflow)?;
    let pixels = if texture.pixels_size == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(texture.pixels, texture.pixels_size).to_vec() }
    };
    Ok(RecordedTextureCommand::Create(RecordedTextureCreate {
        handle: TextureHandle(texture.texture_id),
        desc: PortableTextureDesc {
            width,
            height,
            format,
        },
        pixels,
        generation: texture.generation,
    }))
}

fn convert_texture_update(
    texture: &RfvpTextureCommandV1,
) -> Result<RecordedTextureCommand, RfvpHostRuntimeError> {
    let format = match texture.format {
        RFVP_TEXTURE_FORMAT_RGBA8 => TextureFormat::Rgba8,
        RFVP_TEXTURE_FORMAT_LUMA_A8 => TextureFormat::LumaA8,
        format => return Err(RfvpHostRuntimeError::UnsupportedTextureFormat(format)),
    };
    let rect = rfvp::host_api::TextureRect {
        x: u32::try_from(texture.rect.x).map_err(|_| RfvpHostRuntimeError::RectOverflow)?,
        y: u32::try_from(texture.rect.y).map_err(|_| RfvpHostRuntimeError::RectOverflow)?,
        width: u32::try_from(texture.rect.width).map_err(|_| RfvpHostRuntimeError::RectOverflow)?,
        height: u32::try_from(texture.rect.height)
            .map_err(|_| RfvpHostRuntimeError::RectOverflow)?,
    };
    let pixels = if texture.pixels_size == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(texture.pixels, texture.pixels_size).to_vec() }
    };
    Ok(RecordedTextureCommand::Update(RecordedTextureUpdate {
        handle: TextureHandle(texture.texture_id),
        rect,
        format,
        pixels,
        generation: texture.generation,
    }))
}

fn convert_hit_proxy(proxy: &RfvpHitProxyV1) -> Result<HitProxy, RfvpHostRuntimeError> {
    Ok(HitProxy {
        prim_id: rfvp::host_api::PrimId(proxy.prim_id),
        rect: RectI16 {
            x: i16::try_from(proxy.rect.x).map_err(|_| RfvpHostRuntimeError::HitProxyOverflow)?,
            y: i16::try_from(proxy.rect.y).map_err(|_| RfvpHostRuntimeError::HitProxyOverflow)?,
            w: i16::try_from(proxy.rect.width)
                .map_err(|_| RfvpHostRuntimeError::HitProxyOverflow)?,
            h: i16::try_from(proxy.rect.height)
                .map_err(|_| RfvpHostRuntimeError::HitProxyOverflow)?,
        },
        enabled: proxy.flags & RFVP_HIT_PROXY_ENABLED != 0,
        visible: proxy.flags & RFVP_HIT_PROXY_VISIBLE != 0,
        order: proxy.order,
    })
}

fn convert_vertex(vertex: &rfvp::host_abi::v1::RfvpVertexV1) -> Vertex2D {
    Vertex2D {
        position: [vertex.x, vertex.y],
        tex_coord: [vertex.u, vertex.v],
        color: rfvp::host_api::ColorRgba {
            r: vertex.color.r,
            g: vertex.color.g,
            b: vertex.color.b,
            a: vertex.color.a,
        },
    }
}

fn convert_rect_i16(
    rect: rfvp::host_abi::v1::RfvpRectI32V1,
) -> Result<RectI16, RfvpHostRuntimeError> {
    Ok(RectI16 {
        x: i16::try_from(rect.x).map_err(|_| RfvpHostRuntimeError::RectOverflow)?,
        y: i16::try_from(rect.y).map_err(|_| RfvpHostRuntimeError::RectOverflow)?,
        w: i16::try_from(rect.width).map_err(|_| RfvpHostRuntimeError::RectOverflow)?,
        h: i16::try_from(rect.height).map_err(|_| RfvpHostRuntimeError::RectOverflow)?,
    })
}

fn convert_rgba8(color: rfvp::host_abi::v1::RfvpColorV1) -> Rgba8 {
    Rgba8 {
        r: color_byte(color.r),
        g: color_byte(color.g),
        b: color_byte(color.b),
        a: color_byte(color.a),
    }
}

fn color_byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn convert_blend(blend: u32) -> Result<CommandBlendMode, RfvpHostRuntimeError> {
    match blend {
        RFVP_BLEND_NORMAL => Ok(CommandBlendMode::Normal),
        RFVP_BLEND_ADD => Ok(CommandBlendMode::Add),
        RFVP_BLEND_REVERSE_SUBTRACT => Ok(CommandBlendMode::Sub),
        RFVP_BLEND_MULTIPLY => Ok(CommandBlendMode::Mul),
        blend => Err(RfvpHostRuntimeError::UnsupportedBlend(blend)),
    }
}
