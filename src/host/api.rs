//! Rust entry point for the queued Artemis Host protocol.

#![warn(dead_code, unused_imports)]

use super::frame::FrameLease;
use super::runtime::{self, RuntimeError, RuntimeHandle};
use super::script_io::{ScriptIoError, ScriptIoProgress};
use std::{fmt, marker::PhantomData, rc::Rc};

pub use super::frame::{FRAME_PACKET_VERSION, FrameTexture};
pub use super::input::{HostInput, TouchPhase};
pub use super::media::{PlaybackStatus, VideoFrame};
pub use super::protocol::{
    Domain, ENVELOPE_VERSION, HostReply as HostMessage, MessageBody, MessageKind, PROJECT_READY,
    ProtocolError, QueueBudget, RequestIdentity, RuntimeEvent, SESSION_CHANGED, TransportState,
};
pub use super::runtime::{RuntimePhase, RuntimeState};
pub use super::script_io::{
    HostFrame, IO_DELETE, IO_READ, IO_STAT, IO_WRITE_ABORT, IO_WRITE_ATOMIC, MEDIA_AUDIO_COMMAND,
    MEDIA_VIDEO_COMMAND, RENDER_CAPTURE, RENDER_SAVE_CAPTURE, RUNTIME_CLOSED,
    RUNTIME_EXIT_REQUESTED, RUNTIME_PERSISTENCE_RESULT, UI_AVOID, UI_CAPTION, UI_DIALOG,
    UI_HTTP_CANCEL, UI_HTTP_REQUEST, UI_TRANSLATE, UI_WINDOW_ACTION,
};

pub mod capabilities {
    pub use super::super::runtime::{
        CAP_EMOTE, CAP_GLSL, CAP_GROUPS, CAP_HLSL, CAP_SCREENSHOTS, CAP_SPRITES, CAP_TRANSITIONS,
        HOST_CAP_CHUNKED_WRITES, HOST_CAP_HTTP, HOST_CAP_IO, HOST_CAP_MEDIA,
        HOST_CAP_PROJECT_SAVE_PATH, HOST_CAP_TRANSLATION, HOST_CAP_UI,
    };
}

#[derive(Clone, Copy, Debug)]
pub struct RuntimeOptions {
    pub random_seed: u64,
    pub host_capabilities: u64,
    pub renderer_capabilities: u64,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            random_seed: 0,
            host_capabilities: capabilities::HOST_CAP_IO,
            renderer_capabilities: 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ProjectInfo<'a> {
    pub ini: &'a [u8],
    pub platform: &'a str,
    pub reported_os: Option<&'a str>,
}

#[derive(Clone, Copy, Debug)]
pub struct InputBatch<'a> {
    pub epoch: u64,
    pub events: &'a [HostInput],
}

#[derive(Clone, Copy, Debug)]
pub struct StepResult {
    pub state: RuntimeState,
    pub backpressured: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    Argument,
    State,
    Stale,
    Limit,
    WouldBlock,
    Unsupported,
    Panic,
}

#[derive(Debug)]
pub struct Error {
    pub kind: ErrorKind,
    message: String,
}

impl Error {
    fn new(kind: ErrorKind, message: &str) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<RuntimeError> for Error {
    fn from(error: RuntimeError) -> Self {
        let kind = match &error {
            RuntimeError::Handle | RuntimeError::Thread => ErrorKind::State,
            RuntimeError::Panic => ErrorKind::Panic,
            RuntimeError::Session(session) => match session {
                ScriptIoError::Project(_) => ErrorKind::Argument,
                ScriptIoError::Script(_) => ErrorKind::State,
                ScriptIoError::Protocol(protocol) => match protocol {
                    ProtocolError::Argument(_) => ErrorKind::Argument,
                    ProtocolError::State => ErrorKind::State,
                    ProtocolError::Stale => ErrorKind::Stale,
                    ProtocolError::Limit => ErrorKind::Limit,
                    ProtocolError::WouldBlock => ErrorKind::WouldBlock,
                },
            },
        };
        Self {
            kind,
            message: error.to_string(),
        }
    }
}

/// A CPU frame borrowed for the duration of `Runtime::with_latest_frame`.
pub struct FrameSnapshot<'a>(&'a FrameLease);

