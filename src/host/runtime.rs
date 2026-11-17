//! Artemis runtime handles for the queued Host interface.

use super::frame::FrameLease;
use super::input::HostInput;
use super::media::{PlaybackStatus, VideoFrame};
use super::protocol::{HostReply, PollResult, ProtocolError, TransportLimits, TransportState};
use super::script_io::{HostFrame, ScriptIoError, ScriptIoProgress, ScriptIoSession};
use std::collections::HashMap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, LazyLock, Mutex, TryLockError};
use std::thread::{self, ThreadId};

const ARTEMIS_RUNTIME_TAG: u64 = 0xA201 << 48;
const MAX_RUNTIME_SERIAL: u64 = (1 << 48) - 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct RuntimeHandle(u64);

impl RuntimeHandle {
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn as_raw(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimePhase {
    Created,
    Loading,
    Running,
    Closing,
    Closed,
    Faulted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeState {
    pub phase: RuntimePhase,
    pub transport: TransportState,
    pub host_frame_id: Option<u64>,
    pub latest_render_frame_id: Option<u64>,
    pub stage_size: Option<[u32; 2]>,
    pub waiting_for_io: bool,
    pub waiting_for_ui: bool,
    pub waiting_for_scenario: bool,
    pub exit_requested: bool,
    pub scenario_finished: bool,
}

#[derive(Debug)]
pub enum RuntimeError {
    Handle,
    Thread,
    Panic,
    Session(ScriptIoError),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Handle => f.write_str("invalid Artemis runtime handle"),
            Self::Thread => f.write_str("runtime must be accessed on its owner thread"),
            Self::Panic => f.write_str("Artemis runtime panicked"),
            Self::Session(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<ScriptIoError> for RuntimeError {
    fn from(error: ScriptIoError) -> Self {
        Self::Session(error)
    }
}

impl From<ProtocolError> for RuntimeError {
    fn from(error: ProtocolError) -> Self {
        ScriptIoError::from(error).into()
    }
}

struct Entry {
    owner: ThreadId,
    session: Arc<Mutex<ScriptIoSession>>,
}

#[derive(Default)]
struct Registry {
    serial: u64,
    entries: HashMap<RuntimeHandle, Entry>,
}

impl Registry {
    fn allocate(&mut self) -> Result<RuntimeHandle, ProtocolError> {
        if self.serial == MAX_RUNTIME_SERIAL {
            return Err(ProtocolError::Limit);
        }
        self.serial += 1;
        Ok(RuntimeHandle(ARTEMIS_RUNTIME_TAG | self.serial))
    }

    fn owned_entry(&self, handle: RuntimeHandle) -> Result<&Entry, RuntimeError> {
        let entry = self.entries.get(&handle).ok_or(RuntimeError::Handle)?;
        if entry.owner != thread::current().id() {
            return Err(RuntimeError::Thread);
        }
        Ok(entry)
    }
}

static RUNTIMES: LazyLock<Mutex<Registry>> = LazyLock::new(|| Mutex::new(Registry::default()));

/// Create a runtime owned by the calling thread. Destroy it before that thread exits.
pub fn create(
    random_seed: u64,
    limits: TransportLimits,
    max_file_bytes: usize,
) -> Result<RuntimeHandle, RuntimeError> {
    create_with_transition(random_seed, limits, max_file_bytes, true)
}

pub(super) fn create_with_transition(
    random_seed: u64,
    limits: TransportLimits,
    max_file_bytes: usize,
    transitions_enabled: bool,
) -> Result<RuntimeHandle, RuntimeError> {
    let handle = RUNTIMES.lock().unwrap().allocate()?;
    let session = catch_unwind(|| {
        let mut session =
            ScriptIoSession::new(handle.0, Default::default(), limits, max_file_bytes)?;
        session.set_random_seed(random_seed)?;
        session.transitions_enabled = transitions_enabled;
        Ok::<_, ScriptIoError>(session)
    })
    .map_err(|_| RuntimeError::Panic)??;
    RUNTIMES.lock().unwrap().entries.insert(
        handle,
        Entry {
            owner: thread::current().id(),
            session: Arc::new(Mutex::new(session)),
        },
    );
    Ok(handle)
}

/// Release the runtime and invalidate its handle and outstanding requests.
pub fn destroy(handle: RuntimeHandle) -> Result<(), RuntimeError> {
    with_session(handle, |session| {
        if session.frames.has_leases() {
            return Err(ProtocolError::State.into());
        }
        Ok(())
    })?;
    let entry = {
        let mut registry = RUNTIMES.lock().unwrap();
        if Arc::strong_count(&registry.owned_entry(handle)?.session) != 1 {
            return Err(ProtocolError::State.into());
        }
        registry.entries.remove(&handle).unwrap()
    };
    catch_unwind(AssertUnwindSafe(|| drop(entry))).map_err(|_| RuntimeError::Panic)
}

fn with_session<T>(
    handle: RuntimeHandle,
    f: impl FnOnce(&mut ScriptIoSession) -> Result<T, RuntimeError>,
) -> Result<T, RuntimeError> {
    let session = RUNTIMES
        .lock()
        .unwrap()
        .owned_entry(handle)?
        .session
        .clone();
    let mut session = match session.try_lock() {
        Ok(session) => session,
        Err(TryLockError::WouldBlock) => return Err(ProtocolError::State.into()),
        Err(TryLockError::Poisoned(_)) => return Err(RuntimeError::Panic),
    };
    match catch_unwind(AssertUnwindSafe(|| f(&mut session))) {
        Ok(result) => result,
        Err(_) => {
            session.mark_faulted();
            Err(RuntimeError::Panic)
        }
    }
}

/// Validate and accept a project. Its startup runs in subsequent calls to `step`.
pub fn open_project(
    handle: RuntimeHandle,
    ini: &[u8],
    platform: &str,
    reported_os: Option<&str>,
) -> Result<u64, RuntimeError> {
    with_session(handle, |session| {
        if session.shutdown.is_closing_or_closed() {
            return Err(ProtocolError::State.into());
        }
        Ok(session.open_queued_project(ini, platform, reported_os)?)
    })
}

/// Begin a graceful close at a point with no outstanding scene services.
pub fn submit_shutdown(handle: RuntimeHandle, epoch: u64) -> Result<(), RuntimeError> {
    with_session(handle, |session| Ok(session.submit_shutdown(epoch)?))
}

/// Queue an ordered batch of input for the next newly accepted Host frame.
pub fn submit_input(
    handle: RuntimeHandle,
    epoch: u64,
    events: &[HostInput],
) -> Result<(), RuntimeError> {
    with_session(handle, |session| Ok(session.submit_input(epoch, events)?))
}

/// Queue playback feedback for the next newly accepted Host frame.
pub fn submit_playback_status(
    handle: RuntimeHandle,
    epoch: u64,
    playback_id: u64,
    status: PlaybackStatus,
) -> Result<(), RuntimeError> {
    with_session(handle, |session| {
        Ok(session.submit_playback_status(epoch, playback_id, status)?)
    })
}

/// Copy a decoded video-layer frame for the next newly accepted Host frame.
pub fn submit_video_frame(
    handle: RuntimeHandle,
    epoch: u64,
    frame: VideoFrame<'_>,
) -> Result<(), RuntimeError> {
    with_session(handle, |session| {
        Ok(session.submit_video_frame(epoch, frame)?)
    })
}

/// Supply the fallback font for this project before accepting its first frame.
pub fn set_default_font(
    handle: RuntimeHandle,
    epoch: u64,
    bytes: &[u8],
) -> Result<(), RuntimeError> {
    with_session(
        handle,
        |session| Ok(session.set_default_font(epoch, bytes)?),
    )
}

pub fn step(handle: RuntimeHandle, frame: HostFrame) -> Result<ScriptIoProgress, RuntimeError> {
    with_session(handle, |session| {
        let progress = session.step_queued_frame(frame)?;
        session.publish_frame(progress)?;
        Ok(progress)
    })
}

/// Borrow the latest CPU frame newer than `after`, if any. Dropping the view releases it.
pub fn acquire_latest(
    handle: RuntimeHandle,
    after: u64,
) -> Result<Option<FrameLease>, RuntimeError> {
    with_session(handle, |session| {
        Ok(session.frames.acquire(handle, after)?)
    })
}

pub fn poll_events(handle: RuntimeHandle, output: &mut [u8]) -> Result<PollResult, RuntimeError> {
    with_session(handle, |session| Ok(session.poll_events(output)))
}

pub fn submit_replies(handle: RuntimeHandle, replies: &[HostReply]) -> Result<(), RuntimeError> {
    with_session(handle, |session| {
        if session.is_faulted() {
            return Err(ProtocolError::State.into());
        }
        Ok(session.submit_replies(replies)?)
    })
}

pub fn get_state(handle: RuntimeHandle) -> Result<RuntimeState, RuntimeError> {
    with_session(handle, |session| Ok(session.runtime_state()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_reject_other_threads_types_and_stale_values() {
        let create = || create(0, TransportLimits::default(), 4096).unwrap();
        let first = create();
        let state = get_state(first).unwrap();
        assert_eq!(state.phase, RuntimePhase::Created);
        assert_eq!(state.transport.epoch, 0);
        assert_eq!(state.stage_size, None);
        assert_eq!(state.host_frame_id, None);

        thread::spawn(move || {
            assert!(matches!(get_state(first), Err(RuntimeError::Thread)));
            assert!(matches!(destroy(first), Err(RuntimeError::Thread)));
            let second = create();
            assert_ne!(first, second);
            assert_eq!(get_state(second).unwrap().phase, RuntimePhase::Created);
            destroy(second).unwrap();
        })
        .join()
        .unwrap();

        destroy(first).unwrap();
        let replacement = create();
        assert_ne!(first, replacement);
        assert!(matches!(destroy(first), Err(RuntimeError::Handle)));
        for raw in [0, 1, first.as_raw(), replacement.as_raw() ^ (1 << 48)] {
            assert!(matches!(
                get_state(RuntimeHandle::from_raw(raw)),
                Err(RuntimeError::Handle)
            ));
        }
        destroy(replacement).unwrap();
    }
}
