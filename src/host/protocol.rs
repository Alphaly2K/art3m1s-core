//! Bounded, callback-free session transport for the v2 protocol.
//!
//! The runtime facade supplies a validated runtime handle and consumes accepted
//! replies at a step boundary. Enqueue reliable output before committing the
//! operation that produces it; [`ProtocolError::WouldBlock`] accepts nothing.
//! This module performs no IO and does not dispatch scripts or service bodies.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;

pub const ENVELOPE_VERSION: u16 = 2;
pub const HEADER_BYTES: usize = 56;
pub const FLAG_REQUIRES_REPLY: u32 = 1;
pub const FLAG_CRITICAL: u32 = 2;
pub const FLAG_JSON_BLOB: u32 = 1 << 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum Domain {
    Runtime = 1,
    Input = 2,
    Io = 3,
    Ui = 4,
    Media = 5,
    Render = 6,
    Diagnostics = 7,
}

/// Service schemas define the meaning of each nonzero kind within its domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MessageKind {
    pub domain: Domain,
    pub kind: u32,
}

pub const SESSION_CHANGED: MessageKind = MessageKind {
    domain: Domain::Runtime,
    kind: 1,
};
pub const PROJECT_READY: MessageKind = MessageKind {
    domain: Domain::Runtime,
    kind: 2,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    Argument(&'static str),
    State,
    Stale,
    Limit,
    WouldBlock,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Argument(reason) => write!(f, "invalid protocol argument: {reason}"),
            Self::State => f.write_str("operation is not valid in the current session state"),
            Self::Stale => f.write_str("request identity is stale or does not match"),
            Self::Limit => f.write_str("protocol limit exhausted or message exceeds budget"),
            Self::WouldBlock => f.write_str("queue capacity is temporarily unavailable"),
        }
    }
}

impl std::error::Error for ProtocolError {}

/// UTF-8 JSON object followed by a raw binary blob. Schema-specific validation
/// belongs to the service dispatcher; 64-bit JSON fields use decimal strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageBody {
    metadata: Box<str>,
    blob: Box<[u8]>,
}

impl MessageBody {
    pub fn new(metadata: String, blob: Vec<u8>) -> Result<Self, ProtocolError> {
        let body = Self {
            metadata: metadata.into_boxed_str(),
            blob: blob.into_boxed_slice(),
        };
        body.record_bytes()?;
        let value: serde_json::Value = serde_json::from_str(&body.metadata)
            .map_err(|_| ProtocolError::Argument("metadata must be a JSON object"))?;
        if !value.is_object() {
            return Err(ProtocolError::Argument("metadata must be a JSON object"));
        }
        Ok(body)
    }

    pub fn metadata(&self) -> &str {
        &self.metadata
    }

    pub fn blob(&self) -> &[u8] {
        &self.blob
    }

    fn record_bytes(&self) -> Result<usize, ProtocolError> {
        HEADER_BYTES
            .checked_add(8)
            .and_then(|len| len.checked_add(self.metadata.len()))
            .and_then(|len| len.checked_add(self.blob.len()))
            .filter(|&len| len <= u32::MAX as usize)
            .ok_or(ProtocolError::Limit)
    }
}

impl Default for MessageBody {
    fn default() -> Self {
        Self {
            metadata: "{}".into(),
            blob: Box::default(),
        }
    }
}

/// All four fields must match the original request, including its service kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RequestIdentity {
    pub runtime: u64,
    pub epoch: u64,
    pub request_id: u64,
    pub service: MessageKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostReply {
    pub identity: RequestIdentity,
    pub body: MessageBody,
}

/// Bytes include the encoded envelope, body lengths, metadata and blob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueBudget {
    pub records: usize,
    pub bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransportLimits {
    pub control: QueueBudget,
    pub diagnostics: QueueBudget,
    pub replies: QueueBudget,
    pub pending_requests: usize,
    pub batch_messages: usize,
    pub metadata_bytes: usize,
    pub blob_bytes: usize,
}

