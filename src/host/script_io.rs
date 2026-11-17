//! Luau execution over the Host IO request transport. The Host owns all
//! storage; this driver never installs a physical file loader or renderer.

use super::protocol::*;
use asb_interpreter::io_continuation::{
    self, IoFailure, IoProgress, IoRequest, IoResponse, IoTask,
};
use asb_interpreter::{
    CallbackResult, Event, ExecutionResult, HostTime, Interpreter, InterpreterConfig,
};
use serde_json::{Value, json};
use std::collections::HashMap;
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

#[derive(Debug)]
pub enum ScriptIoError {
    Protocol(ProtocolError),
    Script(asb_interpreter::Error),
}

impl fmt::Display for ScriptIoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(error) => error.fmt(f),
            Self::Script(error) => error.fmt(f),
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
pub enum NumberedSaveResult {
    Written,
    Failed { kind: IoFailure, message: String },
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

enum EventFilterWork {
    Queued(String, HashMap<String, String>),
    Running(IoTask),
    Ready(EventFilterDecision),
}

enum NumberedSaveWork {
    Queued { key: String },
    Hook { key: String },
    Writing,
    Finished(Option<NumberedSaveResult>),
}

enum NumberedLoadWork {
    Queued { key: String },
    ReadingSave,
    ReadingTarget(Box<crate::save::SaveData>),
    Initializing { file: String },
    Hook,
    Finished(Option<NumberedLoadResult>),
}

enum ScenarioStep {
    Request(IoRequest),
    Progress(ScriptIoProgress),
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
    completed: bool,
    faulted: bool,
    cancelled: bool,
    mode: ExecutionMode,
    scenario_event: Option<Event>,
    host_time: HostTime,
    random_seed: u64,
    frame: Option<HostFrame>,
    frame_handler_due: bool,
    event_filter: Option<EventFilterWork>,
    numbered_save: Option<NumberedSaveWork>,
    numbered_load: Option<NumberedLoadWork>,
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
            completed: false,
            faulted: false,
            cancelled: false,
            mode: ExecutionMode::Idle,
            scenario_event: None,
            host_time: HostTime::default(),
            random_seed: 0,
            frame: None,
            frame_handler_due: false,
            event_filter: None,
            numbered_save: None,
            numbered_load: None,
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
        self.event_filter = None;
        self.numbered_save = None;
        self.numbered_load = None;
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

    /// Start a fresh VM by reading BOOT and the optional default macro file
    /// through Host IO. Parsing and loading errors are reported by `step`.
    pub fn open_boot(&mut self, name: &str) -> Result<u64, ScriptIoError> {
        logical_key(name)?;
        let mut interpreter = self.scenario_interpreter()?;
        interpreter.boot(name)?;
        self.replace_scenario(interpreter)
    }

