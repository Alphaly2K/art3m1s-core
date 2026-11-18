//! Luau execution over the Host IO request transport. The Host owns all
//! storage; this driver never installs a physical file loader or renderer.

mod audio;
mod control;
mod http;
mod lifecycle;
mod persistence;
mod save_files;
mod scene;
mod screenshot;
mod transition;
mod translation;
mod ui;
mod video;
mod wait;
mod write_chunks;

pub use audio::MEDIA_AUDIO_COMMAND;
pub use http::{UI_HTTP_CANCEL, UI_HTTP_REQUEST};
use lifecycle::Shutdown;
pub use lifecycle::{RUNTIME_CLOSED, RUNTIME_EXIT_REQUESTED};
pub use persistence::RUNTIME_PERSISTENCE_RESULT;
pub use screenshot::{RENDER_CAPTURE, RENDER_SAVE_CAPTURE};
pub use translation::UI_TRANSLATE;
pub use ui::{UI_AVOID, UI_CAPTION, UI_DIALOG, UI_WINDOW_ACTION};
pub use video::MEDIA_VIDEO_COMMAND;
pub use write_chunks::IO_WRITE_ABORT;

use super::protocol::*;
use crate::save::read_history::{AREAD_FILE, ReadHistory};
use asb_interpreter::io_continuation::{
    self, IoFailure, IoProgress, IoRequest, IoResponse, IoTask,
};
use asb_interpreter::{
    CallbackResult, Event, ExecutionResult, HostTime, Interpreter, InterpreterConfig, Script,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt;

pub const IO_STAT: MessageKind = MessageKind {
    domain: Domain::Io,
    kind: 1,
};
pub const IO_READ: MessageKind = MessageKind {
    domain: Domain::Io,
    kind: 2,
};
pub const IO_WRITE_ATOMIC: MessageKind = MessageKind {
    domain: Domain::Io,
    kind: 4,
};
pub const IO_DELETE: MessageKind = MessageKind {
    domain: Domain::Io,
    kind: 5,
};

const SAVED_SCRIPT_PREFETCH_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug)]
pub enum ScriptIoError {
    Protocol(ProtocolError),
    Script(asb_interpreter::Error),
    Project(crate::CoreError),
}