impl Default for TransportLimits {
    fn default() -> Self {
        Self {
            control: QueueBudget {
                records: 4096,
                bytes: 16 * 1024 * 1024,
            },
            diagnostics: QueueBudget {
                records: 1024,
                bytes: 1024 * 1024,
            },
            replies: QueueBudget {
                records: 4096,
                bytes: 16 * 1024 * 1024,
            },
            pending_requests: 4096,
            batch_messages: 4096,
            metadata_bytes: 1024 * 1024,
            blob_bytes: 1024 * 1024,
        }
    }
}

impl TransportLimits {
    fn validate(self) -> Result<(), ProtocolError> {
        let minimum = MessageBody::default().record_bytes()?;
        for budget in [self.control, self.diagnostics, self.replies] {
            if budget.records == 0 || budget.records > u32::MAX as usize || budget.bytes < minimum {
                return Err(ProtocolError::Argument("invalid queue budget"));
            }
        }
        if self.pending_requests == 0
            || self.batch_messages == 0
            || self.batch_messages > u32::MAX as usize
            || self.metadata_bytes < 2
            || self.metadata_bytes > u32::MAX as usize
            || self.blob_bytes > u32::MAX as usize
        {
            return Err(ProtocolError::Argument("invalid message limit"));
        }
        Ok(())
    }