    fn scenario_interpreter(&self) -> Result<Interpreter, ScriptIoError> {
        let mut interpreter = Interpreter::new(self.interpreter.config().clone());
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
        self.event_filter = None;
        self.numbered_save = None;
        self.numbered_load = None;
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
            || self.numbered_save.is_some()
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

    /// Persist the interpreter snapshot for a pending [save file=...] event.
    /// The snapshot includes the handler's data updates and resumes at the
    /// position after the save event. Only step runs the handler or writes.
    pub fn submit_numbered_save(&mut self) -> Result<(), ProtocolError> {
        if self.faulted || self.numbered_save.is_some() {
            return Err(ProtocolError::State);
        }
        let Some(Event::SaveGame { file }) = &self.scenario_event else {
            return Err(ProtocolError::State);
        };
        if self.event_filter.is_some() || self.operation.is_some() || self.cancelled {
            return Err(ProtocolError::WouldBlock);
        }
        let key = self.save_key(file)?;
        self.numbered_save = Some(NumberedSaveWork::Queued { key });
        Ok(())
    }

    pub fn take_numbered_save_result(&mut self) -> Option<NumberedSaveResult> {
        match &mut self.numbered_save {
            Some(NumberedSaveWork::Finished(result)) => result.take(),
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
            || self
                .numbered_save
                .as_ref()
                .is_some_and(|work| !matches!(work, NumberedSaveWork::Finished(None)))
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
        self.numbered_save = None;
        self.numbered_load = None;
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
            if Some(reply.identity) != self.identity {
                return Err(ProtocolError::Stale);
            }
            self.decode(reply)?;
        }
        self.transport.submit_replies(replies)
    }

    pub fn cancel_pending(&mut self) -> Result<(), ProtocolError> {
        if self.operation.is_none() {
            return Err(ProtocolError::State);
        }
        if let Some(identity) = self.identity {
            self.transport.cancel(identity)?;
        }
        self.identity = None;
        self.operation = None;
        // The Lua error branch is resumed only at the next step boundary.
        self.cancelled = true;
        Ok(())
    }

    pub fn step(&mut self) -> Result<ScriptIoProgress, ScriptIoError> {
        if self.faulted || self.mode == ExecutionMode::Idle {
            return Err(ProtocolError::State.into());
        }
        if self.completed {
            return Ok(ScriptIoProgress::Completed);
        }
        if self.scenario_event.is_some()
            && !self.frame_handler_due
            && self.interpreter.pending_host_io().is_none()
            && self.event_filter.is_none()
            && self.numbered_save.is_none()
            && self.numbered_load.is_none()
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
        if self.completed {
            return Ok(ScriptIoProgress::Completed);
        }
        let new_frame = self.frame.is_none_or(|previous| frame.id != previous.id);
        if new_frame
            && (self.operation.is_some()
                || self.cancelled
                || self.numbered_save.is_some()
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
            self.interpreter.begin_frame();
            self.frame_handler_due = !matches!(
                self.scenario_event,
                Some(Event::ShowDialog { .. } | Event::YesNo { .. })
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
        if self.identity.is_some() {
            let Some(reply) = self.transport.pop_reply() else {
                return Ok(ScriptIoProgress::Waiting);
            };
            response = self.apply(reply)?;
            self.identity = None;
        }
        loop {
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
                },
                ExecutionMode::Idle => return Err(ProtocolError::State.into()),
            };
            match self.operation_for(request) {
                Ok(operation) => self.operation = Some(operation),
                Err(error) => response = Some(failed(IoFailure::Denied, error.to_string())),
            }
        }
    }

    fn step_scenario(
        &mut self,
        response: Option<IoResponse>,
    ) -> Result<ScenarioStep, ScriptIoError> {
        if let Some(work) = self.numbered_save.take() {
            return self.step_numbered_save(work, response);
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
        if self.scenario_event.is_some() {
            return Ok(ScenarioStep::Progress(ScriptIoProgress::Event));
        }
        match self.interpreter.step()? {
            ExecutionResult::HostIoPending => self.interpreter_request(),
            ExecutionResult::Wait(event) => {
                self.scenario_event = Some(event);
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

    fn step_numbered_save(
        &mut self,
        work: NumberedSaveWork,
        response: Option<IoResponse>,
    ) -> Result<ScenarioStep, ScriptIoError> {
        let key = match work {
            NumberedSaveWork::Queued { key } => {
                let Some(Event::SaveGame { file }) = &self.scenario_event else {
                    return Err(ProtocolError::State.into());
                };
                if let Err(error) = self.interpreter.start_host_save_hook(file) {
                    return self.fail_save_hook(error);
                }
                key
            }
            NumberedSaveWork::Hook { key } => {
                if let Some(response) = response
                    && let Err(error) = self.interpreter.resume_host_io(response)
                {
                    return self.fail_save_hook(error);
                }
                key
            }
            NumberedSaveWork::Writing => {
                let result = match response {
                    Some(IoResponse::Written) => NumberedSaveResult::Written,
                    Some(IoResponse::Failed { kind, message }) => {
                        NumberedSaveResult::Failed { kind, message }
                    }
                    _ => return Err(ProtocolError::State.into()),
                };
                return Ok(self.finish_numbered_save(result));
            }
            NumberedSaveWork::Finished(result) => {
                let progress = if result.is_some() {
                    ScriptIoProgress::SaveReady
                } else {
                    ScriptIoProgress::Event
                };
                self.numbered_save = Some(NumberedSaveWork::Finished(result));
                return Ok(ScenarioStep::Progress(progress));
            }
        };
        match self.interpreter.finish_host_save_hook() {
            Ok(IoProgress::Completed) => {}
            Ok(IoProgress::Waiting) => {
                self.numbered_save = Some(NumberedSaveWork::Hook { key });
                return self.interpreter_request();
            }
            Err(error) => return self.fail_save_hook(error),
        }
        let mut save = crate::save::SaveData::from_interpreter(&self.interpreter);
        save.current_line = self.interpreter.position_after_event();
        save.waiting_for_input = Some(false);
        let data = match serde_json::to_vec(&save) {
            Ok(data) => data,
            Err(error) => return self.fail_save_hook(error),
        };
        if data.len() > self.max_file_bytes {
            return Ok(self.finish_numbered_save(NumberedSaveResult::Failed {
                kind: IoFailure::Limit,
                message: "save snapshot exceeds file budget".into(),
            }));
        }
        self.numbered_save = Some(NumberedSaveWork::Writing);
        Ok(ScenarioStep::Request(IoRequest::Write {
            path: format!("{}/{key}", self.save_prefix),
            data,
        }))
    }

    fn fail_save_hook(&mut self, error: impl fmt::Display) -> Result<ScenarioStep, ScriptIoError> {
        self.interpreter.abort_host_save_hook();
        Ok(self.finish_numbered_save(NumberedSaveResult::Failed {
            kind: IoFailure::IoError,
            message: error.to_string(),
        }))
    }

    fn finish_numbered_save(&mut self, result: NumberedSaveResult) -> ScenarioStep {
        self.numbered_save = Some(NumberedSaveWork::Finished(Some(result)));
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
            NumberedLoadWork::ReadingSave | NumberedLoadWork::ReadingTarget(_) => {
                let bytes = match response {
                    Some(IoResponse::Read(bytes)) => bytes,
                    Some(IoResponse::Failed { kind, message }) => {
                        return Ok(self.fail_numbered_load(kind, message));
                    }
                    _ => return Err(ProtocolError::State.into()),
                };
                if let NumberedLoadWork::ReadingTarget(save) = work {
                    return self.restore_numbered_load(*save, Some(&bytes));
                }
                let save: crate::save::SaveData = match serde_json::from_slice(&bytes) {
                    Ok(save) => save,
                    Err(error) => {
                        return Ok(self.fail_numbered_load(IoFailure::IoError, error.to_string()));
                    }
                };
                if save.version > crate::save::SAVE_FORMAT_VERSION
                    || save.scene.is_some()
                    || save.audio.is_some()
                    || save.message_text.is_some()
                {
                    return Ok(self.fail_numbered_load(
                        IoFailure::Unsupported,
                        "unsupported save version or runtime snapshot",
                    ));
                }
                if logical_key(&save.current_script).is_err()
                    || save.call_stack.iter().any(|frame| {
                        logical_key(&frame.script).is_err()
                            || self
                                .interpreter
                                .get_script(&frame.script)
                                .is_some_and(|script| frame.return_line > script.len())
                    })
                {
                    return Ok(self.fail_numbered_load(
                        IoFailure::IoError,
                        "invalid saved script or call frame",
                    ));
                }
                if let Some(script) = self.interpreter.get_script(&save.current_script) {
                    if save.current_line > script.len() {
                        return Ok(self.fail_numbered_load(
                            IoFailure::IoError,
                            "save position is beyond the script",
                        ));
                    }
                    self.restore_numbered_load(save, None)
                } else {
                    let path = save.current_script.clone();
                    self.numbered_load = Some(NumberedLoadWork::ReadingTarget(Box::new(save)));
                    Ok(ScenarioStep::Request(IoRequest::Read { path }))
                }
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

    fn restore_numbered_load(
        &mut self,
        save: crate::save::SaveData,
        source: Option<&[u8]>,
    ) -> Result<ScenarioStep, ScriptIoError> {
        let Some(Event::LoadGame { file, .. }) = &self.scenario_event else {
            return Err(ProtocolError::State.into());
        };
        let file = file.clone();
        let stack = save.call_stack.into_iter().map(Into::into).collect();
        let result = if let Some(source) = source {
            self.interpreter.restore_position_from_file(
                &save.current_script,
                source,
                save.current_line,
                stack,
            )
        } else {
            self.interpreter
                .restore_position(&save.current_script, save.current_line, stack)
        };
        if let Err(error) = result {
            return Ok(self.fail_numbered_load(IoFailure::IoError, error.to_string()));
        }
        let mut variables = self.interpreter.variables();
        variables.restore_local_snapshot(&save.variables);
        self.interpreter.restore_variables(variables);
        if save.waiting_for_input == Some(true) {
            self.interpreter.hold_queued_input_wait();
        }
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

    fn operation_for(&self, request: IoRequest) -> Result<Operation, ProtocolError> {
        let path = match &request {
            IoRequest::Read { path }
            | IoRequest::Exists { path }
            | IoRequest::ModifiedTime { path }
            | IoRequest::Write { path, .. }
            | IoRequest::DeleteSave { path } => path,
        };
        let normalized = logical_key(path)?;
        let (namespace, key) = match normalized.strip_prefix(&format!("{}/", self.save_prefix)) {
            Some(key) => ("save", key.to_string()),
            None => ("asset", normalized),
        };
        match request {
            IoRequest::Read { .. } => Ok(Operation::Stat {
                namespace,
                key,
                purpose: StatPurpose::Read,
            }),
            IoRequest::Exists { .. } => Ok(Operation::Stat {
                namespace,
                key,
                purpose: StatPurpose::Exists,
            }),
            IoRequest::ModifiedTime { .. } => Ok(Operation::Stat {
                namespace,
                key,
                purpose: StatPurpose::ModifiedTime,
            }),
            IoRequest::Write { data, .. } if namespace == "save" => {
                Ok(Operation::Write { key, data })
            }
            IoRequest::Write { .. } => Err(ProtocolError::Argument(
                "writes require the logical save prefix",
            )),
            IoRequest::DeleteSave { .. } => Ok(Operation::Delete { key }),
        }
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
                json!({"namespace":"save","key":key}).to_string(), data.clone()),
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
        match self.operation.as_ref().ok_or(ProtocolError::State)? {
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
            (Operation::Write { .. }, Completion::Written) => Ok(Some(IoResponse::Written)),
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