impl fmt::Display for ScriptIoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(error) => error.fmt(f),
            Self::Script(error) => error.fmt(f),
            Self::Project(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for ScriptIoError {}
impl From<ProtocolError> for ScriptIoError {
    fn from(error: ProtocolError) -> Self {
        Self::Protocol(error)
    }
}
impl From<asb_interpreter::Error> for ScriptIoError {
    fn from(error: asb_interpreter::Error) -> Self {
        Self::Script(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptIoProgress {
    Waiting,
    WouldBlock,
    Completed,
    Event,
    EventFilterReady,
    SaveReady,
    LoadReady,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NumberedLoadResult {
    Restored,
    Failed { kind: IoFailure, message: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveResult {
    Written,
    Failed {
        kind: IoFailure,
        message: String,
        files_written: u8,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventFilterDecision {
    DispatchDefault,
    Handled,
    Rejected,
}

/// Time supplied by the Host for one logical scenario frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostFrame {
    pub id: u64,
    pub monotonic_ns: u64,
    pub utc_unix_ms: i64,
    pub utc_offset_minutes: i32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExecutionMode {
    Idle,
    Chunk,
    Scenario,
}

enum BootRestore {
    ReadTagIni,
    TagIni,
    Start,
    Globals,
    Settings,
    ReadHistory,
}

enum EventFilterWork {
    Queued(String, HashMap<String, String>),
    Running(IoTask),
    Ready(EventFilterDecision),
}

enum SaveWork {
    Queued {
        key: Option<String>,
    },
    Hook {
        key: String,
    },
    Writing {
        remaining: VecDeque<(String, Vec<u8>)>,
        files_written: u8,
        saves_read_history: bool,
    },
    Finished(Option<SaveResult>),
}

enum NumberedLoadWork {
    Queued {
        key: String,
    },
    ReadingSave,
    ReadingScript {
        load: Box<PreparedLoad>,
        remaining: VecDeque<(String, usize)>,
        path: String,
        last_line: usize,
        bytes_left: usize,
    },
    ReadingScene {
        load: Box<PreparedLoad>,
        remaining: VecDeque<String>,
        pending: scene::TextureRead,
    },
    ReadingSavedShader {
        load: Box<PreparedLoad>,
        remaining: VecDeque<(String, String)>,
        id: String,
        path: String,
    },
    ReadingSavedFont {
        load: Box<PreparedLoad>,
        pending: scene::FontRead,
    },
    ReadyToRestore(Box<PreparedLoad>),
    Initializing {
        file: String,
    },
    Hook,
    Finished(Option<NumberedLoadResult>),
}

struct PreparedLoad {
    file: HostSaveFile,
    scripts: Vec<Script>,
    shaders: Option<scene::SceneShaders>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct HostSaveFile {
    #[serde(flatten)]
    save: crate::save::SaveData,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shader_assets: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    layer_edits: scene::EditSnapshot,
}

enum ScenarioStep {
    Request(IoRequest),
    Progress(ScriptIoProgress),
    Continue,
}

enum Operation {
    Stat {
        namespace: &'static str,
        key: String,
        purpose: StatPurpose,
    },
    Read {
        namespace: &'static str,
        key: String,
        revision: u64,
        size: usize,
        data: Vec<u8>,
    },
    Write {
        key: String,
        data: Vec<u8>,
    },
    ChunkedWrite(write_chunks::ChunkedWrite),
    Delete {
        key: String,
    },
}

enum StatPurpose {
    Read,
    Exists,
    ModifiedTime,
}

enum Completion {
    Stat {
        found: bool,
        size: u64,
        revision: u64,
        modified_utc_ms: Option<i64>,
    },
    Read,
    WriteStaged,
    Written,
    Deleted,
    Failed(IoFailure, String),
}

/// Script code runs only in `step`, including after result submission. Scenario
/// events remain pending until the Host resumes them. `step_frame` also drives
/// the script's per-frame handler.
pub struct ScriptIoSession {
    interpreter: Interpreter,
    transport: SessionTransport,
    task: Option<IoTask>,
    operation: Option<Operation>,
    identity: Option<RequestIdentity>,
    save_prefix: String,
    max_file_bytes: usize,
    pub(super) chunked_writes: bool,
    last_write_transaction: u64,
    completed: bool,
    faulted: bool,
    cancelled: bool,
    mode: ExecutionMode,
    scenario_event: Option<Event>,
    host_time: HostTime,
    random_seed: u64,
    frame: Option<HostFrame>,
    frame_handler_due: bool,
    wait_start: Option<(u64, u64)>,
    event_filter: Option<EventFilterWork>,
    save_work: Option<SaveWork>,
    save_file_work: Option<(save_files::SaveFileCaller, save_files::SaveFileWork)>,
    gameplay_checkpoint: Option<crate::save::GameplayCheckpoint>,
    numbered_load: Option<NumberedLoadWork>,
    boot_restore: Option<BootRestore>,
    project_loading: bool,
    dialog_request: Option<RequestIdentity>,
    http: http::Http,
    pub(super) media_enabled: bool,
    pub(super) http_enabled: bool,
    translations: translation::Translations,
    pub(super) translation_enabled: bool,
    pub(super) shutdown: Shutdown,
    audio: audio::Audio,
    control: control::PlaybackControl,
    video: video::Video,
    pub(super) transitions_enabled: bool,
    transition: Option<transition::Transition>,
    pub(super) screenshots_enabled: bool,
    pub(super) groups_enabled: bool,
    pub(super) emote_enabled: bool,
    pub(super) shaders_enabled: bool,
    screenshot: screenshot::Screenshot,
    scene: Option<scene::SceneIo>,
    pub(super) frames: super::frame::FramePublisher,
    read_history: ReadHistory,
    read_history_dirty: bool,
}

impl ScriptIoSession {
    pub fn new(
        runtime: u64,
        config: InterpreterConfig,
        limits: TransportLimits,
        max_file_bytes: usize,
    ) -> Result<Self, ScriptIoError> {
        let save_prefix = logical_key(config.savepath.as_deref().unwrap_or("save"))?;
        if max_file_bytes == 0 || limits.blob_bytes == 0 {
            return Err(ProtocolError::Argument("IO byte budgets must be nonzero").into());
        }
        let transport = SessionTransport::new(runtime, limits)?;
        let mut interpreter = Interpreter::new(config);
        interpreter.enable_host_services(0)?;
        io_continuation::install(&interpreter)?;
        Ok(Self {
            interpreter,
            transport,
            task: None,
            operation: None,
            identity: None,
            save_prefix,
            max_file_bytes,
            chunked_writes: false,
            last_write_transaction: 0,
            completed: false,
            faulted: false,
            cancelled: false,
            mode: ExecutionMode::Idle,
            scenario_event: None,
            host_time: HostTime::default(),
            random_seed: 0,
            frame: None,
            frame_handler_due: false,
            wait_start: None,
            event_filter: None,
            save_work: None,
            save_file_work: None,
            gameplay_checkpoint: None,
            numbered_load: None,
            boot_restore: None,
            project_loading: false,
            dialog_request: None,
            http: http::Http::default(),
            media_enabled: true,
            http_enabled: true,
            translations: Default::default(),
            translation_enabled: false,
            shutdown: Shutdown::default(),
            audio: Default::default(),
            control: Default::default(),
            video: Default::default(),
            transitions_enabled: true,
            transition: None,
            screenshots_enabled: true,
            groups_enabled: true,
            emote_enabled: true,
            shaders_enabled: true,
            screenshot: Default::default(),
            scene: None,
            frames: Default::default(),
            read_history: ReadHistory::default(),
            read_history_dirty: false,
        })
    }

    pub fn interpreter(&self) -> &Interpreter {
        &self.interpreter
    }
    pub fn state(&self) -> TransportState {
        self.transport.state()
    }
    pub fn limits(&self) -> TransportLimits {
        self.transport.limits()
    }

    pub(super) fn runtime_state(&self) -> super::runtime::RuntimeState {
        use super::runtime::{RuntimePhase, RuntimeState};

        let transport = self.transport.state();
        let phase = if self.faulted {
            RuntimePhase::Faulted
        } else if matches!(self.shutdown, Shutdown::Closed) {
            RuntimePhase::Closed
        } else if self.shutdown.is_closing_or_closed() {
            RuntimePhase::Closing
        } else if transport.epoch == 0 {
            RuntimePhase::Created
        } else if self.project_loading {
            RuntimePhase::Loading
        } else {
            RuntimePhase::Running
        };
        let config = self.interpreter.config();
        RuntimeState {
            phase,
            transport,
            host_frame_id: self.frame.map(|frame| frame.id),
            latest_render_frame_id: self.frames.latest_id(),
            stage_size: (transport.epoch != 0).then_some([config.stage_width, config.stage_height]),
            waiting_for_io: self.operation.is_some(),
            waiting_for_ui: matches!(self.scenario_event, Some(Event::ShowDialog { .. }))
                || self.http.waits_for_host(),
            waiting_for_render: self.screenshot.pending_request().is_some(),
            waiting_for_scenario: self.scenario_event.is_some(),
            exit_requested: matches!(self.scenario_event, Some(Event::Exit)),
            scenario_finished: self.completed,
        }
    }

    pub(super) fn is_faulted(&self) -> bool {
        self.faulted
    }

    pub(super) fn mark_faulted(&mut self) {
        self.faulted = true;
    }

    /// Select the session's deterministic seed before opening a task. The same
    /// seed initializes each fresh scenario VM, including Luau's math.random.
    pub fn set_random_seed(&mut self, seed: u64) -> Result<(), ScriptIoError> {
        if self.mode != ExecutionMode::Idle {
            return Err(ProtocolError::State.into());
        }
        self.interpreter.enable_host_services(seed)?;
        self.interpreter.set_host_time(self.host_time)?;
        self.random_seed = seed;
        Ok(())
    }

    /// Update script-visible time without running Lua in unframed mode.
    pub fn set_host_time(&mut self, time: HostTime) -> Result<(), ScriptIoError> {
        if self.frame.is_some() {
            return Err(ProtocolError::State.into());
        }
        self.update_host_time(time)
    }

    fn update_host_time(&mut self, time: HostTime) -> Result<(), ScriptIoError> {
        self.interpreter.set_host_time(time)?;
        self.host_time = time;
        Ok(())
    }

    /// Replace the current chunk and invalidate its outstanding IO. Compilation
    /// errors leave the old task intact. Globals in the same interpreter persist.
    pub fn open_chunk(&mut self, source: &[u8], name: &str) -> Result<u64, ScriptIoError> {
        let task = IoTask::compile(self.interpreter.lua(), source, name)?;
        let epoch = self.transport.begin_session()?;
        self.interpreter.clear_host_io();
        self.task = Some(task);
        self.operation = None;
        self.identity = None;
        self.completed = false;
        self.faulted = false;
        self.cancelled = false;
        self.mode = ExecutionMode::Chunk;
        self.scenario_event = None;
        self.frame = None;
        self.frame_handler_due = false;
        self.wait_start = None;
        self.event_filter = None;
        self.save_work = None;
        self.save_file_work = None;
        self.gameplay_checkpoint = None;
        self.numbered_load = None;
        self.boot_restore = None;
        self.project_loading = false;
        self.dialog_request = None;
        self.http = Default::default();
        self.translations = Default::default();
        self.shutdown = Shutdown::default();
        self.audio = Default::default();
        self.control = Default::default();
        self.video = Default::default();
        self.transition = None;
        self.screenshot = Default::default();
        self.scene = None;
        self.frames.clear();
        Ok(epoch)
    }

    /// Start a fresh scenario VM from Host-supplied AST/IET/ASB bytes. Loading
    /// Lua blocks and calllua execute through the same IO continuation driver.
    pub fn open_scenario(
        &mut self,
        source: &[u8],
        name: &str,
        label: &str,
    ) -> Result<u64, ScriptIoError> {
        let mut interpreter = self.scenario_interpreter()?;
        interpreter.load_file(name, source)?;
        interpreter.start(name, label)?;
        self.replace_scenario(interpreter)
    }

    /// Start a fresh VM by restoring available system settings before reading
    /// BOOT and the default macro file. Host IO progresses only in `step`.
    pub fn open_boot(&mut self, name: &str) -> Result<u64, ScriptIoError> {
        logical_key(name)?;
        let mut interpreter = self.scenario_interpreter()?;
        interpreter.boot(name)?;
        let epoch = self.replace_scenario(interpreter)?;
        self.boot_restore = Some(BootRestore::Start);
        Ok(epoch)
    }

    /// Accept a project descriptor from the Host. Parse optional tag.ini and
    /// restore persistent data before loading the configured BOOT script.
    pub fn open_project(
        &mut self,
        ini: &[u8],
        platform: &str,
        reported_os: Option<&str>,
    ) -> Result<u64, ScriptIoError> {
        if ini.len() > self.max_file_bytes {
            return Err(ProtocolError::Limit.into());
        }
        let project = crate::ProjectConfig::from_system_ini_bytes(ini, platform)
            .map_err(ScriptIoError::Project)?;
        if project.stage_width == 0 || project.stage_height == 0 {
            return Err(ProtocolError::Argument("project stage dimensions must be nonzero").into());
        }
        logical_key(&project.boot_script)?;
        let save_prefix = logical_key(
            project
                .savepath
                .as_deref()
                .filter(|path| !path.trim().is_empty())
                .unwrap_or("save"),
        )?;
        let mut config = project.to_interpreter_config(None);
        config.savepath = Some(save_prefix.clone());
        config.reported_os = reported_os.filter(|os| !os.is_empty()).map(str::to_owned);
        let mut interpreter = self.configured_interpreter(config)?;
        interpreter.boot(&project.boot_script)?;
        let epoch = self.replace_scenario(interpreter)?;
        self.save_prefix = save_prefix;
        self.boot_restore = Some(BootRestore::ReadTagIni);
        self.project_loading = true;
        Ok(epoch)
    }

    fn scenario_interpreter(&self) -> Result<Interpreter, ScriptIoError> {
        self.configured_interpreter(self.interpreter.config().clone())
    }

    fn configured_interpreter(
        &self,
        config: InterpreterConfig,
    ) -> Result<Interpreter, ScriptIoError> {
        let mut interpreter = Interpreter::new(config);
        interpreter.enable_host_services(self.random_seed)?;
        interpreter.set_host_time(self.host_time)?;
        interpreter.enable_resumable_io()?;
        interpreter.set_callback(|_| CallbackResult::Pause);
        Ok(interpreter)
    }

    fn replace_scenario(&mut self, interpreter: Interpreter) -> Result<u64, ScriptIoError> {
        let epoch = self.transport.begin_session()?;
        self.interpreter = interpreter;
        self.task = None;
        self.operation = None;
        self.identity = None;
        self.completed = false;
        self.faulted = false;
        self.cancelled = false;
        self.mode = ExecutionMode::Scenario;
        self.scenario_event = None;
        self.frame = None;
        self.frame_handler_due = false;
        self.wait_start = None;
        self.event_filter = None;
        self.save_work = None;
        self.save_file_work = None;
        self.gameplay_checkpoint = None;
        self.numbered_load = None;
        self.boot_restore = None;
        self.project_loading = false;
        self.dialog_request = None;
        self.http = Default::default();
        self.translations = Default::default();
        self.shutdown = Shutdown::default();
        self.audio = Default::default();
        self.control = Default::default();
        self.video = Default::default();
        self.transition = None;
        self.screenshot = Default::default();
        self.scene = None;
        self.frames.clear();
        self.read_history = ReadHistory::default();
        self.read_history_dirty = false;
        Ok(epoch)
    }

    pub fn pending_event(&self) -> Option<&Event> {
        self.scenario_event.as_ref()
    }

    /// Queue one already matched event for filtering. Runs only in `step`.
    /// The caller dispatches the original binding only on DispatchDefault.
    pub fn submit_event_filter(
        &mut self,
        name: &str,
        params: &HashMap<String, String>,
    ) -> Result<(), ProtocolError> {
        if self.mode != ExecutionMode::Scenario || self.completed || self.faulted {
            return Err(ProtocolError::State);
        }
        if self.event_filter.is_some()
            || self.screenshot.pending_request().is_some()
            || self.save_work.is_some()
            || self.numbered_load.is_some()
            || self.operation.is_some()
            || self.cancelled
        {
            return Err(ProtocolError::WouldBlock);
        }
        if name.is_empty() {
            return Err(ProtocolError::Argument("event filter name is empty"));
        }
        let bytes = params
            .iter()
            .try_fold(name.len(), |bytes, (key, value)| {
                bytes
                    .checked_add(key.len())?
                    .checked_add(value.len())?
                    .checked_add(1)
            })
            .ok_or(ProtocolError::Limit)?;
        if bytes > self.limits().metadata_bytes {
            return Err(ProtocolError::Limit);
        }
        self.event_filter = Some(EventFilterWork::Queued(name.into(), params.clone()));
        Ok(())
    }

    pub fn take_event_filter_decision(&mut self) -> Option<EventFilterDecision> {
        if let Some(EventFilterWork::Ready(decision)) = self.event_filter {
            self.event_filter = None;
            Some(decision)
        } else {
            None
        }
    }

    /// Persist a pending [save] event. A named save captures the interpreter
    /// after onSave; an unnamed save writes global and system settings.
    pub fn submit_save(&mut self) -> Result<(), ProtocolError> {
        if self.faulted || self.save_work.is_some() {
            return Err(ProtocolError::State);
        }
        let Some(Event::SaveGame { file }) = &self.scenario_event else {
            return Err(ProtocolError::State);
        };
        if self.event_filter.is_some() || self.operation.is_some() || self.cancelled {
            return Err(ProtocolError::WouldBlock);
        }
        let key = (!file.is_empty())
            .then(|| self.save_key(file))
            .transpose()?;
        self.save_work = Some(SaveWork::Queued { key });
        Ok(())
    }

    pub fn take_save_result(&mut self) -> Option<SaveResult> {
        match &mut self.save_work {
            Some(SaveWork::Finished(result)) => result.take(),
            _ => None,
        }
    }

    /// Load an interpreter snapshot for the current [load file=...] event.
    pub fn submit_numbered_load(&mut self) -> Result<(), ProtocolError> {
        if self.faulted || self.numbered_load.is_some() {
            return Err(ProtocolError::State);
        }
        let Some(Event::LoadGame { file, .. }) = &self.scenario_event else {
            return Err(ProtocolError::State);
        };
        if self.event_filter.is_some() || self.operation.is_some() || self.cancelled {
            return Err(ProtocolError::WouldBlock);
        }
        let key = self.save_key(file)?;
        self.numbered_load = Some(NumberedLoadWork::Queued { key });
        Ok(())
    }

    pub fn take_numbered_load_result(&mut self) -> Option<NumberedLoadResult> {
        let Some(NumberedLoadWork::Finished(result)) = &mut self.numbered_load else {
            return None;
        };
        let result = result.take();
        if result == Some(NumberedLoadResult::Restored) {
            self.numbered_load = None;
        }
        result
    }

    pub fn resume_scenario(&mut self) -> Result<(), ProtocolError> {
        if self.faulted
            || self.interpreter.pending_host_io().is_some()
            || self.event_filter.is_some()
            || self.http.is_pending()
            || self.save_file_work.is_some()
            || self.screenshot.pending_request().is_some()
            || self
                .save_work
                .as_ref()
                .is_some_and(|work| !matches!(work, SaveWork::Finished(None)))
            || self
                .numbered_load
                .as_ref()
                .is_some_and(|work| !matches!(work, NumberedLoadWork::Finished(None)))
        {
            return Err(ProtocolError::State);
        }
        if self.scenario_event.take().is_none() {
            return Err(ProtocolError::State);
        }
        self.save_work = None;
        self.numbered_load = None;
        self.wait_start = None;
        self.control.reset_wait();
        self.interpreter.advance_line();
        Ok(())
    }

    pub fn poll_events(&mut self, output: &mut [u8]) -> PollResult {
        self.transport.poll_events(output)
    }

    /// Service payload validation precedes transport acceptance, so malformed
    /// replies leave the original request available for a corrected response.
    pub fn submit_replies(&mut self, replies: &[HostReply]) -> Result<(), ProtocolError> {
        let limits = self.limits();
        if replies.len() > limits.batch_messages || replies.len() > limits.replies.records {
            return Err(ProtocolError::Limit);
        }
        let mut bytes = 0usize;
        for reply in replies {
            if reply.body.metadata().len() > limits.metadata_bytes
                || reply.body.blob().len() > limits.blob_bytes
            {
                return Err(ProtocolError::Limit);
            }
            bytes = bytes
                .checked_add(HEADER_BYTES + 8)
                .and_then(|bytes| bytes.checked_add(reply.body.metadata().len()))
                .and_then(|bytes| bytes.checked_add(reply.body.blob().len()))
                .ok_or(ProtocolError::Limit)?;
            if bytes > limits.replies.bytes {
                return Err(ProtocolError::Limit);
            }
        }
        for reply in replies {
            if Some(reply.identity) == self.identity {
                self.decode(reply)?;
            } else if Some(reply.identity) == self.dialog_request {
                ui::decode_dialog_reply(reply)?;
            } else if Some(reply.identity) == self.http.identity() {
                http::decode_reply(reply)?;
            } else if self.translations.accepts(reply.identity) {
                translation::decode_reply(reply)?;
            } else if Some(reply.identity) == self.screenshot.pending_request() {
                screenshot::decode_screenshot_reply(reply)?;
            } else {
                return Err(ProtocolError::Stale);
            }
        }
        self.transport.submit_replies(replies)
    }

    pub fn cancel_pending(&mut self) -> Result<(), ProtocolError> {
        if self.operation.is_none() {
            return Err(ProtocolError::State);
        }
        let abort = self.pending_write_abort()?;
        if let Some(body) = &abort {
            self.transport.check_notify(IO_WRITE_ABORT, body)?;
        }
        if let Some(identity) = self.identity {
            self.transport.cancel(identity)?;
        }
        if let Some(body) = abort {
            self.transport.notify(IO_WRITE_ABORT, body)?;
        }
        self.identity = None;
        self.operation = None;
        // The Lua error branch is resumed only at the next step boundary.
        self.cancelled = true;
        Ok(())
    }

    fn pointer_ready(&self) -> bool {
        matches!(self.shutdown, Shutdown::Open)
            && matches!(self.scenario_event, None | Some(Event::Wait { .. }))
            && self
                .scene
                .as_ref()
                .is_some_and(scene::SceneIo::pointer_ready)
    }

    pub fn step(&mut self) -> Result<ScriptIoProgress, ScriptIoError> {
        if self.faulted || self.mode == ExecutionMode::Idle {
            return Err(ProtocolError::State.into());
        }
        if matches!(self.shutdown, Shutdown::Closed) {
            return Ok(ScriptIoProgress::Completed);
        }
        if matches!(self.shutdown, Shutdown::Reporting(_)) {
            return Ok(ScriptIoProgress::SaveReady);
        }
        // A reply can arrive while onEnterFrame awaits IO. Release its queue
        // slot now and apply the HTTP result only after the handler returns.
        self.collect_http_reply();
        if let Some(scene) = &self.scene {
            self.translations.advance(scene, &mut self.transport)?;
        }
        let dispatch_scene =
            self.scene_handlers_ready() || self.pointer_ready() || self.control.avoid_pending();
        if self.completed && !self.shutdown.is_closing_or_closed() && !dispatch_scene {
            return Ok(ScriptIoProgress::Completed);
        }
        if self.scenario_event.is_some()
            && !self.frame_handler_due
            && self.interpreter.pending_host_io().is_none()
            && self.event_filter.is_none()
            && self.save_work.is_none()
            && self.save_file_work.is_none()
            && self.numbered_load.is_none()
            && !matches!(self.http, http::Http::Writing(_))
            && !dispatch_scene
        {
            return Ok(ScriptIoProgress::Event);
        }
        let result = self.step_inner();
        if result.is_err() {
            self.faulted = true;
        }
        result
    }

    /// An accepted frame runs onEnterFrame once. Retry the same ID and
    /// monotonic timestamp to resume IO, or use `step()` to keep its clock.
    /// A newer frame cannot replace a frame still suspended on IO.
    pub fn step_frame(&mut self, frame: HostFrame) -> Result<ScriptIoProgress, ScriptIoError> {
        if self.faulted || self.mode != ExecutionMode::Scenario {
            return Err(ProtocolError::State.into());
        }
        if self.frame.is_some_and(|previous| {
            frame.id < previous.id
                || frame.monotonic_ns < previous.monotonic_ns
                || (frame.id == previous.id && frame.monotonic_ns != previous.monotonic_ns)
        }) {
            return Err(ProtocolError::Argument("invalid Host frame order or timestamp").into());
        }
        if !(-1440..=1440).contains(&frame.utc_offset_minutes) {
            return Err(ProtocolError::Argument("invalid Host UTC offset").into());
        }
        if matches!(self.shutdown, Shutdown::Closed)
            || (self.scene.is_none() && self.completed && !self.shutdown.is_closing_or_closed())
        {
            return Ok(ScriptIoProgress::Completed);
        }
        let new_frame = self.frame.is_none_or(|previous| frame.id != previous.id);
        if new_frame
            && (self.operation.is_some()
                || self.control.avoid_pending()
                || self.cancelled
                || (self.scene.is_some()
                    && self.screenshots_enabled
                    && self.has_screenshot_event())
                || self.save_work.as_ref().is_some_and(|work| {
                    !matches!(
                        (&self.shutdown, work),
                        (Shutdown::Saving, SaveWork::Queued { .. })
                    )
                })
                || self.numbered_load.is_some()
                || matches!(self.event_filter, Some(EventFilterWork::Ready(_))))
        {
            return Ok(ScriptIoProgress::WouldBlock);
        }
        self.update_host_time(HostTime {
            monotonic_ms: frame.monotonic_ns / 1_000_000,
            utc_unix_ms: frame.utc_unix_ms,
            utc_offset_minutes: frame.utc_offset_minutes,
        })?;
        self.frame = Some(frame);
        if new_frame {
            self.expire_load_transition(frame);
            self.audio.advance(frame);
            if let Some(scene) = self.scene.as_mut() {
                self.video.advance(scene);
                scene.advance_input(frame.monotonic_ns);
                scene.advance_clock(frame.monotonic_ns);
            }
            self.interpreter.begin_frame();
            self.frame_handler_due = !self.completed
                && matches!(self.shutdown, Shutdown::Open)
                && !matches!(
                    self.scenario_event,
                    Some(Event::ShowDialog { .. } | Event::YesNo { .. } | Event::Exit)
                );
        }
        self.step()
    }

    fn step_inner(&mut self) -> Result<ScriptIoProgress, ScriptIoError> {
        let mut response = if std::mem::take(&mut self.cancelled) {
            Some(failed(IoFailure::Cancelled, "Host IO cancelled"))
        } else {
            None
        };
        if let Some(identity) = self.identity {
            let Some(reply) = self.transport.take_reply(identity) else {
                return Ok(ScriptIoProgress::Waiting);
            };
            response = self.apply(reply)?;
            self.identity = None;
        }
        loop {
            if let Err(error) = self.prepare_write() {
                self.operation = None;
                response = Some(failed(IoFailure::Limit, error.to_string()));
            }
            if let Some(operation) = &self.operation {
                let (service, metadata, blob) = self.message(operation);
                let limits = self.transport.limits();
                let bytes = (HEADER_BYTES + 8)
                    .checked_add(metadata.len())
                    .and_then(|bytes| bytes.checked_add(blob.len()))
                    .ok_or(ProtocolError::Limit)?;
                if metadata.len() > limits.metadata_bytes
                    || blob.len() > limits.blob_bytes
                    || bytes > limits.control.bytes
                    || bytes > u32::MAX as usize
                {
                    self.operation = None;
                    response = Some(failed(
                        IoFailure::Limit,
                        "Host IO request exceeds transport budget",
                    ));
                    continue;
                }
                match self
                    .transport
                    .request(service, MessageBody::new(metadata, blob)?)
                {
                    Ok(identity) => {
                        self.identity = Some(identity);
                        return Ok(ScriptIoProgress::Waiting);
                    }
                    Err(ProtocolError::WouldBlock) => return Ok(ScriptIoProgress::WouldBlock),
                    Err(error) => return Err(error.into()),
                }
            }
            if let Some((caller, work)) = self.save_file_work.take() {
                let result = self.advance_save_file_operation(
                    caller,
                    work,
                    response.take().ok_or(ProtocolError::State)?,
                )?;
                let Some(result) = result else {
                    continue;
                };
                match caller {
                    save_files::SaveFileCaller::Scenario => {
                        if let IoResponse::Failed { kind, .. } = result {
                            crate::core_warn!("save file operation failed: {}", kind.as_str());
                        }
                        self.resume_scenario()?;
                    }
                    save_files::SaveFileCaller::Lua => response = Some(result),
                }
            }
            let request = match self.mode {
                ExecutionMode::Chunk => {
                    let task = self.task.as_mut().unwrap();
                    if task.step(response.take())? == IoProgress::Completed {
                        let _ = task.take_result();
                        self.completed = true;
                        return Ok(ScriptIoProgress::Completed);
                    }
                    task.pending_request().unwrap().clone()
                }
                ExecutionMode::Scenario => match self.step_scenario(response.take())? {
                    ScenarioStep::Request(request) => request,
                    ScenarioStep::Progress(progress) => return Ok(progress),
                    ScenarioStep::Continue => continue,
                },
                ExecutionMode::Idle => return Err(ProtocolError::State.into()),
            };
            match self.schedule_io(request) {
                Ok(result) => response = result,
                Err(error) => response = Some(failed(IoFailure::Denied, error.to_string())),
            }
        }
    }

    fn step_scenario(
        &mut self,
        mut response: Option<IoResponse>,
    ) -> Result<ScenarioStep, ScriptIoError> {
        if matches!(self.http, http::Http::Writing(_)) {
            self.complete_http_write(response.take().ok_or(ProtocolError::State)?)?;
        }
        if let Some(scene) = self.scene.as_mut()
            && scene.shader_pending()
        {
            scene.complete_shader(response.take().ok_or(ProtocolError::State)?);
            self.interpreter.advance_line();
        }
        if let Some(scene) = self.scene.as_mut()
            && scene.font_pending()
        {
            if let Some(request) =
                scene.complete_event_font(response.take().ok_or(ProtocolError::State)?)
            {
                return Ok(ScenarioStep::Request(request));
            }
            self.interpreter.advance_line();
        }
        if let Some(scene) = self.scene.as_mut()
            && scene.asset_pending()
        {
            match scene.complete_asset(response.take().ok_or(ProtocolError::State)?) {
                scene::AssetProgress::More(request) => return Ok(ScenarioStep::Request(request)),
                scene::AssetProgress::Transition(event) => {
                    self.scenario_event = Some(event);
                    self.wait_start = None;
                    return Ok(ScenarioStep::Progress(ScriptIoProgress::Event));
                }
                scene::AssetProgress::Applied => self.interpreter.advance_line(),
            }
        }
        if let Some(stage) = self.boot_restore.take()
            && let Some(request) = self.step_boot_restore(stage, response.take())?
        {
            return Ok(ScenarioStep::Request(request));
        }
        if let Some(work) = self.save_work.take() {
            return self.step_save(work, response);
        }
        if let Some(work) = self.numbered_load.take() {
            return self.step_numbered_load(work, response);
        }
        if matches!(self.event_filter, Some(EventFilterWork::Running(_))) {
            let Some(EventFilterWork::Running(task)) = self.event_filter.take() else {
                unreachable!()
            };
            return self.step_event_filter(task, response);
        }
        if let Some(response) = response {
            self.interpreter.resume_host_io(response)?;
        }
        if self.project_loading {
            if self.interpreter.prepare_host_handler()? == IoProgress::Waiting {
                return self.interpreter_request();
            }
            match self.transport.notify(PROJECT_READY, MessageBody::default()) {
                Ok(()) => self.project_loading = false,
                Err(ProtocolError::WouldBlock) => {
                    return Ok(ScenarioStep::Progress(ScriptIoProgress::WouldBlock));
                }
                Err(error) => return Err(error.into()),
            }
        }
        if (self.frame_handler_due
            || matches!(self.event_filter, Some(EventFilterWork::Queued(..))))
            && self.interpreter.prepare_host_handler()? == IoProgress::Waiting
        {
            return self.interpreter_request();
        }
        if self.frame_handler_due {
            self.frame_handler_due = false;
            if self
                .interpreter
                .start_host_event_handler("onEnterFrame", &Default::default())?
                == IoProgress::Waiting
            {
                return self.interpreter_request();
            }
        }
        if self.interpreter.pending_host_io().is_some() {
            return self.interpreter_request();
        }
        if self.event_filter.is_none() && self.pointer_ready() {
            if self
                .scene
                .as_ref()
                .is_some_and(scene::SceneIo::freeze_new_input)
            {
                self.handle_early_input();
            }
            self.event_filter = self
                .scene
                .as_mut()
                .and_then(scene::SceneIo::next_pointer_filter);
        }
        if !self.deliver_avoid_state()? {
            return Ok(ScenarioStep::Progress(ScriptIoProgress::WouldBlock));
        }
        match self.event_filter.take() {
            Some(EventFilterWork::Queued(name, params)) => {
                return match self.interpreter.host_event_filter_task(&name, &params) {
                    Ok(Some(task)) => self.step_event_filter(task, None),
                    Ok(None) => self.finish_event_filter(EventFilterDecision::DispatchDefault),
                    Err(error) => self.reject_event_filter(error),
                };
            }
            Some(ready @ EventFilterWork::Ready(_)) => {
                self.event_filter = Some(ready);
                return Ok(ScenarioStep::Progress(ScriptIoProgress::EventFilterReady));
            }
            Some(EventFilterWork::Running(_)) => unreachable!(),
            None => {}
        }
        if let Some(frame) = self.frame {
            self.update_control_input(frame);
        }
        if !self.deliver_avoid_state()? {
            return Ok(ScenarioStep::Progress(ScriptIoProgress::WouldBlock));
        }
        if self.pointer_ready() {
            return Ok(ScenarioStep::Continue);
        }
        self.dispatch_scene_handlers()?;
        if self.completed {
            return Ok(ScenarioStep::Progress(ScriptIoProgress::Completed));
        }
        if self.scenario_event.is_some() {
            return Ok(ScenarioStep::Progress(ScriptIoProgress::Event));
        }
        match self.interpreter.step()? {
            ExecutionResult::HostIoPending => self.interpreter_request(),
            ExecutionResult::Wait(event) => {
                if let Event::ShaderLoad { id, file } = &event
                    && let Some(scene) = self.scene.as_mut()
                {
                    if self.shaders_enabled
                        && let Some(request) = scene.prepare_shader(id, file)
                    {
                        return Ok(ScenarioStep::Request(request));
                    }
                    self.interpreter.advance_line();
                    return Ok(ScenarioStep::Continue);
                }
                let load_asset = !matches!(event, Event::Trans { .. })
                    || (self.transitions_enabled && self.frames.latest_id().is_some());
                if let Some(scene) = self.scene.as_mut()
                    && load_asset
                    && let Some(request) = scene
                        .prepare_asset(&event)
                        .or_else(|| scene.prepare_event_font(&event))
                {
                    return Ok(ScenarioStep::Request(request));
                }
                if matches!(event, Event::ScenarioText { .. }) {
                    self.record_read_history();
                }
                self.wait_start = self.interpreter.current_host_wait_start();
                self.scenario_event = Some(if let Some(scene) = &self.scene {
                    scene.take_scene_wait(event)
                } else {
                    event
                });
                Ok(ScenarioStep::Progress(ScriptIoProgress::Event))
            }
            ExecutionResult::Completed => {
                self.completed = true;
                Ok(ScenarioStep::Progress(ScriptIoProgress::Completed))
            }
            _ => Err(ProtocolError::State.into()),
        }
    }

    fn interpreter_request(&self) -> Result<ScenarioStep, ScriptIoError> {
        Ok(ScenarioStep::Request(
            self.interpreter
                .pending_host_io()
                .ok_or(ProtocolError::State)?
                .clone(),
        ))
    }

    fn record_read_history(&mut self) {
        let source = self
            .interpreter
            .engine_context()
            .lock()
            .unwrap()
            .scenario_text_source
            .clone();
        if let Some((script, line)) = source {
            let new = self.read_history.insert(&script, line);
            self.read_history_dirty |= new;
            if new {
                self.stop_skip_at_unread_text();
            }
            self.interpreter.set_variable(
                "s.status.alreadyread",
                asb_interpreter::Value::Int(i64::from(!new)),
            );
        }
    }

    fn step_boot_restore(
        &mut self,
        stage: BootRestore,
        response: Option<IoResponse>,
    ) -> Result<Option<IoRequest>, ScriptIoError> {
        use crate::save::system::{SAVEG_FILE, SYSTEM_FILE, apply_persistent_variables};
        let file = match stage {
            BootRestore::ReadTagIni => {
                self.boot_restore = Some(BootRestore::TagIni);
                return Ok(Some(IoRequest::Read {
                    path: "tag.ini".into(),
                }));
            }
            BootRestore::Start => {
                self.boot_restore = Some(BootRestore::Globals);
                return Ok(Some(IoRequest::Read {
                    path: format!("{}/{SAVEG_FILE}", self.save_prefix),
                }));
            }
            BootRestore::Globals => SAVEG_FILE,
            BootRestore::TagIni => "tag.ini",
            BootRestore::Settings => SYSTEM_FILE,
            BootRestore::ReadHistory => AREAD_FILE,
        };
        let result = match response {
            Some(IoResponse::Read(data)) if matches!(stage, BootRestore::TagIni) => {
                let (text, _, _) = self.interpreter.config().encoding.decode(&data);
                self.interpreter.load_tag_ini(&text);
                Ok(())
            }
            Some(IoResponse::Read(data)) if file == AREAD_FILE => serde_json::from_slice(&data)
                .map(|values| self.read_history.import(values))
                .map_err(|error| error.to_string()),
            Some(IoResponse::Read(data)) => serde_json::from_slice(&data)
                .map(|values| {
                    apply_persistent_variables(&mut self.interpreter, values, file == SAVEG_FILE);
                })
                .map_err(|error| error.to_string()),
            Some(IoResponse::Failed {
                kind: IoFailure::NotFound,
                ..
            }) => Ok(()),
            Some(IoResponse::Failed { message, .. }) => Err(message),
            _ => return Err(ProtocolError::State.into()),
        };
        if let Err(error) = result {
            self.interpreter
                .engine_context()
                .lock()
                .unwrap()
                .callbacks
                .debug(0, &format!("system restore skipped {file}: {error}"), false);
        }
        let (next, requested) = match stage {
            BootRestore::TagIni => (BootRestore::Globals, SAVEG_FILE),
            BootRestore::Globals => (BootRestore::Settings, SYSTEM_FILE),
            BootRestore::Settings => (BootRestore::ReadHistory, AREAD_FILE),
            BootRestore::ReadHistory => return Ok(None),
            BootRestore::Start | BootRestore::ReadTagIni => unreachable!(),
        };
        self.boot_restore = Some(next);
        Ok(Some(IoRequest::Read {
            path: format!("{}/{requested}", self.save_prefix),
        }))
    }

    fn step_save(
        &mut self,
        work: SaveWork,
        response: Option<IoResponse>,
    ) -> Result<ScenarioStep, ScriptIoError> {
        let key = match work {
            SaveWork::Queued { key: None } => return self.start_save_writes(None),
            SaveWork::Queued { key: Some(key) } => {
                let Some(Event::SaveGame { file }) = &self.scenario_event else {
                    return Err(ProtocolError::State.into());
                };
                if let Err(error) = self.interpreter.start_host_save_hook(file) {
                    return self.fail_save_hook(error);
                }
                key
            }
            SaveWork::Hook { key } => {
                if let Some(response) = response
                    && let Err(error) = self.interpreter.resume_host_io(response)
                {
                    return self.fail_save_hook(error);
                }
                key
            }
            SaveWork::Writing {
                mut remaining,
                files_written,
                saves_read_history,
            } => {
                let result = match response {
                    Some(IoResponse::Written) => {
                        if let Some((key, data)) = remaining.pop_front() {
                            self.save_work = Some(SaveWork::Writing {
                                remaining,
                                files_written: files_written + 1,
                                saves_read_history,
                            });
                            return Ok(self.save_write_request(&key, data));
                        }
                        if saves_read_history {
                            self.read_history_dirty = false;
                        }
                        SaveResult::Written
                    }
                    Some(IoResponse::Failed { kind, message }) => SaveResult::Failed {
                        kind,
                        message,
                        files_written,
                    },
                    _ => return Err(ProtocolError::State.into()),
                };
                return Ok(self.finish_save(result));
            }
            SaveWork::Finished(result) => {
                let progress = if result.is_some() {
                    ScriptIoProgress::SaveReady
                } else {
                    ScriptIoProgress::Event
                };
                self.save_work = Some(SaveWork::Finished(result));
                return Ok(ScenarioStep::Progress(progress));
            }
        };
        match self.interpreter.finish_host_save_hook() {
            Ok(IoProgress::Completed) => {}
            Ok(IoProgress::Waiting) => {
                self.save_work = Some(SaveWork::Hook { key });
                return self.interpreter_request();
            }
            Err(error) => return self.fail_save_hook(error),
        }
        let mut save = crate::save::SaveData::from_interpreter(&self.interpreter);
        save.current_line = self.interpreter.position_after_event();
        save.waiting_for_input = Some(false);
        if let Some(checkpoint) = &self.gameplay_checkpoint
            && checkpoint.is_ancestor_of(&save.current_script, &save.call_stack)
            && save.call_stack[checkpoint.call_stack.len()].return_line == checkpoint.line
        {
            save = save.with_gameplay_checkpoint(checkpoint);
        }
        let mut shader_assets = None;
        let mut layer_edits = Ok(scene::EditSnapshot::new());
        if let Some(scene) = &self.scene {
            if let Some(capture) = &self.screenshot.captured {
                save.scene = Some(capture.scene.clone());
                save.message_text = capture.text.clone();
                shader_assets = capture.shaders.clone();
                layer_edits = capture.edits.clone();
            } else {
                save.scene = Some(self.capture_scene_snapshot(scene));
                save.message_text = scene.capture_text();
                shader_assets = scene.capture_shader_assets();
                layer_edits = scene.capture_layer_edits();
            }
            save.audio = Some(self.audio.snapshot());
        }
        let layer_edits = match layer_edits {
            Ok(edits) => edits,
            Err(error) => {
                return Ok(self.finish_save(SaveResult::Failed {
                    kind: if error == ProtocolError::Limit {
                        IoFailure::Limit
                    } else {
                        IoFailure::IoError
                    },
                    message: error.to_string(),
                    files_written: 0,
                }));
            }
        };
        let data = match serde_json::to_vec(&HostSaveFile {
            save,
            shader_assets,
            layer_edits,
        }) {
            Ok(data) => data,
            Err(error) => return self.fail_save_hook(error),
        };
        self.start_save_writes(Some((key, data)))
    }

    fn start_save_writes(
        &mut self,
        snapshot: Option<(String, Vec<u8>)>,
    ) -> Result<ScenarioStep, ScriptIoError> {
        use crate::save::system::{SAVEG_FILE, SYSTEM_FILE, persistent_system_snapshot};
        let store = self.interpreter.variables();
        let global: HashMap<_, _> = store.iter_global().collect();
        let system = persistent_system_snapshot(&store);
        let build = || -> Result<_, serde_json::Error> {
            let mut files = VecDeque::new();
            files.extend(snapshot);
            files.push_back((SAVEG_FILE.into(), serde_json::to_vec(&global)?));
            files.push_back((SYSTEM_FILE.into(), serde_json::to_vec(&system)?));
            if self.read_history_dirty {
                files.push_back((
                    AREAD_FILE.into(),
                    serde_json::to_vec(&self.read_history.export())?,
                ));
            }
            Ok(files)
        };
        let mut files = match build() {
            Ok(files) => files,
            Err(error) => return self.fail_save_hook(error),
        };
        let byte_limit = self.write_file_limit();
        if files.iter().any(|(_, data)| data.len() > byte_limit) {
            return Ok(self.finish_save(SaveResult::Failed {
                kind: IoFailure::Limit,
                message: "save exceeds file budget".into(),
                files_written: 0,
            }));
        }
        let (key, data) = files
            .pop_front()
            .expect("global and system saves are present");
        self.save_work = Some(SaveWork::Writing {
            remaining: files,
            files_written: 0,
            saves_read_history: self.read_history_dirty,
        });
        Ok(self.save_write_request(&key, data))
    }

    fn save_write_request(&self, key: &str, data: Vec<u8>) -> ScenarioStep {
        ScenarioStep::Request(IoRequest::Write {
            path: format!("{}/{key}", self.save_prefix),
            data,
        })
    }

    fn fail_save_hook(&mut self, error: impl fmt::Display) -> Result<ScenarioStep, ScriptIoError> {
        self.interpreter.abort_host_save_hook();
        Ok(self.finish_save(SaveResult::Failed {
            kind: IoFailure::IoError,
            message: error.to_string(),
            files_written: 0,
        }))
    }

    fn finish_save(&mut self, result: SaveResult) -> ScenarioStep {
        self.save_work = Some(SaveWork::Finished(Some(result)));
        ScenarioStep::Progress(ScriptIoProgress::SaveReady)
    }

    fn step_numbered_load(
        &mut self,
        work: NumberedLoadWork,
        response: Option<IoResponse>,
    ) -> Result<ScenarioStep, ScriptIoError> {
        match work {
            NumberedLoadWork::Queued { key } => {
                self.numbered_load = Some(NumberedLoadWork::ReadingSave);
                Ok(ScenarioStep::Request(IoRequest::Read {
                    path: format!("{}/{key}", self.save_prefix),
                }))
            }
            NumberedLoadWork::ReadingSave => {
                let bytes = match response {
                    Some(IoResponse::Read(bytes)) => bytes,
                    Some(IoResponse::Failed { kind, message }) => {
                        return Ok(self.fail_numbered_load(kind, message));
                    }
                    _ => return Err(ProtocolError::State.into()),
                };
                let file: HostSaveFile = match serde_json::from_slice(&bytes) {
                    Ok(file) => file,
                    Err(error) => {
                        return Ok(self.fail_numbered_load(IoFailure::IoError, error.to_string()));
                    }
                };
                let save = &file.save;
                if save.version > crate::save::SAVE_FORMAT_VERSION
                    || ((save.scene.is_some()
                        || save.audio.is_some()
                        || save.message_text.is_some()
                        || file.shader_assets.is_some()
                        || !file.layer_edits.is_empty())
                        && self.scene.is_none())
                {
                    return Ok(self.fail_numbered_load(
                        IoFailure::Unsupported,
                        "unsupported save version or runtime snapshot",
                    ));
                }
                self.prepare_numbered_load_scripts(file)
            }
            NumberedLoadWork::ReadingScript {
                mut load,
                remaining,
                path,
                last_line,
                bytes_left,
            } => {
                let bytes = match response {
                    Some(IoResponse::Read(bytes)) => bytes,
                    Some(IoResponse::Failed { kind, message }) => {
                        return Ok(self.fail_numbered_load(kind, message));
                    }
                    _ => return Err(ProtocolError::State.into()),
                };
                let Some(bytes_left) = bytes_left.checked_sub(bytes.len()) else {
                    return Ok(self.fail_numbered_load(
                        IoFailure::Limit,
                        "saved scripts exceed the load budget",
                    ));
                };
                match self
                    .interpreter
                    .prepare_host_restore_script(&path, &bytes, last_line)
                {
                    Ok(script) => load.scripts.push(script),
                    Err(error) => {
                        return Ok(self.fail_numbered_load(IoFailure::IoError, error.to_string()));
                    }
                }
                self.read_saved_script(load, remaining, bytes_left)
            }
            NumberedLoadWork::ReadingScene {
                load,
                mut remaining,
                mut pending,
            } => {
                let response = response.ok_or(ProtocolError::State)?;
                if let IoResponse::Failed {
                    kind: IoFailure::Cancelled,
                    message,
                } = response
                {
                    return Ok(self.fail_numbered_load(IoFailure::Cancelled, message));
                }
                let scene = self.scene.as_ref().ok_or(ProtocolError::State)?;
                if let Some(request) = scene.complete_texture(&mut pending, response) {
                    self.numbered_load = Some(NumberedLoadWork::ReadingScene {
                        load,
                        remaining,
                        pending,
                    });
                    return Ok(ScenarioStep::Request(request));
                }
                if let Some(file) = remaining.pop_front() {
                    return Ok(self.read_saved_scene_texture(load, remaining, file));
                }
                self.restore_numbered_load(load)
            }
            NumberedLoadWork::ReadingSavedShader {
                mut load,
                remaining,
                id,
                path,
            } => {
                let source = match response {
                    Some(IoResponse::Read(source)) => source,
                    Some(IoResponse::Failed { kind, message }) => {
                        return Ok(self.fail_numbered_load(kind, message));
                    }
                    _ => return Err(ProtocolError::State.into()),
                };
                let shaders = load.shaders.as_mut().ok_or(ProtocolError::State)?;
                if !shaders.register(id, path, source) {
                    return Ok(self.fail_numbered_load(
                        IoFailure::IoError,
                        "invalid saved shader source or shader budget exceeded",
                    ));
                }
                self.read_saved_shader(load, remaining)
            }
            NumberedLoadWork::ReadyToRestore(load) => self.restore_numbered_load(load),
            NumberedLoadWork::ReadingSavedFont { load, mut pending } => {
                let response = response.ok_or(ProtocolError::State)?;
                if let IoResponse::Failed {
                    kind: IoFailure::Cancelled,
                    message,
                } = response
                {
                    return Ok(self.fail_numbered_load(IoFailure::Cancelled, message));
                }
                let scene = self.scene.as_ref().ok_or(ProtocolError::State)?;
                if let Some(request) = scene.complete_font(&mut pending, response) {
                    self.numbered_load = Some(NumberedLoadWork::ReadingSavedFont { load, pending });
                    return Ok(ScenarioStep::Request(request));
                }
                self.restore_numbered_load(load)
            }
            NumberedLoadWork::Initializing { file } => {
                let _ = self
                    .interpreter
                    .resume_host_io(response.ok_or(ProtocolError::State)?)?;
                self.start_numbered_load_hook(file)
            }
            NumberedLoadWork::Hook => {
                if self
                    .interpreter
                    .resume_host_io(response.ok_or(ProtocolError::State)?)?
                    == IoProgress::Waiting
                {
                    self.numbered_load = Some(NumberedLoadWork::Hook);
                    return self.interpreter_request();
                }
                Ok(self.finish_numbered_load(NumberedLoadResult::Restored))
            }
            NumberedLoadWork::Finished(result) => {
                let progress = if result.is_some() {
                    ScriptIoProgress::LoadReady
                } else {
                    ScriptIoProgress::Event
                };
                self.numbered_load = Some(NumberedLoadWork::Finished(result));
                Ok(ScenarioStep::Progress(progress))
            }
        }
    }

    fn prepare_numbered_load_scripts(
        &mut self,
        file: HostSaveFile,
    ) -> Result<ScenarioStep, ScriptIoError> {
        let save = &file.save;
        let mut required = Vec::new();
        let mut indices = HashMap::new();
        let positions = save
            .call_stack
            .iter()
            .map(|frame| (frame.script.as_str(), frame.return_line))
            .chain(std::iter::once((
                save.current_script.as_str(),
                save.current_line,
            )));
        for (path, line) in positions {
            if logical_key(path).is_err() {
                return Ok(self
                    .fail_numbered_load(IoFailure::IoError, "invalid saved script or call frame"));
            }
            if let Some(script) = self.interpreter.get_script(path) {
                if line > script.len() {
                    return Ok(self.fail_numbered_load(
                        IoFailure::IoError,
                        "save position is beyond the script",
                    ));
                }
            } else if let Some(&i) = indices.get(path) {
                let entry: &mut (String, usize) = &mut required[i];
                entry.1 = entry.1.max(line);
            } else {
                indices.insert(path, required.len());
                required.push((path.to_owned(), line));
            }
        }
        self.read_saved_script(
            Box::new(PreparedLoad {
                file,
                scripts: Vec::new(),
                shaders: None,
            }),
            required.into(),
            self.max_file_bytes.max(SAVED_SCRIPT_PREFETCH_BYTES),
        )
    }

    fn read_saved_script(
        &mut self,
        load: Box<PreparedLoad>,
        mut remaining: VecDeque<(String, usize)>,
        bytes_left: usize,
    ) -> Result<ScenarioStep, ScriptIoError> {
        if let Some((path, last_line)) = remaining.pop_front() {
            let request = IoRequest::Read { path: path.clone() };
            self.numbered_load = Some(NumberedLoadWork::ReadingScript {
                load,
                remaining,
                path,
                last_line,
                bytes_left,
            });
            return Ok(ScenarioStep::Request(request));
        }
        self.prepare_numbered_load_shaders(load)
    }

    fn prepare_numbered_load_shaders(
        &mut self,
        mut load: Box<PreparedLoad>,
    ) -> Result<ScenarioStep, ScriptIoError> {
        if let Some(assets) = load.file.shader_assets.take()
            && let Some(scene) = &self.scene
        {
            match scene.prepare_saved_shaders(&assets) {
                Ok(Some(shaders)) => {
                    load.shaders = Some(shaders);
                    return self.read_saved_shader(load, assets.into_iter().collect());
                }
                Ok(None) => {}
                Err(error) => {
                    let kind = if error == ProtocolError::Limit {
                        IoFailure::Limit
                    } else {
                        IoFailure::Denied
                    };
                    return Ok(self.fail_numbered_load(kind, error.to_string()));
                }
            }
        }
        self.prepare_numbered_load_scene(load)
    }

    fn read_saved_shader(
        &mut self,
        load: Box<PreparedLoad>,
        mut remaining: VecDeque<(String, String)>,
    ) -> Result<ScenarioStep, ScriptIoError> {
        if let Some((id, path)) = remaining.pop_front() {
            let request = IoRequest::Read { path: path.clone() };
            self.numbered_load = Some(NumberedLoadWork::ReadingSavedShader {
                load,
                remaining,
                id,
                path,
            });
            return Ok(ScenarioStep::Request(request));
        }
        self.prepare_numbered_load_scene(load)
    }

    fn prepare_numbered_load_scene(
        &mut self,
        load: Box<PreparedLoad>,
    ) -> Result<ScenarioStep, ScriptIoError> {
        if let (Some(scene), Some(snapshot)) = (&self.scene, &load.file.save.scene) {
            if !snapshot.valid_host_snapshot() {
                return Ok(self.fail_numbered_load(IoFailure::IoError, "invalid saved scene tree"));
            }
            let mut remaining =
                match scene.missing_snapshot_textures(snapshot, &load.file.layer_edits) {
                    Ok(files) => files,
                    Err(error) => {
                        let kind = if error == ProtocolError::Limit {
                            IoFailure::Limit
                        } else {
                            IoFailure::IoError
                        };
                        return Ok(self.fail_numbered_load(kind, error.to_string()));
                    }
                };
            if remaining.iter().any(|file| logical_key(file).is_err()) {
                return Ok(self.fail_numbered_load(IoFailure::Denied, "invalid saved texture path"));
            }
            if let Some(file) = remaining.pop_front() {
                return Ok(self.read_saved_scene_texture(load, remaining, file));
            }
        }
        self.restore_numbered_load(load)
    }

    fn read_saved_scene_texture(
        &mut self,
        load: Box<PreparedLoad>,
        remaining: VecDeque<String>,
        file: String,
    ) -> ScenarioStep {
        let (pending, request) = scene::TextureRead::start(file);
        self.numbered_load = Some(NumberedLoadWork::ReadingScene {
            load,
            remaining,
            pending,
        });
        ScenarioStep::Request(request)
    }

    fn restore_numbered_load(
        &mut self,
        load: Box<PreparedLoad>,
    ) -> Result<ScenarioStep, ScriptIoError> {
        let Some(Event::LoadGame { file, trans_type }) = &self.scenario_event else {
            return Err(ProtocolError::State.into());
        };
        let file = file.clone();
        let trans_type = *trans_type;
        if let Some(scene) = &self.scene
            && let Some(snapshot) = &load.file.save.message_text
            && let Some((pending, request)) = scene.prepare_saved_font(snapshot)
        {
            self.numbered_load = Some(NumberedLoadWork::ReadingSavedFont { load, pending });
            return Ok(ScenarioStep::Request(request));
        }
        let audio = if self.media_enabled
            && let Some(scene) = &self.scene
        {
            match self.audio.prepare_restore(
                scene,
                load.file.save.audio.as_ref(),
                self.video.is_active(),
                &self.transport,
            ) {
                Ok(audio) => audio,
                Err(ProtocolError::WouldBlock) => {
                    self.numbered_load = Some(NumberedLoadWork::ReadyToRestore(load));
                    return Ok(ScenarioStep::Progress(ScriptIoProgress::WouldBlock));
                }
                Err(error) => {
                    let kind = if error == ProtocolError::Limit {
                        IoFailure::Limit
                    } else {
                        IoFailure::Denied
                    };
                    return Ok(self.fail_numbered_load(kind, error.to_string()));
                }
            }
        } else {
            None
        };
        let PreparedLoad {
            file: HostSaveFile {
                save, layer_edits, ..
            },
            scripts,
            shaders,
        } = *load;
        let edits = if let Some(scene) = &self.scene {
            match scene.prepare_restored_edits(save.scene.as_ref(), layer_edits) {
                Ok(edits) => edits,
                Err(kind) => {
                    return Ok(self.fail_numbered_load(kind, "could not restore saved layer edits"));
                }
            }
        } else {
            scene::LayerEdits::default()
        };
        let stack = save.call_stack.into_iter().map(Into::into).collect();
        let result = self.interpreter.restore_position_with_host_scripts(
            &save.current_script,
            save.current_line,
            stack,
            scripts,
        );
        if let Err(error) = result {
            return Ok(self.fail_numbered_load(IoFailure::IoError, error.to_string()));
        }
        let mut variables = self.interpreter.variables();
        variables.restore_local_snapshot(&save.variables);
        self.interpreter.restore_variables(variables);
        if save
            .waiting_for_input
            .unwrap_or_else(|| crate::save::saved_position_resumes_input_wait(&self.interpreter))
        {
            self.interpreter.hold_queued_input_wait();
        }
        if let Some(scene) = &mut self.scene {
            scene.restore_snapshot(save.scene, shaders, edits, self.frame);
            scene.restore_text(save.message_text);
        }
        self.translations.clear(&mut self.transport);
        self.audio.commit_restore(audio, &mut self.transport)?;
        self.gameplay_checkpoint = None;
        self.clear_control_modes();
        self.video.reset_playback();
        self.begin_load_transition(trans_type);
        self.screenshot = Default::default();
        self.wait_start = None;
        self.scenario_event = None;
        self.start_numbered_load_hook(file)
    }

    fn start_numbered_load_hook(&mut self, file: String) -> Result<ScenarioStep, ScriptIoError> {
        if self.interpreter.prepare_host_handler()? == IoProgress::Waiting {
            self.numbered_load = Some(NumberedLoadWork::Initializing { file });
            return self.interpreter_request();
        }
        if self
            .interpreter
            .start_host_event_handler("onLoad", &HashMap::from([("file".into(), file)]))?
            == IoProgress::Waiting
        {
            self.numbered_load = Some(NumberedLoadWork::Hook);
            return self.interpreter_request();
        }
        Ok(self.finish_numbered_load(NumberedLoadResult::Restored))
    }

    fn fail_numbered_load(&mut self, kind: IoFailure, message: impl Into<String>) -> ScenarioStep {
        self.finish_numbered_load(NumberedLoadResult::Failed {
            kind,
            message: message.into(),
        })
    }

    fn finish_numbered_load(&mut self, result: NumberedLoadResult) -> ScenarioStep {
        self.numbered_load = Some(NumberedLoadWork::Finished(Some(result)));
        ScenarioStep::Progress(ScriptIoProgress::LoadReady)
    }

    fn step_event_filter(
        &mut self,
        mut task: IoTask,
        response: Option<IoResponse>,
    ) -> Result<ScenarioStep, ScriptIoError> {
        match task.step(response) {
            Ok(IoProgress::Waiting) => {
                let request = task.pending_request().ok_or(ProtocolError::State)?.clone();
                self.event_filter = Some(EventFilterWork::Running(task));
                Ok(ScenarioStep::Request(request))
            }
            Ok(IoProgress::Completed) => match self
                .interpreter
                .lua()
                .unpack_multi::<i32>(task.take_result().unwrap_or_default())
            {
                Ok(1) => self.finish_event_filter(EventFilterDecision::Handled),
                Ok(2) => self.finish_event_filter(EventFilterDecision::Rejected),
                Ok(_) => self.finish_event_filter(EventFilterDecision::DispatchDefault),
                Err(error) => self.reject_event_filter(error),
            },
            Err(error) => self.reject_event_filter(error),
        }
    }

    fn finish_event_filter(
        &mut self,
        decision: EventFilterDecision,
    ) -> Result<ScenarioStep, ScriptIoError> {
        if self
            .scene
            .as_mut()
            .is_some_and(|scene| scene.finish_pointer_filter(decision))
        {
            return Ok(ScenarioStep::Continue);
        }
        self.event_filter = Some(EventFilterWork::Ready(decision));
        Ok(ScenarioStep::Progress(ScriptIoProgress::EventFilterReady))
    }

    fn reject_event_filter(
        &mut self,
        error: impl fmt::Display,
    ) -> Result<ScenarioStep, ScriptIoError> {
        self.interpreter
            .engine_context()
            .lock()
            .unwrap()
            .callbacks
            .debug(0, &format!("eventFilter error: {error}"), false);
        self.finish_event_filter(EventFilterDecision::Rejected)
    }

    fn schedule_io(&mut self, request: IoRequest) -> Result<Option<IoResponse>, ProtocolError> {
        let path = match &request {
            IoRequest::Read { path }
            | IoRequest::Exists { path }
            | IoRequest::ModifiedTime { path }
            | IoRequest::Write { path, .. }
            | IoRequest::DeleteSave { path } => path,
            IoRequest::TransferSave {
                source,
                destination,
                remove_source,
            } => {
                let source = self.save_key(source)?;
                let destination = self.save_key(destination)?;
                if let Some((operation, work)) =
                    Self::save_transfer(source, destination, *remove_source)
                {
                    self.operation = Some(operation);
                    self.save_file_work = Some((save_files::SaveFileCaller::Lua, work));
                    return Ok(None);
                }
                return Ok(Some(IoResponse::Transferred));
            }
        };
        let normalized = logical_key(path)?;
        let (namespace, key) = match normalized.strip_prefix(&format!("{}/", self.save_prefix)) {
            Some(key) => ("save", key.to_string()),
            None => ("asset", normalized),
        };
        let operation = match request {
            IoRequest::Read { .. } => Operation::Stat {
                namespace,
                key,
                purpose: StatPurpose::Read,
            },
            IoRequest::Exists { .. } => Operation::Stat {
                namespace,
                key,
                purpose: StatPurpose::Exists,
            },
            IoRequest::ModifiedTime { .. } => Operation::Stat {
                namespace,
                key,
                purpose: StatPurpose::ModifiedTime,
            },
            IoRequest::Write { data, .. } if namespace == "save" => Operation::Write { key, data },
            IoRequest::Write { .. } => {
                return Err(ProtocolError::Argument(
                    "writes require the logical save prefix",
                ));
            }
            IoRequest::DeleteSave { .. } => Operation::Delete { key },
            IoRequest::TransferSave { .. } => unreachable!(),
        };
        self.operation = Some(operation);
        Ok(None)
    }

    fn save_key(&self, path: &str) -> Result<String, ProtocolError> {
        let normalized = logical_key(path)?;
        Ok(normalized
            .strip_prefix(&format!("{}/", self.save_prefix))
            .map(str::to_owned)
            .unwrap_or(normalized))
    }

    fn message(&self, operation: &Operation) -> (MessageKind, String, Vec<u8>) {
        match operation {
            Operation::Stat { namespace, key, .. } => (IO_STAT, json!({"namespace":namespace,"key":key}).to_string(), vec![]),
            Operation::Read { namespace, key, revision, size, data } => (IO_READ,
                json!({"namespace":namespace,"key":key,"revision":revision.to_string(),
                    "offset":data.len().to_string(),"length":(size-data.len()).min(self.limits().blob_bytes).to_string()}).to_string(), vec![]),
            Operation::Write { key, data } => (IO_WRITE_ATOMIC,
                write_chunks::write_metadata(key), data.clone()),
            Operation::ChunkedWrite(write) => write.message(),
            Operation::Delete { key } => (IO_DELETE,
                json!({"namespace":"save","key":key}).to_string(), vec![]),
        }
    }

    fn decode(&self, reply: &HostReply) -> Result<Completion, ProtocolError> {
        let metadata: Value =
            serde_json::from_str(reply.body.metadata()).map_err(|_| invalid_reply())?;
        let status = metadata
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(invalid_reply)?;
        let operation = self.operation.as_ref().ok_or(ProtocolError::State)?;
        if matches!(status, "ok" | "staged")
            && let Operation::ChunkedWrite(write) = operation
        {
            return write.decode(status, &metadata, reply.body.blob());
        }
        if status != "ok" {
            if !reply.body.blob().is_empty() {
                return Err(invalid_reply());
            }
            let kind = match status {
                "not_found" => IoFailure::NotFound,
                "denied" => IoFailure::Denied,
                "io_error" => IoFailure::IoError,
                "cancelled" => IoFailure::Cancelled,
                "unsupported" => IoFailure::Unsupported,
                _ => return Err(invalid_reply()),
            };
            let message = match metadata.get("message") {
                Some(value) => value.as_str().ok_or_else(invalid_reply)?.to_string(),
                None => status.to_string(),
            };
            return Ok(Completion::Failed(kind, message));
        }
        match operation {
            Operation::Stat { .. } => {
                if !reply.body.blob().is_empty() {
                    return Err(invalid_reply());
                }
                let found = metadata
                    .get("found")
                    .and_then(Value::as_bool)
                    .ok_or_else(invalid_reply)?;
                let (size, revision) = if found {
                    (decimal(&metadata, "size")?, decimal(&metadata, "revision")?)
                } else {
                    (0, 0)
                };
                Ok(Completion::Stat {
                    found,
                    size,
                    revision,
                    modified_utc_ms: if found {
                        optional_signed_decimal(&metadata, "modified_utc_ms")?
                    } else {
                        None
                    },
                })
            }
            Operation::Read {
                revision,
                size,
                data,
                ..
            } => {
                let eof = metadata
                    .get("eof")
                    .and_then(Value::as_bool)
                    .ok_or_else(invalid_reply)?;
                let count = reply.body.blob().len();
                let remaining = size - data.len();
                if decimal(&metadata, "revision")? != *revision
                    || count > remaining.min(self.limits().blob_bytes)
                    || eof != (count == remaining)
                    || count == 0
                {
                    return Err(invalid_reply());
                }
                Ok(Completion::Read)
            }
            Operation::Write { .. } => {
                if !reply.body.blob().is_empty() {
                    return Err(invalid_reply());
                }
                Ok(Completion::Written)
            }
            Operation::ChunkedWrite(_) => Err(invalid_reply()),
            Operation::Delete { .. } => {
                if !reply.body.blob().is_empty() {
                    return Err(invalid_reply());
                }
                Ok(Completion::Deleted)
            }
        }
    }

    fn apply(&mut self, reply: HostReply) -> Result<Option<IoResponse>, ProtocolError> {
        let completion = self.decode(&reply)?;
        let operation = self.operation.take().ok_or(ProtocolError::State)?;
        match (operation, completion) {
            (_, Completion::Failed(kind, message)) => Ok(Some(failed(kind, message))),
            (
                Operation::Stat {
                    purpose: StatPurpose::Exists,
                    ..
                },
                Completion::Stat { found, .. },
            ) => Ok(Some(IoResponse::Exists(found))),
            (
                Operation::Stat {
                    purpose: StatPurpose::ModifiedTime,
                    ..
                },
                Completion::Stat {
                    modified_utc_ms, ..
                },
            ) => Ok(Some(IoResponse::ModifiedTime(modified_utc_ms))),
            (Operation::Stat { .. }, Completion::Stat { found: false, .. }) => {
                Ok(Some(failed(IoFailure::NotFound, "file not found")))
            }
            (Operation::Stat { namespace, key, .. }, Completion::Stat { size, revision, .. }) => {
                let Ok(size) = usize::try_from(size) else {
                    return Ok(Some(failed(
                        IoFailure::Limit,
                        "file exceeds addressable memory",
                    )));
                };
                if size > self.max_file_bytes {
                    return Ok(Some(failed(IoFailure::Limit, "file exceeds read budget")));
                }
                let mut data = Vec::new();
                if data.try_reserve_exact(size).is_err() {
                    return Ok(Some(failed(IoFailure::Limit, "file allocation failed")));
                }
                if size == 0 {
                    return Ok(Some(IoResponse::Read(data)));
                }
                self.operation = Some(Operation::Read {
                    namespace,
                    key,
                    revision,
                    size,
                    data,
                });
                Ok(None)
            }
            (
                Operation::Read {
                    namespace,
                    key,
                    revision,
                    size,
                    mut data,
                },
                Completion::Read,
            ) => {
                data.extend_from_slice(reply.body.blob());
                if data.len() == size {
                    return Ok(Some(IoResponse::Read(data)));
                }
                self.operation = Some(Operation::Read {
                    namespace,
                    key,
                    revision,
                    size,
                    data,
                });
                Ok(None)
            }
            (Operation::ChunkedWrite(mut write), Completion::WriteStaged) => {
                write.advance();
                self.operation = Some(Operation::ChunkedWrite(write));
                Ok(None)
            }
            (Operation::Write { .. } | Operation::ChunkedWrite(_), Completion::Written) => {
                Ok(Some(IoResponse::Written))
            }
            (Operation::Delete { .. }, Completion::Deleted) => Ok(Some(IoResponse::Deleted)),
            _ => Err(invalid_reply()),
        }
    }
}

fn invalid_reply() -> ProtocolError {
    ProtocolError::Argument("invalid Host IO result payload")
}
fn failed(kind: IoFailure, message: impl Into<String>) -> IoResponse {
    IoResponse::Failed {
        kind,
        message: message.into(),
    }
}
fn decimal(metadata: &Value, key: &str) -> Result<u64, ProtocolError> {
    let text = metadata
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(invalid_reply)?;
    let value = text.parse::<u64>().map_err(|_| invalid_reply())?;
    if value.to_string() != text {
        return Err(invalid_reply());
    }
    Ok(value)
}
fn optional_signed_decimal(metadata: &Value, key: &str) -> Result<Option<i64>, ProtocolError> {
    let Some(value) = metadata.get(key).filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let text = value.as_str().ok_or_else(invalid_reply)?;
    let value = text.parse::<i64>().map_err(|_| invalid_reply())?;
    if value.to_string() != text {
        return Err(invalid_reply());
    }
    Ok(Some(value))
}
fn logical_key(path: &str) -> Result<String, ProtocolError> {
    let path = path.replace('\\', "/");
    if path.starts_with('/')
        || path.contains(['\0', ':'])
        || path.split('/').any(|part| part == "..")
    {
        return Err(ProtocolError::Argument("invalid logical resource key"));
    }
    let key = path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/")
        .to_ascii_lowercase();
    if key.is_empty() {
        return Err(ProtocolError::Argument("empty logical resource key"));
    }
    Ok(key)
}

#[cfg(test)]
mod tests;
