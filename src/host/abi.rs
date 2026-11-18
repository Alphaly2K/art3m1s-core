//! Versioned C tables for the queued Artemis runtime.

use super::frame::{FRAME_PACKET_VERSION, FrameLease};
use super::protocol::{PollStatus, ProtocolError, TransportLimits};
use super::runtime::{self, RuntimeError, RuntimeHandle, RuntimePhase, RuntimeState};
use super::script_io::{HostFrame, ScriptIoError, ScriptIoProgress};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::size_of;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};

mod submit;

const OK: i32 = 0;
const NO_DATA: i32 = 1;
const WOULD_BLOCK: i32 = 2;
const ARGUMENT: i32 = -1;
const HANDLE: i32 = -2;
const THREAD: i32 = -3;
const UNSUPPORTED: i32 = -4;
const BUFFER: i32 = -5;
const STATE: i32 = -6;
const STALE: i32 = -7;
const LIMIT: i32 = -8;
const PANIC: i32 = -9;
const MAGIC: u64 = 0x415254334D324150;
const MAJOR: u32 = 2;
const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;

type AbiResult = Result<i32, Failure>;

#[derive(Debug)]
struct Failure(i32, Cow<'static, str>);

impl Failure {
    fn new(status: i32, message: &'static str) -> Self {
        Self(status, Cow::Borrowed(message))
    }
}

impl From<RuntimeError> for Failure {
    fn from(error: RuntimeError) -> Self {
        let status = match &error {
            RuntimeError::Handle => HANDLE,
            RuntimeError::Thread => THREAD,
            RuntimeError::Panic => PANIC,
            RuntimeError::Session(ScriptIoError::Protocol(error)) => match error {
                ProtocolError::Argument(_) => ARGUMENT,
                ProtocolError::State => STATE,
                ProtocolError::Stale => STALE,
                ProtocolError::Limit => LIMIT,
                ProtocolError::WouldBlock => WOULD_BLOCK,
            },
            RuntimeError::Session(ScriptIoError::Project(_)) => ARGUMENT,
            RuntimeError::Session(ScriptIoError::Script(_)) => STATE,
        };
        Self(status, Cow::Owned(error.to_string()))
    }
}

thread_local! {
    static LAST_ERROR: RefCell<Cow<'static, str>> = const { RefCell::new(Cow::Borrowed("")) };
    static FRAMES: RefCell<HashMap<u64, FrameLease>> = RefCell::new(HashMap::new());
}
static FRAME_TOKEN: AtomicU64 = AtomicU64::new(1);

fn boundary(f: impl FnOnce() -> AbiResult) -> i32 {
    let result = catch_unwind(AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(Failure::new(PANIC, "Artemis C interface panicked")));
    match result {
        Ok(status) => status,
        Err(Failure(status, text)) => {
            if status < 0 {
                LAST_ERROR.with(|last| *last.borrow_mut() = text);
            }
            status
        }
    }
}

fn check_ptr<T>(p: *const T) -> Result<(), Failure> {
    if p.is_null() || !p.is_aligned() {
        return Err(Failure::new(ARGUMENT, "null or misaligned pointer"));
    }
    Ok(())
}

unsafe fn struct_size<T>(p: *const T) -> Result<u32, Failure> {
    check_ptr(p)?;
    let size = unsafe { p.cast::<u32>().read() };
    if (size as usize) < size_of::<T>() {
        return Err(Failure::new(
            ARGUMENT,
            "structure is smaller than the required prefix",
        ));
    }
    Ok(size)
}

unsafe fn clear_output<T: Default>(p: *mut T) -> Result<u32, Failure> {
    let size = unsafe { struct_size(p)? };
    unsafe { set_output(p, size, T::default()) };
    Ok(size)
}

unsafe fn set_output<T>(p: *mut T, size: u32, value: T) {
    unsafe {
        p.write(value);
        p.cast::<u32>().write(size);
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Bytes {
    data: *const u8,
    size: u64,
}

impl Bytes {
    fn borrowed(bytes: &[u8]) -> Self {
        Self {
            data: bytes.as_ptr(),
            size: bytes.len() as u64,
        }
    }

    unsafe fn slice<'a>(self, max: usize) -> Result<&'a [u8], Failure> {
        let n = checked_len(self.size, max)?;
        if n == 0 {
            return Ok(&[]);
        }
        check_ptr(self.data)?;
        Ok(unsafe { std::slice::from_raw_parts(self.data, n) })
    }

    unsafe fn utf8<'a>(self, max: usize) -> Result<&'a str, Failure> {
        let bytes = unsafe { self.slice(max)? };
        let text =
            std::str::from_utf8(bytes).map_err(|_| Failure::new(ARGUMENT, "invalid UTF-8"))?;
        if text.contains('\0') {
            return Err(Failure::new(ARGUMENT, "NUL in UTF-8 field"));
        }
        Ok(text)
    }
}

fn checked_len(bytes: u64, max: usize) -> Result<usize, Failure> {
    usize::try_from(bytes)
        .ok()
        .filter(|n| *n <= max.min(isize::MAX as usize))
        .ok_or(Failure::new(LIMIT, "byte count exceeds interface limit"))
}

unsafe fn output_bytes<'a>(output: *mut u8, size: u64) -> Result<&'a mut [u8], Failure> {
    let n = checked_len(size, isize::MAX as usize)?;
    if n == 0 {
        return Ok(&mut []);
    }
    check_ptr(output)?;
    Ok(unsafe { std::slice::from_raw_parts_mut(output, n) })
}

#[repr(C)]
pub struct ApiHeader {
    magic: u64,
    struct_size: u32,
    interface_id: u32,
    abi_major: u32,
    abi_minor: u32,
}

const fn header<T>(interface_id: u32) -> ApiHeader {
    ApiHeader {
        magic: MAGIC,
        struct_size: size_of::<T>() as u32,
        interface_id,
        abi_major: MAJOR,
        abi_minor: 0,
    }
}

#[repr(C)]
pub struct RootApi {
    header: ApiHeader,
    query_interface: unsafe extern "C" fn(u32, u32, u32, *mut *const c_void) -> i32,
    copy_last_error: unsafe extern "C" fn(*mut u8, u64, *mut u64) -> i32,
}

#[repr(C)]
struct RuntimeApi {
    header: ApiHeader,
    create: unsafe extern "C" fn(*const CreateInfo, *mut u64) -> i32,
    destroy: extern "C" fn(u64) -> i32,
    open_project: unsafe extern "C" fn(u64, *const ProjectInfo) -> i32,
    step: unsafe extern "C" fn(u64, *const StepInfo, *mut StepResult) -> i32,
    submit: unsafe extern "C" fn(u64, *const submit::Message, u32) -> i32,
    poll_events: unsafe extern "C" fn(u64, *mut u8, u64, *mut PollResult) -> i32,
    get_state: unsafe extern "C" fn(u64, *mut State) -> i32,
}

#[repr(C)]
struct FrameApi {
    header: ApiHeader,
    acquire_latest: unsafe extern "C" fn(u64, u64, *mut FrameView) -> i32,
    release: extern "C" fn(u64, u64) -> i32,
    texture_rgba: unsafe extern "C" fn(u64, u64, u32, *mut Bytes) -> i32,
}

#[repr(C)]
struct CreateInfo {
    struct_size: u32,
    flags: u32,
    host_service_caps: u64,
    renderer_caps: u64,
    random_seed: u64,
}

#[repr(C)]
struct ProjectInfo {
    struct_size: u32,
    flags: u32,
    ini_bytes: Bytes,
    platform_utf8: Bytes,
    reported_os_utf8: Bytes,
}

#[repr(C)]
struct StepInfo {
    struct_size: u32,
    flags: u32,
    host_frame_id: u64,
    monotonic_ns: u64,
    utc_unix_ms: i64,
    utc_offset_minutes: i32,
    reserved: u32,
}

#[repr(C)]
#[derive(Default)]
struct StepResult {
    struct_size: u32,
    flags: u32,
    host_frame_id: u64,
    latest_render_frame_id: u64,
    pending_requests: u64,
}

#[repr(C)]
#[derive(Default)]
struct State {
    struct_size: u32,
    state: u32,
    session_epoch: u64,
    host_frame_id: u64,
    stage_width: u32,
    stage_height: u32,
    wait_flags: u32,
    exit_requested: u32,
    pending_requests: u64,
    control_queue_bytes: u64,
    dropped_diagnostic_records: u64,
}

#[repr(C)]
#[derive(Default)]
struct PollResult {
    struct_size: u32,
    record_count: u32,
    written_bytes: u64,
    required_next_bytes: u64,
}

#[repr(C)]
#[derive(Default)]
struct FrameView {
    struct_size: u32,
    schema_version: u32,
    session_epoch: u64,
    render_frame_id: u64,
    token: u64,
    packet: Bytes,
}

static ROOT: RootApi = RootApi {
    header: header::<RootApi>(0),
    query_interface,
    copy_last_error,
};
static RUNTIME: RuntimeApi = RuntimeApi {
    header: header::<RuntimeApi>(1),
    create,
    destroy,
    open_project,
    step,
    submit: submit::submit,
    poll_events,
    get_state,
};
static FRAME: FrameApi = FrameApi {
    header: header::<FrameApi>(2),
    acquire_latest,
    release,
    texture_rgba,
};

/// Query the root table. The returned static table is valid for the library lifetime.
///
/// # Safety
/// `out` must point to writable, aligned storage for one C pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_get_api_v2(
    major: u32,
    minimum_size: u32,
    out: *mut *const RootApi,
) -> i32 {
    boundary(|| {
        check_ptr(out)?;
        unsafe { out.write(ptr::null()) };
        check_api(&ROOT.header, major, minimum_size)?;
        unsafe { out.write(&ROOT) };
        Ok(OK)
    })
}

fn check_api(header: &ApiHeader, major: u32, minimum_size: u32) -> Result<(), Failure> {
    if major != header.abi_major || minimum_size > header.struct_size {
        return Err(Failure::new(
            UNSUPPORTED,
            "requested ABI version or size is unavailable",
        ));
    }
    Ok(())
}

unsafe extern "C" fn query_interface(
    id: u32,
    major: u32,
    minimum_size: u32,
    out: *mut *const c_void,
) -> i32 {
    boundary(|| {
        check_ptr(out)?;
        unsafe { out.write(ptr::null()) };
        let (head, table) = match id {
            1 => (&RUNTIME.header, ptr::from_ref(&RUNTIME).cast()),
            2 => (&FRAME.header, ptr::from_ref(&FRAME).cast()),
            _ => return Err(Failure::new(UNSUPPORTED, "unknown interface ID")),
        };
        check_api(head, major, minimum_size)?;
        unsafe { out.write(table) };
        Ok(OK)
    })
}

unsafe extern "C" fn copy_last_error(output: *mut u8, capacity: u64, required: *mut u64) -> i32 {
    let result = (|| -> AbiResult {
        check_ptr(required)?;
        unsafe { required.write(0) };
        let output = unsafe { output_bytes(output, capacity)? };
        LAST_ERROR.with(|last| {
            let last = last.borrow();
            let bytes = last.as_bytes();
            unsafe { required.write(bytes.len() as u64) };
            if output.len() < bytes.len() {
                return Ok(BUFFER);
            }
            output[..bytes.len()].copy_from_slice(bytes);
            Ok(OK)
        })
    })();
    result.unwrap_or_else(|Failure(status, _)| status)
}

unsafe extern "C" fn create(info: *const CreateInfo, out: *mut u64) -> i32 {
    boundary(|| {
        check_ptr(out)?;
        unsafe {
            out.write(0);
            struct_size(info)?;
        }
        let info = unsafe { &*info };
        if info.flags != 0
            || info.host_service_caps & !runtime::HOST_CAPS != 0
            || info.renderer_caps & !runtime::RENDERER_CAPS != 0
        {
            return Err(Failure::new(
                UNSUPPORTED,
                "unknown create flag or capability",
            ));
        }
        let handle = runtime::create_with_caps(
            info.random_seed,
            TransportLimits::default(),
            MAX_FILE_BYTES,
            info.host_service_caps,
            info.renderer_caps,
        )?;
        unsafe { out.write(handle.as_raw()) };
        Ok(OK)
    })
}

extern "C" fn destroy(rt: u64) -> i32 {
    boundary(|| {
        runtime::destroy(RuntimeHandle::from_raw(rt))?;
        Ok(OK)
    })
}

unsafe extern "C" fn open_project(rt: u64, info: *const ProjectInfo) -> i32 {
    boundary(|| {
        unsafe { struct_size(info)? };
        let info = unsafe { &*info };
        if info.flags != 0 {
            return Err(Failure::new(UNSUPPORTED, "unknown project flag"));
        }
        let ini = unsafe { info.ini_bytes.slice(MAX_FILE_BYTES)? };
        let platform = unsafe { info.platform_utf8.utf8(4096)? };
        let os = unsafe { info.reported_os_utf8.utf8(4096)? };
        runtime::open_project(
            RuntimeHandle::from_raw(rt),
            ini,
            platform,
            (!os.is_empty()).then_some(os),
        )?;
        Ok(OK)
    })
}

unsafe extern "C" fn step(rt: u64, info: *const StepInfo, out: *mut StepResult) -> i32 {
    boundary(|| {
        let size = unsafe { clear_output(out)? };
        unsafe { struct_size(info)? };
        let info = unsafe { &*info };
        if info.flags != 0 || info.reserved != 0 || info.host_frame_id == 0 {
            return Err(Failure::new(ARGUMENT, "invalid Host frame descriptor"));
        }
        let handle = RuntimeHandle::from_raw(rt);
        let progress = runtime::step(
            handle,
            HostFrame {
                id: info.host_frame_id,
                monotonic_ns: info.monotonic_ns,
                utc_unix_ms: info.utc_unix_ms,
                utc_offset_minutes: info.utc_offset_minutes,
            },
        )?;
        let state = runtime::get_state(handle)?;
        unsafe {
            set_output(
                out,
                size,
                StepResult {
                    struct_size: 0,
                    flags: wait_flags(state) | (u32::from(state.scenario_finished) << 3),
                    host_frame_id: state.host_frame_id.unwrap_or(0),
                    latest_render_frame_id: state.latest_render_frame_id.unwrap_or(0),
                    pending_requests: state.transport.pending_requests as u64,
                },
            )
        };
        Ok(if progress == ScriptIoProgress::WouldBlock {
            WOULD_BLOCK
        } else {
            OK
        })
    })
}

unsafe extern "C" fn get_state(rt: u64, out: *mut State) -> i32 {
    boundary(|| {
        let size = unsafe { clear_output(out)? };
        let state = runtime::get_state(RuntimeHandle::from_raw(rt))?;
        let [stage_width, stage_height] = state.stage_size.unwrap_or([0, 0]);
        let phase = match state.phase {
            RuntimePhase::Created => 0,
            RuntimePhase::Loading => 1,
            RuntimePhase::Running => 2,
            RuntimePhase::Closing => 3,
            RuntimePhase::Closed => 4,
            RuntimePhase::Faulted => 5,
        };
        unsafe {
            set_output(
                out,
                size,
                State {
                    struct_size: 0,
                    state: phase,
                    session_epoch: state.transport.epoch,
                    host_frame_id: state.host_frame_id.unwrap_or(0),
                    stage_width,
                    stage_height,
                    wait_flags: wait_flags(state),
                    exit_requested: u32::from(state.exit_requested),
                    pending_requests: state.transport.pending_requests as u64,
                    control_queue_bytes: state.transport.control.bytes as u64,
                    dropped_diagnostic_records: state.transport.dropped_diagnostics,
                },
            )
        };
        Ok(OK)
    })
}

fn wait_flags(state: RuntimeState) -> u32 {
    u32::from(state.waiting_for_io)
        | (u32::from(state.waiting_for_ui) << 1)
        | (u32::from(state.waiting_for_scenario) << 2)
        | (u32::from(state.waiting_for_render) << 4)
}

unsafe extern "C" fn poll_events(
    rt: u64,
    output: *mut u8,
    capacity: u64,
    out: *mut PollResult,
) -> i32 {
    boundary(|| {
        let size = unsafe { clear_output(out)? };
        let output = unsafe { output_bytes(output, capacity)? };
        let result = runtime::poll_events(RuntimeHandle::from_raw(rt), output)?;
        unsafe {
            set_output(
                out,
                size,
                PollResult {
                    struct_size: 0,
                    record_count: result.record_count,
                    written_bytes: result.written_bytes as u64,
                    required_next_bytes: result.required_next_bytes as u64,
                },
            )
        };
        match result.status {
            PollStatus::Ok => Ok(OK),
            PollStatus::NoData => Ok(NO_DATA),
            PollStatus::Buffer => Err(Failure::new(BUFFER, "event buffer is too small")),
        }
    })
}

unsafe extern "C" fn acquire_latest(rt: u64, after: u64, out: *mut FrameView) -> i32 {
    boundary(|| {
        let size = unsafe { clear_output(out)? };
        let Some(lease) = runtime::acquire_latest(RuntimeHandle::from_raw(rt), after)? else {
            return Ok(NO_DATA);
        };
        let token = FRAME_TOKEN
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| Failure::new(LIMIT, "frame tokens exhausted"))?;
        let view = FrameView {
            struct_size: 0,
            schema_version: u32::from(FRAME_PACKET_VERSION),
            session_epoch: lease.session_epoch,
            render_frame_id: lease.render_frame_id,
            token,
            packet: Bytes::borrowed(lease.packet()),
        };
        FRAMES.with(|frames| frames.borrow_mut().insert(token, lease));
        unsafe { set_output(out, size, view) };
        Ok(OK)
    })
}

extern "C" fn release(rt: u64, token: u64) -> i32 {
    boundary(|| {
        let rt = RuntimeHandle::from_raw(rt);
        runtime::get_state(rt)?;
        FRAMES.with(|frames| {
            let mut frames = frames.borrow_mut();
            if !frames
                .get(&token)
                .is_some_and(|lease| lease.runtime() == rt)
            {
                return Err(Failure::new(HANDLE, "invalid frame token"));
            }
            frames.remove(&token);
            Ok(OK)
        })
    })
}

unsafe extern "C" fn texture_rgba(rt: u64, token: u64, index: u32, out: *mut Bytes) -> i32 {
    boundary(|| {
        check_ptr(out)?;
        unsafe { out.write(Bytes::default()) };
        let rt = RuntimeHandle::from_raw(rt);
        runtime::get_state(rt)?;
        FRAMES.with(|frames| {
            let frames = frames.borrow();
            let lease = frames
                .get(&token)
                .filter(|lease| lease.runtime() == rt)
                .ok_or(Failure::new(HANDLE, "invalid frame token"))?;
            let texture = lease
                .texture(index as usize)
                .ok_or(Failure::new(ARGUMENT, "texture index out of range"))?;
            unsafe { out.write(Bytes::borrowed(texture.rgba())) };
            Ok(OK)
        })
    })
}