    fn check_body(self, body: &MessageBody) -> Result<usize, ProtocolError> {
        if body.metadata.len() > self.metadata_bytes || body.blob.len() > self.blob_bytes {
            return Err(ProtocolError::Limit);
        }
        body.record_bytes()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollStatus {
    Ok,
    NoData,
    Buffer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PollResult {
    pub status: PollStatus,
    pub record_count: u32,
    pub written_bytes: usize,
    pub required_next_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticOutcome {
    Queued,
    Dropped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransportState {
    pub epoch: u64,
    pub pending_requests: usize,
    pub control: QueueBudget,
    pub diagnostics: QueueBudget,
    pub replies: QueueBudget,
    pub dropped_diagnostics: u64,
}

struct Envelope {
    kind: MessageKind,
    sequence: u64,
    request_id: u64,
    flags: u32,
    body: MessageBody,
    bytes: usize,
}

impl Envelope {
    fn encode(&self, runtime: u64, epoch: u64, output: &mut [u8]) {
        output[0..4].copy_from_slice(&(self.bytes as u32).to_le_bytes());
        output[4..6].copy_from_slice(&ENVELOPE_VERSION.to_le_bytes());
        output[6..8].copy_from_slice(&(self.kind.domain as u16).to_le_bytes());
        output[8..12].copy_from_slice(&self.kind.kind.to_le_bytes());
        output[12..16].copy_from_slice(&self.flags.to_le_bytes());
        output[16..24].copy_from_slice(&runtime.to_le_bytes());
        output[24..32].copy_from_slice(&epoch.to_le_bytes());
        output[32..40].copy_from_slice(&self.sequence.to_le_bytes());
        output[40..48].copy_from_slice(&self.request_id.to_le_bytes());
        output[48..52].copy_from_slice(&((self.bytes - HEADER_BYTES) as u32).to_le_bytes());
        output[52..56].fill(0);
        output[56..60].copy_from_slice(&(self.body.metadata.len() as u32).to_le_bytes());
        output[60..64].copy_from_slice(&(self.body.blob.len() as u32).to_le_bytes());
        let blob_start = 64 + self.body.metadata.len();
        output[64..blob_start].copy_from_slice(self.body.metadata.as_bytes());
        output[blob_start..self.bytes].copy_from_slice(&self.body.blob);
    }
}

#[derive(Default)]
struct EventQueue {
    records: VecDeque<Envelope>,
    bytes: usize,
}

impl EventQueue {
    fn usage(&self) -> QueueBudget {
        QueueBudget {
            records: self.records.len(),
            bytes: self.bytes,
        }
    }

    fn check_capacity(&self, budget: QueueBudget, bytes: usize) -> Result<(), ProtocolError> {
        if bytes > budget.bytes {
            return Err(ProtocolError::Limit);
        }
        if self.records.len() >= budget.records || bytes > budget.bytes - self.bytes {
            return Err(ProtocolError::WouldBlock);
        }
        Ok(())
    }

    fn push(&mut self, envelope: Envelope) {
        self.bytes += envelope.bytes;
        self.records.push_back(envelope);
    }

    fn pop(&mut self) -> Option<Envelope> {
        let record = self.records.pop_front()?;
        self.bytes -= record.bytes;
        Some(record)
    }
}

struct PendingRequest {
    service: MessageKind,
    delivered: bool,
}

/// One runtime's transport. The facade owns handle validation, owner-thread
/// checks and service schemas; this type enforces queue and request lifetimes.
pub struct SessionTransport {
    runtime: u64,
    epoch: u64,
    last_sequence: u64,
    last_request_id: u64,
    limits: TransportLimits,
    control: EventQueue,
    diagnostics: EventQueue,
    pending: HashMap<u64, PendingRequest>,
    replies: VecDeque<HostReply>,
    reply_bytes: usize,
    dropped_diagnostics: u64,
}

impl SessionTransport {
    pub fn new(runtime: u64, limits: TransportLimits) -> Result<Self, ProtocolError> {
        if runtime == 0 {
            return Err(ProtocolError::Argument("runtime handle must be nonzero"));
        }
        limits.validate()?;
        Ok(Self {
            runtime,
            epoch: 0,
            last_sequence: 0,
            last_request_id: 0,
            limits,
            control: EventQueue::default(),
            diagnostics: EventQueue::default(),
            pending: HashMap::new(),
            replies: VecDeque::new(),
            reply_bytes: 0,
            dropped_diagnostics: 0,
        })
    }

    pub fn limits(&self) -> TransportLimits {
        self.limits
    }

    pub fn state(&self) -> TransportState {
        TransportState {
            epoch: self.epoch,
            pending_requests: self.pending.len(),
            control: self.control.usage(),
            diagnostics: self.diagnostics.usage(),
            replies: QueueBudget {
                records: self.replies.len(),
                bytes: self.reply_bytes,
            },
            dropped_diagnostics: self.dropped_diagnostics,
        }
    }

    /// Atomically replace the project session. The first new control record is
    /// session_changed; old requests, accepted replies and undelivered output go
    /// away together. Runtime-lifetime IDs and diagnostic loss counts survive.
    pub fn begin_session(&mut self) -> Result<u64, ProtocolError> {
        let epoch = self.epoch.checked_add(1).ok_or(ProtocolError::Limit)?;
        let sequence = self.next_sequence()?;
        let body = MessageBody::default();
        let bytes = body.record_bytes()?;
        self.control = EventQueue::default();
        self.diagnostics = EventQueue::default();
        self.pending.clear();
        self.replies.clear();
        self.reply_bytes = 0;
        self.epoch = epoch;
        self.last_sequence = sequence;
        self.control.push(Envelope {
            kind: SESSION_CHANGED,
            sequence,
            request_id: 0,
            flags: FLAG_CRITICAL | FLAG_JSON_BLOB,
            body,
            bytes,
        });
        Ok(epoch)
    }

    /// Reserve and enqueue a reliable request before committing its operation.
    pub fn request(
        &mut self,
        service: MessageKind,
        body: MessageBody,
    ) -> Result<RequestIdentity, ProtocolError> {
        if !matches!(
            service.domain,
            Domain::Io | Domain::Ui | Domain::Media | Domain::Render
        ) {
            return Err(ProtocolError::Argument(
                "domain does not support service requests",
            ));
        }
        self.check_kind(service)?;
        let bytes = self.limits.check_body(&body)?;
        self.control.check_capacity(self.limits.control, bytes)?;
        if self.pending.len() >= self.limits.pending_requests {
            return Err(ProtocolError::WouldBlock);
        }
        let sequence = self.next_sequence()?;
        let request_id = self
            .last_request_id
            .checked_add(1)
            .ok_or(ProtocolError::Limit)?;
        self.pending.insert(
            request_id,
            PendingRequest {
                service,
                delivered: false,
            },
        );
        self.control.push(Envelope {
            kind: service,
            sequence,
            request_id,
            flags: FLAG_REQUIRES_REPLY | FLAG_CRITICAL | FLAG_JSON_BLOB,
            body,
            bytes,
        });
        self.last_sequence = sequence;
        self.last_request_id = request_id;
        Ok(RequestIdentity {
            runtime: self.runtime,
            epoch: self.epoch,
            request_id,
            service,
        })
    }

    pub fn notify(&mut self, kind: MessageKind, body: MessageBody) -> Result<(), ProtocolError> {
        let (bytes, sequence) = self.notification_slot(kind, &body)?;
        self.control.push(Envelope {
            kind,
            sequence,
            request_id: 0,
            flags: FLAG_CRITICAL | FLAG_JSON_BLOB,
            body,
            bytes,
        });
        self.last_sequence = sequence;
        Ok(())
    }

    pub(super) fn check_notify(
        &self,
        kind: MessageKind,
        body: &MessageBody,
    ) -> Result<(), ProtocolError> {
        self.notification_slot(kind, body).map(|_| ())
    }

    fn notification_slot(
        &self,
        kind: MessageKind,
        body: &MessageBody,
    ) -> Result<(usize, u64), ProtocolError> {
        self.check_kind(kind)?;
        if kind.domain == Domain::Diagnostics || kind == SESSION_CHANGED {
            return Err(ProtocolError::Argument("reserved notification kind"));
        }
        let bytes = self.limits.check_body(body)?;
        self.control.check_capacity(self.limits.control, bytes)?;
        Ok((bytes, self.next_sequence()?))
    }

    /// Diagnostics drop the newest record when either budget is full. They
    /// never evict reliable records or prevent a control enqueue.
    pub fn diagnostic(
        &mut self,
        kind: u32,
        body: MessageBody,
    ) -> Result<DiagnosticOutcome, ProtocolError> {
        let kind = MessageKind {
            domain: Domain::Diagnostics,
            kind,
        };
        self.check_kind(kind)?;
        let bytes = match self.limits.check_body(&body) {
            Ok(bytes)
                if self
                    .diagnostics
                    .check_capacity(self.limits.diagnostics, bytes)
                    .is_ok() =>
            {
                bytes
            }
            _ => {
                self.dropped_diagnostics = self.dropped_diagnostics.saturating_add(1);
                return Ok(DiagnosticOutcome::Dropped);
            }
        };
        let sequence = self.next_sequence()?;
        self.diagnostics.push(Envelope {
            kind,
            sequence,
            request_id: 0,
            flags: FLAG_JSON_BLOB,
            body,
            bytes,
        });
        self.last_sequence = sequence;
        Ok(DiagnosticOutcome::Queued)
    }

    /// Control records have delivery priority. Neither a short buffer nor a
    /// partial successful poll consumes the next record that did not fit.
    pub fn poll_events(&mut self, output: &mut [u8]) -> PollResult {
        let mut written = 0;
        let mut count = 0;
        loop {
            let queue = if self.control.records.is_empty() {
                &mut self.diagnostics
            } else {
                &mut self.control
            };
            let Some(record) = queue.records.front() else {
                break;
            };
            if count == u32::MAX || record.bytes > output.len() - written {
                break;
            }
            let record = queue.pop().unwrap();
            record.encode(
                self.runtime,
                self.epoch,
                &mut output[written..written + record.bytes],
            );
            if record.request_id != 0 {
                self.pending.get_mut(&record.request_id).unwrap().delivered = true;
            }
            written += record.bytes;
            count += 1;
        }
        let required = self
            .control
            .records
            .front()
            .or_else(|| self.diagnostics.records.front())
            .map_or(0, |record| record.bytes);
        PollResult {
            status: if count != 0 {
                PollStatus::Ok
            } else if required == 0 {
                PollStatus::NoData
            } else {
                PollStatus::Buffer
            },
            record_count: count,
            written_bytes: written,
            required_next_bytes: required,
        }
    }

    /// Validate and copy the entire reply batch before retiring any request.
    /// Accepted replies only become executable when the runtime drains them.
    pub fn submit_replies(&mut self, replies: &[HostReply]) -> Result<(), ProtocolError> {
        if replies.len() > self.limits.batch_messages || replies.len() > self.limits.replies.records
        {
            return Err(ProtocolError::Limit);
        }
        let mut seen = HashSet::new();
        let mut bytes = 0usize;
        for reply in replies {
            let pending = self.find_pending(reply.identity)?;
            if !pending.delivered {
                return Err(ProtocolError::State);
            }
            if !seen.insert(reply.identity.request_id) {
                return Err(ProtocolError::Stale);
            }
            bytes = bytes
                .checked_add(self.limits.check_body(&reply.body)?)
                .ok_or(ProtocolError::Limit)?;
        }
        if bytes > self.limits.replies.bytes {
            return Err(ProtocolError::Limit);
        }
        if replies.len() > self.limits.replies.records - self.replies.len()
            || bytes > self.limits.replies.bytes - self.reply_bytes
        {
            return Err(ProtocolError::WouldBlock);
        }
        for reply in replies {
            self.replies.push_back(reply.clone());
            self.pending.remove(&reply.identity.request_id);
        }
        self.reply_bytes += bytes;
        Ok(())
    }

    pub fn pop_reply(&mut self) -> Option<HostReply> {
        let reply = self.replies.pop_front()?;
        self.reply_bytes -= reply.body.record_bytes().unwrap();
        Some(reply)
    }

    pub(super) fn take_reply(&mut self, identity: RequestIdentity) -> Option<HostReply> {
        let index = self
            .replies
            .iter()
            .position(|reply| reply.identity == identity)?;
        let reply = self.replies.remove(index)?;
        self.reply_bytes -= reply.body.record_bytes().unwrap();
        Some(reply)
    }

    /// Retire locally; the Host still needs a service-specific cancellation
    /// notification if work has already been dispatched outside this transport.
    pub fn cancel(&mut self, identity: RequestIdentity) -> Result<(), ProtocolError> {
        self.find_pending(identity)?;
        self.pending.remove(&identity.request_id);
        self.control.records.retain(|record| {
            if record.request_id == identity.request_id {
                self.control.bytes -= record.bytes;
                false
            } else {
                true
            }
        });
        Ok(())
    }

    fn next_sequence(&self) -> Result<u64, ProtocolError> {
        self.last_sequence
            .checked_add(1)
            .ok_or(ProtocolError::Limit)
    }

    fn check_kind(&self, kind: MessageKind) -> Result<(), ProtocolError> {
        if kind.kind == 0 {
            return Err(ProtocolError::Argument("message kind must be nonzero"));
        }
        if self.epoch == 0 {
            return Err(ProtocolError::State);
        }
        Ok(())
    }

    fn find_pending(&self, identity: RequestIdentity) -> Result<&PendingRequest, ProtocolError> {
        if identity.runtime != self.runtime || identity.epoch != self.epoch {
            return Err(ProtocolError::Stale);
        }
        self.pending
            .get(&identity.request_id)
            .filter(|pending| pending.service == identity.service)
            .ok_or(ProtocolError::Stale)
    }
}

#[cfg(test)]
mod tests;
