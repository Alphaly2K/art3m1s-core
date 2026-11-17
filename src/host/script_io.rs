//! Luau execution over the Host IO request transport. The Host owns all
//! storage; this driver never installs a physical file loader or renderer.

use super::protocol::*;
use asb_interpreter::io_continuation::{
    self, IoFailure, IoProgress, IoRequest, IoResponse, IoTask,
};
use asb_interpreter::{CallbackResult, Event, ExecutionResult, Interpreter, InterpreterConfig};
use serde_json::{Value, json};
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
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExecutionMode {
    Idle,
    Chunk,
    Scenario,
}

enum Operation {
    Stat {
        namespace: &'static str,
        key: String,
        exists_only: bool,
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
}

enum Completion {
    Stat {
        found: bool,
        size: u64,
        revision: u64,
    },
    Read,
    Written,
    Failed(IoFailure, String),
}

/// Script code runs only in `step`, including after result submission. Scenario
/// events remain pending until the Host resumes them; frame scheduling and the
/// C facade are separate.
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
        let interpreter = Interpreter::new(config);
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
        let mut interpreter = Interpreter::new(self.interpreter.config().clone());
        interpreter.enable_resumable_io()?;
        interpreter.set_callback(|_| CallbackResult::Pause);
        interpreter.load_file(name, source)?;
        interpreter.start(name, label)?;
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
        Ok(epoch)
    }

    pub fn pending_event(&self) -> Option<&Event> {
        self.scenario_event.as_ref()
    }

    pub fn resume_scenario(&mut self) -> Result<(), ProtocolError> {
        if self.scenario_event.take().is_none() {
            return Err(ProtocolError::State);
        }
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
        if self.scenario_event.is_some() {
            return Ok(ScriptIoProgress::Event);
        }
        let result = self.step_inner();
        if result.is_err() {
            self.faulted = true;
        }
        result
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
                        self.completed = true;
                        return Ok(ScriptIoProgress::Completed);
                    }
                    task.pending_request().unwrap().clone()
                }
                ExecutionMode::Scenario => {
                    if let Some(response) = response.take() {
                        self.interpreter.resume_host_io(response)?;
                    }
                    match self.interpreter.step()? {
                        ExecutionResult::HostIoPending => self
                            .interpreter
                            .pending_host_io()
                            .ok_or(ProtocolError::State)?
                            .clone(),
                        ExecutionResult::Wait(event) => {
                            self.scenario_event = Some(event);
                            return Ok(ScriptIoProgress::Event);
                        }
                        ExecutionResult::Completed => {
                            self.completed = true;
                            return Ok(ScriptIoProgress::Completed);
                        }
                        _ => return Err(ProtocolError::State.into()),
                    }
                }
                ExecutionMode::Idle => return Err(ProtocolError::State.into()),
            };
            match self.operation_for(request) {
                Ok(operation) => self.operation = Some(operation),
                Err(error) => response = Some(failed(IoFailure::Denied, error.to_string())),
            }
        }
    }

    fn operation_for(&self, request: IoRequest) -> Result<Operation, ProtocolError> {
        let path = match &request {
            IoRequest::Read { path }
            | IoRequest::Exists { path }
            | IoRequest::Write { path, .. } => path,
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
                exists_only: false,
            }),
            IoRequest::Exists { .. } => Ok(Operation::Stat {
                namespace,
                key,
                exists_only: true,
            }),
            IoRequest::Write { data, .. } if namespace == "save" => {
                Ok(Operation::Write { key, data })
            }
            IoRequest::Write { .. } => Err(ProtocolError::Argument(
                "writes require the logical save prefix",
            )),
        }
    }

    fn message(&self, operation: &Operation) -> (MessageKind, String, Vec<u8>) {
        match operation {
            Operation::Stat { namespace, key, .. } => (IO_STAT, json!({"namespace":namespace,"key":key}).to_string(), vec![]),
            Operation::Read { namespace, key, revision, size, data } => (IO_READ,
                json!({"namespace":namespace,"key":key,"revision":revision.to_string(),
                    "offset":data.len().to_string(),"length":(size-data.len()).min(self.limits().blob_bytes).to_string()}).to_string(), vec![]),
            Operation::Write { key, data } => (IO_WRITE_ATOMIC,
                json!({"namespace":"save","key":key}).to_string(), data.clone()),
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
        }
    }

    fn apply(&mut self, reply: HostReply) -> Result<Option<IoResponse>, ProtocolError> {
        let completion = self.decode(&reply)?;
        let operation = self.operation.take().ok_or(ProtocolError::State)?;
        match (operation, completion) {
            (_, Completion::Failed(kind, message)) => Ok(Some(failed(kind, message))),
            (
                Operation::Stat {
                    exists_only: true, ..
                },
                Completion::Stat { found, .. },
            ) => Ok(Some(IoResponse::Exists(found))),
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