impl FrameSnapshot<'_> {
    pub fn runtime_id(&self) -> u64 {
        self.0.runtime().as_raw()
    }

    pub fn render_frame_id(&self) -> u64 {
        self.0.render_frame_id
    }

    pub fn packet(&self) -> &[u8] {
        self.0.packet()
    }

    pub fn texture(&self, index: usize) -> Option<&FrameTexture> {
        self.0.texture(index)
    }
}

/// Owned runtime. Drive it on its creating thread and submit Host results there.
/// Dropping releases the runtime; request shutdown and finish its IO to save first.
pub struct Runtime {
    handle: RuntimeHandle,
    _thread_bound: PhantomData<Rc<()>>,
}

impl Runtime {
    pub fn new(options: RuntimeOptions) -> Result<Self, Error> {
        if options.host_capabilities & !runtime::HOST_CAPS != 0
            || options.renderer_capabilities & !runtime::RENDERER_CAPS != 0
        {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "unknown runtime capability",
            ));
        }
        let handle = runtime::create_with_caps(
            options.random_seed,
            Default::default(),
            runtime::DEFAULT_MAX_FILE_BYTES,
            options.host_capabilities,
            options.renderer_capabilities,
        )?;
        Ok(Self {
            handle,
            _thread_bound: PhantomData,
        })
    }

    /// Accept the project and return its epoch. Subsequent steps run its startup.
    pub fn open_project(&self, project: ProjectInfo<'_>) -> Result<u64, Error> {
        Ok(runtime::open_project(
            self.handle,
            project.ini,
            project.platform,
            project.reported_os,
        )?)
    }

    pub fn state(&self) -> Result<RuntimeState, Error> {
        Ok(runtime::get_state(self.handle)?)
    }

    pub fn step(&self, frame: HostFrame) -> Result<StepResult, Error> {
        if frame.id == 0 {
            return Err(Error::new(ErrorKind::Argument, "invalid Host frame ID"));
        }
        self.step_result(runtime::step(self.handle, frame)?)
    }

    pub fn step_background_autosave(&self) -> Result<StepResult, Error> {
        self.step_result(runtime::step_background_autosave(self.handle)?)
    }

    fn step_result(&self, progress: ScriptIoProgress) -> Result<StepResult, Error> {
        Ok(StepResult {
            state: self.state()?,
            backpressured: progress == ScriptIoProgress::WouldBlock,
        })
    }

    /// Delivers the next reliable event, or the next diagnostic when no reliable events remain.
    pub fn poll_event(&self) -> Result<Option<RuntimeEvent>, Error> {
        Ok(runtime::next_event(self.handle)?)
    }

    /// Accepts a complete batch or none of it. Retry the same batch on `WouldBlock`.
    pub fn submit_replies(&self, replies: &[HostMessage]) -> Result<(), Error> {
        Ok(runtime::submit_replies(self.handle, replies)?)
    }

    pub fn submit_input(&self, input: InputBatch<'_>) -> Result<(), Error> {
        Ok(runtime::submit_input(
            self.handle,
            input.epoch,
            input.events,
        )?)
    }

    pub fn submit_playback_status(
        &self,
        epoch: u64,
        playback_id: u64,
        status: PlaybackStatus,
    ) -> Result<(), Error> {
        Ok(runtime::submit_playback_status(
            self.handle,
            epoch,
            playback_id,
            status,
        )?)
    }

    pub fn submit_video_frame(&self, epoch: u64, frame: VideoFrame<'_>) -> Result<(), Error> {
        Ok(runtime::submit_video_frame(self.handle, epoch, frame)?)
    }

    pub fn set_default_font(&self, epoch: u64, bytes: &[u8]) -> Result<(), Error> {
        Ok(runtime::set_default_font(self.handle, epoch, bytes)?)
    }

    pub fn request_shutdown(&self, epoch: u64) -> Result<(), Error> {
        Ok(runtime::submit_shutdown(self.handle, epoch)?)
    }

    /// Read a newer frame without copying its commands or textures. The frame
    /// stays valid across any steps or reloads performed inside `draw`.
    pub fn with_latest_frame<T>(
        &self,
        after: u64,
        draw: impl FnOnce(FrameSnapshot<'_>) -> T,
    ) -> Result<Option<T>, Error> {
        let frame = runtime::acquire_latest(self.handle, after)?;
        Ok(frame.as_ref().map(|frame| draw(FrameSnapshot(frame))))
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = runtime::destroy(self.handle);
    }
}
