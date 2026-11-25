//! Siglus's independent, versioned host ABI. No VM or GPU pointers cross it.
//!
//! The upstream VM uses `Rc` internally. Handles are process-unique, but the
//! runtime table is thread-local: a handle called from another thread fails
//! closed instead of unsafely moving the VM or graphics backend.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use art3m1s_log::{Level, Record};
use art3m1s_render::{BlendMode, Extent2D, FrameTarget, GpuBackend, NativeSurface};
use art3m1s_siglus::siglus_scene_vm::{
    host::SiglusHostConfig,
    runtime::native_ui::{NativeMessageBoxKind, NativeMessageBoxRequest, NativeUiBackend},
};
use art3m1s_siglus::{
    DirectorySaves, SiglusAdapter, SiglusFrameCompiler, SiglusFrameSnapshot, SiglusProjectIoV1,
    SiglusRenderer, is_siglus_project,
};

pub const ABI_VERSION: u32 = 1;
pub const ABI_MAGIC: u32 = 0x5349_4731; // SIG1
pub const STATUS_OK: i32 = 0;
pub const STATUS_ARGUMENT: i32 = -1;
pub const STATUS_HANDLE: i32 = -2;
pub const STATUS_ENGINE: i32 = -3;
const DIAGNOSTICS_MAGIC: u64 = 0x3156_4753_4d33_4152; // "RA3MSGV1"
const AUDIO_MAGIC: u64 = 0x3156_4153_4d33_4152; // "RA3MSAV1"
const AUDIO_SAMPLE_RATE: u32 = 48_000;
const AUDIO_CHANNELS: u32 = 2;
const MAX_AUDIO_FRAMES: usize = 4_800;
const MAX_LOG_RECORDS: usize = 1024;
const MAX_LOG_MESSAGE_BYTES: usize = 16 * 1024;

#[repr(C)]
pub struct SiglusDiagnosticsApiV1 {
    pub struct_size: u32,
    pub abi_version: u32,
    pub magic: u64,
    pub log_next_bytes: unsafe extern "C" fn() -> usize,
    pub poll_log: unsafe extern "C" fn(*mut u8, usize) -> usize,
    pub runtime_set_debug: unsafe extern "C" fn(u64, i32) -> i32,
}

#[repr(C)]
pub struct SiglusAudioApiV1 {
    pub struct_size: u32,
    pub abi_version: u32,
    pub magic: u64,
    pub sample_rate: u32,
    pub channels: u32,
    /// Fill exactly `frames * channels` interleaved f32 samples.
    pub runtime_render_pcm: unsafe extern "C" fn(u64, *mut f32, usize) -> i32,
}

unsafe extern "C" fn runtime_render_pcm(handle: u64, out: *mut f32, frames: usize) -> i32 {
    guarded(|| {
        if out.is_null()
            || !(1..=MAX_AUDIO_FRAMES).contains(&frames)
            || (out as usize) % std::mem::align_of::<f32>() != 0
        {
            return STATUS_ARGUMENT;
        }
        with_runtime(handle, |runtime| {
            let output =
                unsafe { std::slice::from_raw_parts_mut(out, frames * AUDIO_CHANNELS as usize) };
            runtime.adapter.render_audio(output);
            STATUS_OK
        })
    })
}

struct LogRecord {
    level: u32,
    message: String,
}

static LOG_QUEUE: Mutex<VecDeque<LogRecord>> = Mutex::new(VecDeque::new());
static LOG_ACTIVE: AtomicBool = AtomicBool::new(false);
static LOG_DEBUG: AtomicBool = AtomicBool::new(false);
static LOG_PREVIOUS_LEVEL: Mutex<Option<Level>> = Mutex::new(None);

fn begin_log_session() -> bool {
    if LOG_ACTIVE
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }
    crate::host::logging::install_once();
    if let Ok(mut queue) = LOG_QUEUE.lock() {
        queue.clear();
    }
    if let Some(logger) = art3m1s_log::global() {
        if let Ok(mut previous) = LOG_PREVIOUS_LEVEL.lock() {
            *previous = Some(logger.level());
            logger.set_level(Level::Debug);
        }
    }
    LOG_DEBUG.store(false, Ordering::Relaxed);
    true
}

fn end_log_session() {
    LOG_ACTIVE.store(false, Ordering::Release);
    LOG_DEBUG.store(false, Ordering::Relaxed);
    if let (Some(logger), Ok(mut previous)) = (art3m1s_log::global(), LOG_PREVIOUS_LEVEL.lock()) {
        if let Some(level) = previous.take() {
            logger.set_level(level);
        }
    }
}

struct PendingLogSession(bool);

impl Drop for PendingLogSession {
    fn drop(&mut self) {
        if self.0 {
            end_log_session();
        }
    }
}

pub(crate) fn dispatch_log(record: &Record) {
    if !LOG_ACTIVE.load(Ordering::Acquire)
        || (record.level() >= Level::Debug && !LOG_DEBUG.load(Ordering::Relaxed))
    {
        return;
    }
    let level = match record.level() {
        Level::Error => 'E',
        Level::Warn => 'W',
        Level::Info => 'I',
        Level::Debug | Level::Trace => 'D',
    } as u32;
    let message = format!("[{}] {}", record.target(), record.message());
    let mut end = message.len().min(MAX_LOG_MESSAGE_BYTES);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    if let Ok(mut queue) = LOG_QUEUE.lock() {
        if queue.len() == MAX_LOG_RECORDS {
            queue.pop_front();
        }
        queue.push_back(LogRecord {
            level,
            message: message[..end].to_owned(),
        });
    }
}

unsafe extern "C" fn log_next_bytes() -> usize {
    catch_unwind(AssertUnwindSafe(|| {
        LOG_QUEUE
            .lock()
            .ok()
            .and_then(|queue| queue.front().map(|record| 8 + record.message.len()))
            .unwrap_or(0)
    }))
    .unwrap_or(0)
}

unsafe extern "C" fn poll_log(out: *mut u8, capacity: usize) -> usize {
    catch_unwind(AssertUnwindSafe(|| {
        if out.is_null() || !(8..=1024 * 1024).contains(&capacity) {
            return 0;
        }
        let Ok(mut queue) = LOG_QUEUE.lock() else {
            return 0;
        };
        let mut written = 0;
        while let Some(record) = queue.front() {
            let needed = 8 + record.message.len();
            if needed > capacity - written {
                break;
            }
            unsafe {
                std::ptr::copy_nonoverlapping(
                    record.level.to_le_bytes().as_ptr(),
                    out.add(written),
                    4,
                );
                std::ptr::copy_nonoverlapping(
                    (record.message.len() as u32).to_le_bytes().as_ptr(),
                    out.add(written + 4),
                    4,
                );
                std::ptr::copy_nonoverlapping(
                    record.message.as_ptr(),
                    out.add(written + 8),
                    record.message.len(),
                );
            }
            written += needed;
            queue.pop_front();
        }
        written
    }))
    .unwrap_or(0)
}

unsafe extern "C" fn runtime_set_debug(handle: u64, enabled: i32) -> i32 {
    guarded(|| {
        with_runtime(handle, |_| {
            LOG_DEBUG.store(enabled != 0, Ordering::Relaxed);
            STATUS_OK
        })
    })
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SiglusApiV1 {
    pub struct_size: u32,
    pub abi_version: u32,
    pub magic: u32,
    pub probe_project: unsafe extern "C" fn(*const c_char) -> i32,
    pub runtime_create: unsafe extern "C" fn(*const c_char, i32, *mut u64) -> i32,
    pub runtime_destroy: unsafe extern "C" fn(u64),
    pub runtime_stage_width: unsafe extern "C" fn(u64) -> u32,
    pub runtime_stage_height: unsafe extern "C" fn(u64) -> u32,
    /// kind 0 detaches; kind 1-4 match the common native surface ABI.
    pub runtime_set_external_surface: unsafe extern "C" fn(u64, i32, *mut c_void, u32, u32) -> i32,
    /// mode: 0 advance, 1 read RGBA, 2 present to attached native surface.
    /// Output pixels are RGBA8 at the logical stage size; written is optional.
    pub runtime_tick: unsafe extern "C" fn(u64, u32, i32, *mut u8, usize, *mut usize) -> i32,
    /// kind: 0 move, 1 mouse button, 2 touch, 3 key, 4 wheel.
    /// x/y are stage coordinates, code is button/key/touch phase, value is
    /// pressed (0/1) or wheel delta. Touch phase follows down/move/up/cancel.
    pub runtime_input: unsafe extern "C" fn(u64, i32, i32, i32, i32, i32) -> i32,
    pub runtime_is_exit_requested: unsafe extern "C" fn(u64) -> i32,
    /// Returns UTF-8 byte length (excluding NUL); copies at most capacity.
    pub last_error: unsafe extern "C" fn(*mut u8, usize) -> usize,
}

#[repr(C)]
pub struct SiglusApiV2 {
    pub base: SiglusApiV1,
    pub runtime_create_with_save_dir:
        unsafe extern "C" fn(*const c_char, *const c_char, i32, *mut u64) -> i32,
}

#[derive(Default)]
struct DialogSlot(Mutex<Option<PendingDialog>>);

struct PendingDialog {
    id: u64,
    choices: Vec<i64>,
    payload: Vec<u8>,
}

impl NativeUiBackend for DialogSlot {
    fn show_system_messagebox(&self, request: NativeMessageBoxRequest) {
        let kind = match request.kind {
            NativeMessageBoxKind::Ok => 0,
            NativeMessageBoxKind::OkCancel => 1,
            NativeMessageBoxKind::YesNo => 2,
            NativeMessageBoxKind::YesNoCancel => 3,
        };
        let choices: Vec<_> = request.buttons.iter().map(|button| button.value).collect();
        let buttons: Vec<_> = request
            .buttons
            .iter()
            .map(|button| serde_json::json!({"label": button.label, "value": button.value}))
            .collect();
        let payload = serde_json::json!({
            "id": request.request_id,
            "kind": kind,
            "title": request.title,
            "message": request.message,
            "buttons": buttons,
        });
        *self.0.lock().unwrap() = Some(PendingDialog {
            id: request.request_id,
            choices,
            payload: payload.to_string().into_bytes(),
        });
    }
}

struct Runtime {
    adapter: SiglusAdapter,
    presenter: Option<Presenter>,
    compiler: SiglusFrameCompiler,
    latest: Option<Rc<SiglusFrameSnapshot>>,
    explicit_presentation: bool,
    width: u32,
    height: u32,
    exiting: bool,
    save_fingerprint: Option<u64>,
    audio_output: Option<super::siglus_output::Output>,
    host_audio_clock: bool,
    dialog: Option<Arc<DialogSlot>>,
}

struct Presenter {
    renderer: SiglusRenderer,
    gpu: Box<dyn GpuBackend>,
    surface_attached: bool,
    presented: Option<Rc<SiglusFrameSnapshot>>,
}

impl Presenter {
    fn new(backend: i32, width: u32, height: u32) -> Result<Self, String> {
        let selection = crate::backend::BackendSelection::try_from_legacy_int(backend)?;
        Ok(Self {
            renderer: SiglusRenderer::default(),
            gpu: crate::backend::create_backend(selection, width, height)?,
            surface_attached: false,
            presented: None,
        })
    }
}

impl Drop for Presenter {
    fn drop(&mut self) {
        self.renderer.release_textures(self.gpu.as_mut());
        self.gpu.clear_native_surface();
    }
}

thread_local! {
    static FRAME_LEASES: RefCell<HashMap<u64, FrameLease>> = RefCell::new(HashMap::new());
    static RUNTIMES: RefCell<HashMap<u64, Runtime>> = RefCell::new(HashMap::new());
    static LAST_ERROR: RefCell<String> = RefCell::new(String::new());
}
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

fn fail(message: impl Into<String>) -> i32 {
    let message = message.into();
    LAST_ERROR.with(|error| *error.borrow_mut() = message);
    STATUS_ENGINE
}

fn guarded(f: impl FnOnce() -> i32) -> i32 {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(status) => status,
        Err(_) => fail("Siglus native API panicked"),
    }
}

fn with_runtime(handle: u64, f: impl FnOnce(&mut Runtime) -> i32) -> i32 {
    RUNTIMES.with(|table| match table.borrow_mut().get_mut(&handle) {
        Some(runtime) => f(runtime),
        None => STATUS_HANDLE,
    })
}

fn project_path(ptr: *const c_char) -> Result<PathBuf, i32> {
    if ptr.is_null() {
        return Err(STATUS_ARGUMENT);
    }
    let path = unsafe { CStr::from_ptr(ptr) };
    let path = path.to_str().map_err(|_| STATUS_ARGUMENT)?;
    if path.is_empty() {
        return Err(STATUS_ARGUMENT);
    }
    Ok(PathBuf::from(path))
}

unsafe extern "C" fn probe_project(ptr: *const c_char) -> i32 {
    guarded(|| match project_path(ptr) {
        Ok(path) => i32::from(is_siglus_project(&path)),
        Err(status) => status,
    })
}

unsafe extern "C" fn runtime_create(ptr: *const c_char, backend: i32, out: *mut u64) -> i32 {
    unsafe { runtime_create_with_save_dir(ptr, std::ptr::null(), backend, out) }
}

unsafe extern "C" fn runtime_create_with_save_dir(
    ptr: *const c_char,
    save_dir: *const c_char,
    backend: i32,
    out: *mut u64,
) -> i32 {
    create_runtime(ptr, save_dir, Some(backend), out)
}

unsafe extern "C" fn create_frame_runtime(
    ptr: *const c_char,
    save_dir: *const c_char,
    out: *mut u64,
) -> i32 {
    create_runtime(ptr, save_dir, None, out)
}

fn create_runtime(
    ptr: *const c_char,
    save_dir: *const c_char,
    backend: Option<i32>,
    out: *mut u64,
) -> i32 {
    guarded(|| {
        if out.is_null() {
            return STATUS_ARGUMENT;
        }
        unsafe { *out = 0 };
        let path = match project_path(ptr) {
            Ok(path) => path,
            Err(status) => return status,
        };
        if !is_siglus_project(&path) {
            return fail("directory has no Siglus Scene.pck and Gameexe.dat/ini");
        }
        let mut config = SiglusHostConfig::new(path);
        if !save_dir.is_null() {
            let path = match project_path(save_dir) {
                Ok(path) => path,
                Err(status) => return status,
            };
            let legacy = config.project_dir.join("savedata");
            match DirectorySaves::open(&path, &legacy) {
                Ok(store) => config.save_store = Some(Arc::new(store)),
                Err(error) => return fail(format!("Siglus save storage: {error:#}")),
            }
        }
        let uses_host_saves = config.save_store.is_some();
        let log_session = PendingLogSession(begin_log_session());
        let opened = if backend.is_none() {
            SiglusAdapter::open_directory_host(config)
        } else {
            SiglusAdapter::open(config)
        };
        let adapter = match opened {
            Ok(adapter) => adapter,
            Err(error) => return fail(format!("Siglus VM initialization: {error:#}")),
        };
        register_runtime(adapter, backend, uses_host_saves, log_session, out)
    })
}

unsafe extern "C" fn open_project_io(
    callbacks: *const SiglusProjectIoV1,
    append: *const c_char,
    out: *mut u64,
) -> i32 {
    guarded(|| {
        if out.is_null() {
            return STATUS_ARGUMENT;
        }
        unsafe { out.write(0) };
        if callbacks.is_null()
            || unsafe { callbacks.cast::<u32>().read() }
                < std::mem::size_of::<SiglusProjectIoV1>() as u32
        {
            return STATUS_ARGUMENT;
        }
        let callbacks = unsafe { callbacks.read() };
        if !callbacks.is_valid() {
            return STATUS_ARGUMENT;
        }
        let append = if append.is_null() {
            ""
        } else {
            let Ok(append) = unsafe { CStr::from_ptr(append) }.to_str() else {
                return STATUS_ARGUMENT;
            };
            append
        };
        let log_session = PendingLogSession(begin_log_session());
        let adapter = match unsafe { SiglusAdapter::open_callback_io(callbacks, append) } {
            Ok(adapter) => adapter,
            Err(error) => return fail(format!("Siglus Host IO initialization: {error:#}")),
        };
        register_runtime(adapter, None, true, log_session, out)
    })
}

fn register_runtime(
    mut adapter: SiglusAdapter,
    backend: Option<i32>,
    uses_host_saves: bool,
    mut log_session: PendingLogSession,
    out: *mut u64,
) -> i32 {
    let (width, height) = adapter.logical_size();
    let save_fingerprint =
        uses_host_saves.then(|| adapter.with_host(|host| host.persist_global_if_changed(None)));
    if width == 0
        || height == 0
        || (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .is_none()
    {
        return fail("Siglus stage dimensions are invalid");
    }
    let presenter = match backend
        .map(|id| Presenter::new(id, width, height))
        .transpose()
    {
        Ok(presenter) => presenter,
        Err(error) => return fail(format!("Siglus GPU initialization: {error}")),
    };
    let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    if handle == 0 || handle == u64::MAX {
        return fail("Siglus runtime handles exhausted");
    }
    RUNTIMES.with(|table| {
        table.borrow_mut().insert(
            handle,
            Runtime {
                adapter,
                presenter,
                compiler: SiglusFrameCompiler::default(),
                latest: None,
                explicit_presentation: backend.is_none(),
                width,
                height,
                exiting: false,
                save_fingerprint,
                audio_output: None,
                host_audio_clock: false,
                dialog: None,
            },
        );
    });
    unsafe { *out = handle };
    log_session.0 = false;
    STATUS_OK
}

unsafe extern "C" fn runtime_destroy(handle: u64) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        RUNTIMES.with(|table| {
            let mut table = table.borrow_mut();
            if let Some(mut runtime) = table.remove(&handle) {
                if let Some(initial) = runtime.save_fingerprint {
                    runtime
                        .adapter
                        .with_host(|host| host.persist_global_if_changed(Some(initial)));
                }
                drop(runtime);
                if table.is_empty() {
                    end_log_session();
                }
            }
        });
    }));
}

unsafe extern "C" fn runtime_stage_width(handle: u64) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        RUNTIMES.with(|table| table.borrow().get(&handle).map_or(0, |rt| rt.width))
    }))
    .unwrap_or(0)
}

unsafe extern "C" fn runtime_stage_height(handle: u64) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        RUNTIMES.with(|table| table.borrow().get(&handle).map_or(0, |rt| rt.height))
    }))
    .unwrap_or(0)
}

unsafe extern "C" fn runtime_set_external_surface(
    handle: u64,
    kind: i32,
    ptr: *mut c_void,
    width: u32,
    height: u32,
) -> i32 {
    guarded(|| {
        with_runtime(handle, |rt| {
            let Some(presenter) = rt.presenter.as_mut() else {
                return STATUS_ARGUMENT;
            };
            if kind == 0 {
                presenter.gpu.clear_native_surface();
                presenter.surface_attached = false;
                presenter.presented = None;
                return STATUS_OK;
            }
            let surface = match NativeSurface::from_legacy_parts(kind, ptr, width, height) {
                Ok(surface) => surface,
                Err(error) => return fail(error),
            };
            match presenter.gpu.set_native_surface(surface) {
                Ok(()) => {
                    presenter.surface_attached = true;
                    presenter.presented = None;
                    STATUS_OK
                }
                Err(error) => fail(error),
            }
        })
    })
}

unsafe extern "C" fn runtime_tick(
    handle: u64,
    dt_ms: u32,
    mode: i32,
    out: *mut u8,
    capacity: usize,
    written: *mut usize,
) -> i32 {
    guarded(|| {
        with_runtime(handle, |rt| {
            if !matches!(mode, 0..=2) || (rt.explicit_presentation && mode != 0) {
                return STATUS_ARGUMENT;
            }
            if mode == 1 && (out.is_null() || capacity < rt.width as usize * rt.height as usize * 4)
            {
                return STATUS_ARGUMENT;
            }
            if mode == 2 && !rt.presenter.as_ref().is_some_and(|p| p.surface_attached) {
                return STATUS_ARGUMENT;
            }
            let step = match rt.adapter.step(dt_ms) {
                Ok(step) => step,
                Err(error) => return fail(format!("Siglus step: {error:#}")),
            };
            if let Some(frame) = step.frame {
                let snapshot = match rt.compiler.compile(&frame) {
                    Ok(snapshot) => snapshot,
                    Err(error) => return fail(format!("Siglus frame: {error:#}")),
                };
                if !rt.explicit_presentation {
                    let presenter = rt.presenter.as_mut().expect("legacy presenter");
                    if let Err(error) = presenter.renderer.draw_snapshot(
                        &snapshot,
                        presenter.gpu.as_mut(),
                        mode == 2,
                    ) {
                        return fail(format!("Siglus frame: {error:#}"));
                    }
                }
                rt.latest = Some(Rc::new(snapshot));
            }
            rt.exiting = step.exiting;
            if !rt.host_audio_clock {
                let requested = dt_ms.min(100) as usize * AUDIO_SAMPLE_RATE as usize / 1000;
                let status = mix_output(rt, requested);
                if status != STATUS_OK {
                    return status;
                }
            }
            if mode == 1 {
                let gpu = &mut rt.presenter.as_mut().expect("legacy presenter").gpu;
                return copy_readback(gpu.as_mut(), rt.width, rt.height, out, capacity, written);
            } else if !written.is_null() {
                unsafe { *written = 0 };
            }
            STATUS_OK
        })
    })
}

fn copy_readback(
    gpu: &mut dyn GpuBackend,
    width: u32,
    height: u32,
    out: *mut u8,
    capacity: usize,
    written: *mut usize,
) -> i32 {
    gpu.begin_access();
    let pixels = gpu.readback_owned(FrameTarget::Main, Extent2D::new(width, height));
    gpu.end_access();
    let pixels = match pixels {
        Ok(pixels) => pixels,
        Err(error) => return fail(error),
    };
    if pixels.len() > capacity {
        return STATUS_ARGUMENT;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), out, pixels.len());
        if !written.is_null() {
            written.write(pixels.len());
        }
    }
    STATUS_OK
}

unsafe extern "C" fn runtime_input(
    handle: u64,
    kind: i32,
    code: i32,
    x: i32,
    y: i32,
    value: i32,
) -> i32 {
    guarded(|| {
        with_runtime(handle, |rt| {
            match kind {
                0 => rt.adapter.pointer_move(x, y),
                1 => {
                    rt.adapter.pointer_move(x, y);
                    rt.adapter.pointer_button(code, value != 0);
                }
                2 => rt.adapter.touch(code, x, y),
                3 => rt.adapter.key(code, value != 0),
                4 => rt.adapter.wheel(value),
                _ => return STATUS_ARGUMENT,
            }
            STATUS_OK
        })
    })
}

unsafe extern "C" fn runtime_is_exit_requested(handle: u64) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        RUNTIMES.with(|table| {
            table
                .borrow()
                .get(&handle)
                .map_or(STATUS_HANDLE, |rt| i32::from(rt.exiting))
        })
    }))
    .unwrap_or(STATUS_ENGINE)
}

unsafe extern "C" fn last_error(out: *mut u8, capacity: usize) -> usize {
    catch_unwind(AssertUnwindSafe(|| {
        LAST_ERROR.with(|error| {
            let error = error.borrow();
            let bytes = error.as_bytes();
            if !out.is_null() && capacity > 0 {
                unsafe {
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len().min(capacity));
                }
            }
            bytes.len()
        })
    }))
    .unwrap_or(0)
}

const API_BASE: SiglusApiV1 = SiglusApiV1 {
    struct_size: std::mem::size_of::<SiglusApiV1>() as u32,
    abi_version: ABI_VERSION,
    magic: ABI_MAGIC,
    probe_project,
    runtime_create,
    runtime_destroy,
    runtime_stage_width,
    runtime_stage_height,
    runtime_set_external_surface,
    runtime_tick,
    runtime_input,
    runtime_is_exit_requested,
    last_error,
};

static API: SiglusApiV1 = API_BASE;

static API_V2: SiglusApiV2 = SiglusApiV2 {
    base: SiglusApiV1 {
        struct_size: std::mem::size_of::<SiglusApiV2>() as u32,
        abi_version: 2,
        ..API_BASE
    },
    runtime_create_with_save_dir,
};

static DIAGNOSTICS_API: SiglusDiagnosticsApiV1 = SiglusDiagnosticsApiV1 {
    struct_size: std::mem::size_of::<SiglusDiagnosticsApiV1>() as u32,
    abi_version: 1,
    magic: DIAGNOSTICS_MAGIC,
    log_next_bytes,
    poll_log,
    runtime_set_debug,
};

static AUDIO_API: SiglusAudioApiV1 = SiglusAudioApiV1 {
    struct_size: std::mem::size_of::<SiglusAudioApiV1>() as u32,
    abi_version: 1,
    magic: AUDIO_MAGIC,
    sample_rate: AUDIO_SAMPLE_RATE,
    channels: AUDIO_CHANNELS,
    runtime_render_pcm,
};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_siglus_get_api_v1(out_size: *mut usize) -> *const SiglusApiV1 {
    if !out_size.is_null() {
        unsafe { *out_size = std::mem::size_of::<SiglusApiV1>() };
    }
    &API
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_siglus_get_api_v2(out_size: *mut usize) -> *const SiglusApiV2 {
    if !out_size.is_null() {
        unsafe { *out_size = std::mem::size_of::<SiglusApiV2>() };
    }
    &API_V2
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_siglus_get_diagnostics_api_v1(
    out_size: *mut usize,
) -> *const SiglusDiagnosticsApiV1 {
    if !out_size.is_null() {
        unsafe { *out_size = std::mem::size_of::<SiglusDiagnosticsApiV1>() };
    }
    &DIAGNOSTICS_API
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_siglus_get_audio_api_v1(
    out_size: *mut usize,
) -> *const SiglusAudioApiV1 {
    if !out_size.is_null() {
        unsafe { *out_size = std::mem::size_of::<SiglusAudioApiV1>() };
    }
    &AUDIO_API
}

#[repr(C)]
pub struct SiglusOutputApiV1 {
    pub struct_size: u32,
    pub abi_version: u32,
    pub magic: u64,
    pub runtime_start: unsafe extern "C" fn(u64) -> i32,
    pub runtime_stop: unsafe extern "C" fn(u64) -> i32,
    pub runtime_suspend: unsafe extern "C" fn(u64, i32) -> i32,
    pub runtime_volume: unsafe extern "C" fn(u64, f32) -> i32,
    pub initialize_android: unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32,
}

unsafe extern "C" fn output_start(handle: u64) -> i32 {
    start_output(handle, false)
}

unsafe extern "C" fn output_start_clocked(handle: u64) -> i32 {
    start_output(handle, true)
}

fn start_output(handle: u64, host_clock: bool) -> i32 {
    guarded(|| {
        with_runtime(handle, |rt| {
            if rt.audio_output.is_none() {
                match super::siglus_output::Output::start() {
                    Ok(output) => rt.audio_output = Some(output),
                    Err(error) => return fail(error),
                }
            }
            rt.host_audio_clock = host_clock;
            STATUS_OK
        })
    })
}

fn mix_output(rt: &mut Runtime, requested: usize) -> i32 {
    let Some(output) = &rt.audio_output else {
        return STATUS_OK;
    };
    if output.failed() {
        return fail("Siglus audio device disconnected");
    }
    let frames = requested.min(output.capacity());
    if frames > 0 {
        let mut pcm = vec![0.0; frames * AUDIO_CHANNELS as usize];
        rt.adapter.render_audio(&mut pcm);
        output.append(&pcm);
    }
    STATUS_OK
}

unsafe extern "C" fn output_pump(handle: u64, frames: u32) -> i32 {
    guarded(|| {
        if frames as usize > MAX_AUDIO_FRAMES {
            return STATUS_ARGUMENT;
        }
        with_runtime(handle, |rt| {
            if !rt.host_audio_clock || rt.audio_output.is_none() {
                return STATUS_ARGUMENT;
            }
            mix_output(rt, frames as usize)
        })
    })
}

unsafe extern "C" fn output_stop(handle: u64) -> i32 {
    guarded(|| {
        with_runtime(handle, |rt| {
            rt.audio_output = None;
            STATUS_OK
        })
    })
}
unsafe extern "C" fn output_suspend(handle: u64, suspended: i32) -> i32 {
    guarded(|| {
        with_runtime(handle, |rt| {
            if let Some(output) = &rt.audio_output {
                output.suspend(suspended != 0);
            }
            STATUS_OK
        })
    })
}
unsafe extern "C" fn output_volume(handle: u64, volume: f32) -> i32 {
    guarded(|| {
        if !volume.is_finite() {
            return STATUS_ARGUMENT;
        }
        with_runtime(handle, |rt| {
            if let Some(output) = &rt.audio_output {
                output.volume(volume);
            }
            STATUS_OK
        })
    })
}
unsafe extern "C" fn output_initialize_android(vm: *mut c_void, context: *mut c_void) -> i32 {
    guarded(|| {
        if vm.is_null() || context.is_null() {
            return STATUS_ARGUMENT;
        }
        #[cfg(target_os = "android")]
        {
            static INITIALIZED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
            INITIALIZED.get_or_init(|| unsafe {
                ndk_context::initialize_android_context(vm, context);
            });
            STATUS_OK
        }
        #[cfg(not(target_os = "android"))]
        {
            STATUS_ARGUMENT
        }
    })
}
static OUTPUT_API: SiglusOutputApiV1 = SiglusOutputApiV1 {
    struct_size: std::mem::size_of::<SiglusOutputApiV1>() as u32,
    abi_version: 1,
    magic: 0x3156_4f53_4d33_4152,
    runtime_start: output_start,
    runtime_stop: output_stop,
    runtime_suspend: output_suspend,
    runtime_volume: output_volume,
    initialize_android: output_initialize_android,
};

/// Output owns a bounded PCM queue and never calls the VM from the device thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_siglus_get_output_api_v1(
    out_size: *mut usize,
) -> *const SiglusOutputApiV1 {
    if !out_size.is_null() {
        unsafe {
            *out_size = std::mem::size_of::<SiglusOutputApiV1>();
        }
    }
    &OUTPUT_API
}

#[repr(C)]
pub struct SiglusHostApiV1 {
    pub struct_size: u32,
    pub abi_version: u32,
    pub magic: u64,
    pub runtime: &'static SiglusApiV2,
    pub audio: &'static SiglusAudioApiV1,
    pub diagnostics: &'static SiglusDiagnosticsApiV1,
    pub output: &'static SiglusOutputApiV1,
}

static HOST_API: SiglusHostApiV1 = SiglusHostApiV1 {
    struct_size: std::mem::size_of::<SiglusHostApiV1>() as u32,
    abi_version: 1,
    magic: u64::from_le_bytes(*b"RA3MSHV1"),
    runtime: &API_V2,
    audio: &AUDIO_API,
    diagnostics: &DIAGNOSTICS_API,
    output: &OUTPUT_API,
};

/// # Safety
/// `out_size` must be null or writable for a `usize`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_siglus_get_host_api_v1(
    out_size: *mut usize,
) -> *const SiglusHostApiV1 {
    if !out_size.is_null() {
        unsafe { out_size.write(std::mem::size_of::<SiglusHostApiV1>()) };
    }
    &HOST_API
}

unsafe extern "C" fn enable_dialogs(handle: u64) -> i32 {
    guarded(|| {
        with_runtime(handle, |rt| {
            if rt.dialog.is_none() {
                let slot = Arc::new(DialogSlot::default());
                rt.adapter.with_host(|host| {
                    host.vm_mut().ctx.set_native_ui_backend(Some(slot.clone()));
                });
                rt.dialog = Some(slot);
            }
            STATUS_OK
        })
    })
}

unsafe extern "C" fn read_dialog(
    handle: u64,
    out: *mut u8,
    capacity: usize,
    required: *mut usize,
) -> i32 {
    guarded(|| {
        if required.is_null() {
            return STATUS_ARGUMENT;
        }
        with_runtime(handle, |rt| {
            let slot = rt.dialog.as_ref().map(|slot| slot.0.lock().unwrap());
            let dialog = slot.as_deref().and_then(Option::as_ref);
            let len = dialog.map_or(0, |dialog| dialog.payload.len());
            unsafe { required.write(len) };
            if let Some(dialog) = dialog
                && !out.is_null()
                && capacity >= len
            {
                unsafe { std::ptr::copy_nonoverlapping(dialog.payload.as_ptr(), out, len) };
            }
            STATUS_OK
        })
    })
}

unsafe extern "C" fn submit_dialog(handle: u64, id: u64, value: i64) -> i32 {
    guarded(|| {
        with_runtime(handle, |rt| {
            let Some(slot) = &rt.dialog else {
                return STATUS_ARGUMENT;
            };
            {
                let mut pending = slot.0.lock().unwrap();
                let Some(dialog) = pending.as_ref() else {
                    return STATUS_ARGUMENT;
                };
                if dialog.id != id || !dialog.choices.contains(&value) {
                    return STATUS_ARGUMENT;
                }
                *pending = None;
            }
            rt.adapter.with_host(|host| {
                // Releases delivered to the Host dialog do not reach the VM.
                host.vm_mut().ctx.input.clear_all();
                host.vm_mut().ctx.script_input.clear_all();
                host.submit_native_messagebox_result(id, value);
            });
            STATUS_OK
        })
    })
}

unsafe extern "C" fn send_text(handle: u64, text: *const u8, len: usize) -> i32 {
    guarded(|| {
        if text.is_null() || len == 0 || len > isize::MAX as usize {
            return STATUS_ARGUMENT;
        }
        let Ok(text) = std::str::from_utf8(unsafe { std::slice::from_raw_parts(text, len) }) else {
            return STATUS_ARGUMENT;
        };
        with_runtime(handle, |rt| {
            rt.adapter.with_host(|host| {
                if host.vm_mut().ctx.globals.system.messagebox_modal.is_none()
                    && host.editbox_accepts_direct_text()
                {
                    host.text_input(text);
                    return 1;
                }
                STATUS_OK
            })
        })
    })
}

const NO_FRAME: i32 = 1;
const MAX_FRAME_LEASES: usize = 16;

#[repr(C)]
struct FrameMesh {
    texture_id: u64,
    blend: u32,
    clipped: u32,
    opacity: f32,
    clip: [f32; 4],
    vertices: [[f32; 4]; 6],
}

#[repr(C)]
pub struct FrameView {
    width: u32,
    height: u32,
    mesh_count: u32,
    texture_count: u32,
    meshes: *const FrameMesh,
}

#[repr(C)]
pub struct TextureView {
    id: u64,
    width: u32,
    height: u32,
    rgba: *const u8,
    rgba_len: usize,
}

struct FrameLease {
    frame: Rc<SiglusFrameSnapshot>,
    meshes: Vec<FrameMesh>,
}

unsafe extern "C" fn acquire_frame(session: u64, out: *mut u64) -> i32 {
    guarded(|| {
        if out.is_null() {
            return STATUS_ARGUMENT;
        }
        unsafe { out.write(0) };
        with_runtime(session, |rt| {
            let Some(frame) = &rt.latest else {
                return NO_FRAME;
            };
            FRAME_LEASES.with(|leases| {
                let mut leases = leases.borrow_mut();
                if leases.len() >= MAX_FRAME_LEASES {
                    return fail("Siglus frame lease limit reached");
                }
                let id = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
                let mesh = |command: &art3m1s_render::DrawCommand| FrameMesh {
                    texture_id: command.texture.0,
                    blend: match command.blend {
                        BlendMode::Alpha => 0,
                        BlendMode::Add => 1,
                        BlendMode::Multiply => 2,
                        BlendMode::Screen => 3,
                        _ => unreachable!("unsupported Siglus blend"),
                    },
                    clipped: u32::from(command.clip_bounds.is_some()),
                    opacity: command.opacity,
                    clip: command.clip_bounds.unwrap_or_default(),
                    vertices: command
                        .mesh
                        .as_ref()
                        .expect("Siglus mesh")
                        .vertices
                        .as_ref()
                        .try_into()
                        .expect("Siglus quad"),
                };
                leases.insert(
                    id,
                    FrameLease {
                        frame: frame.clone(),
                        meshes: frame.draw_list().commands.iter().map(mesh).collect(),
                    },
                );
                unsafe { out.write(id) };
                STATUS_OK
            })
        })
    })
}

unsafe extern "C" fn frame_view(lease: u64, out: *mut FrameView) -> i32 {
    guarded(|| {
        if out.is_null() {
            return STATUS_ARGUMENT;
        }
        FRAME_LEASES.with(|leases| {
            let leases = leases.borrow();
            let Some(lease) = leases.get(&lease) else {
                return STATUS_HANDLE;
            };
            let (width, height) = lease.frame.size();
            unsafe {
                out.write(FrameView {
                    width,
                    height,
                    mesh_count: lease.meshes.len() as u32,
                    texture_count: lease.frame.textures().len() as u32,
                    meshes: if lease.meshes.is_empty() {
                        std::ptr::null()
                    } else {
                        lease.meshes.as_ptr()
                    },
                })
            };
            STATUS_OK
        })
    })
}

unsafe extern "C" fn frame_texture(lease: u64, index: u32, out: *mut TextureView) -> i32 {
    guarded(|| {
        if out.is_null() {
            return STATUS_ARGUMENT;
        }
        FRAME_LEASES.with(|leases| {
            let leases = leases.borrow();
            let Some(lease) = leases.get(&lease) else {
                return STATUS_HANDLE;
            };
            let Some(texture) = lease.frame.textures().get(index as usize) else {
                return STATUS_ARGUMENT;
            };
            unsafe {
                out.write(TextureView {
                    id: texture.id.0,
                    width: texture.image.width,
                    height: texture.image.height,
                    rgba: texture.image.rgba.as_ptr(),
                    rgba_len: texture.image.rgba.len(),
                })
            };
            STATUS_OK
        })
    })
}

unsafe extern "C" fn release_frame(lease: u64) -> i32 {
    guarded(|| {
        FRAME_LEASES.with(|leases| match leases.borrow_mut().remove(&lease) {
            Some(_) => STATUS_OK,
            None => STATUS_HANDLE,
        })
    })
}

unsafe extern "C" fn configure_presenter(session: u64, backend: i32) -> i32 {
    guarded(|| {
        with_runtime(session, |rt| {
            if !rt.explicit_presentation {
                return STATUS_ARGUMENT;
            }
            match Presenter::new(backend, rt.width, rt.height) {
                Ok(presenter) => {
                    rt.presenter = Some(presenter);
                    STATUS_OK
                }
                Err(error) => fail(format!("Siglus GPU initialization: {error}")),
            }
        })
    })
}

unsafe extern "C" fn present_latest(session: u64) -> i32 {
    guarded(|| {
        with_runtime(session, |rt| {
            if !rt.explicit_presentation {
                return STATUS_ARGUMENT;
            }
            let Some(presenter) = rt.presenter.as_mut().filter(|p| p.surface_attached) else {
                return STATUS_ARGUMENT;
            };
            let Some(frame) = &rt.latest else {
                return NO_FRAME;
            };
            if presenter
                .presented
                .as_ref()
                .is_some_and(|previous| Rc::ptr_eq(previous, frame))
            {
                return NO_FRAME;
            }
            match presenter
                .renderer
                .draw_snapshot(frame, presenter.gpu.as_mut(), true)
            {
                Ok(()) => {
                    presenter.presented = Some(frame.clone());
                    STATUS_OK
                }
                Err(error) => fail(format!("Siglus frame: {error:#}")),
            }
        })
    })
}

unsafe extern "C" fn readback_latest(
    session: u64,
    out: *mut u8,
    capacity: usize,
    written: *mut usize,
) -> i32 {
    guarded(|| {
        with_runtime(session, |rt| {
            if !rt.explicit_presentation
                || out.is_null()
                || capacity < rt.width as usize * rt.height as usize * 4
            {
                return STATUS_ARGUMENT;
            }
            let Some(presenter) = rt.presenter.as_mut() else {
                return STATUS_ARGUMENT;
            };
            let Some(frame) = &rt.latest else {
                return NO_FRAME;
            };
            if let Err(error) =
                presenter
                    .renderer
                    .draw_snapshot(frame, presenter.gpu.as_mut(), false)
            {
                return fail(format!("Siglus frame: {error:#}"));
            }
            copy_readback(
                presenter.gpu.as_mut(),
                rt.width,
                rt.height,
                out,
                capacity,
                written,
            )
        })
    })
}

#[repr(C)]
pub struct SiglusFrameApiV1 {
    pub struct_size: u32,
    pub abi_version: u32,
    pub create_runtime: unsafe extern "C" fn(*const c_char, *const c_char, *mut u64) -> i32,
    pub acquire: unsafe extern "C" fn(u64, *mut u64) -> i32,
    pub view: unsafe extern "C" fn(u64, *mut FrameView) -> i32,
    pub texture: unsafe extern "C" fn(u64, u32, *mut TextureView) -> i32,
    pub release: unsafe extern "C" fn(u64) -> i32,
    pub configure_presenter: unsafe extern "C" fn(u64, i32) -> i32,
    pub present_latest: unsafe extern "C" fn(u64) -> i32,
    pub readback_latest: unsafe extern "C" fn(u64, *mut u8, usize, *mut usize) -> i32,
}

static FRAME_API: SiglusFrameApiV1 = SiglusFrameApiV1 {
    struct_size: std::mem::size_of::<SiglusFrameApiV1>() as u32,
    abi_version: 1,
    create_runtime: create_frame_runtime,
    acquire: acquire_frame,
    view: frame_view,
    texture: frame_texture,
    release: release_frame,
    configure_presenter,
    present_latest,
    readback_latest,
};

#[repr(C)]
pub struct SiglusOutputClockApiV1 {
    pub struct_size: u32,
    pub abi_version: u32,
    pub start: unsafe extern "C" fn(u64) -> i32,
    pub pump: unsafe extern "C" fn(u64, u32) -> i32,
}

static OUTPUT_CLOCK_API: SiglusOutputClockApiV1 = SiglusOutputClockApiV1 {
    struct_size: std::mem::size_of::<SiglusOutputClockApiV1>() as u32,
    abi_version: 1,
    start: output_start_clocked,
    pump: output_pump,
};

#[repr(C)]
pub struct SiglusHostApiV2 {
    pub struct_size: u32,
    pub abi_version: u32,
    pub magic: u64,
    pub base: &'static SiglusHostApiV1,
    pub enable_dialogs: unsafe extern "C" fn(u64) -> i32,
    pub read_dialog: unsafe extern "C" fn(u64, *mut u8, usize, *mut usize) -> i32,
    pub submit_dialog: unsafe extern "C" fn(u64, u64, i64) -> i32,
    pub send_text: unsafe extern "C" fn(u64, *const u8, usize) -> i32,
    pub frames: &'static SiglusFrameApiV1,
    pub output_clock: &'static SiglusOutputClockApiV1,
    pub open_project_io:
        unsafe extern "C" fn(*const SiglusProjectIoV1, *const c_char, *mut u64) -> i32,
}

static HOST_API_V2: SiglusHostApiV2 = SiglusHostApiV2 {
    struct_size: std::mem::size_of::<SiglusHostApiV2>() as u32,
    abi_version: 2,
    magic: u64::from_le_bytes(*b"RA3MSHV2"),
    base: &HOST_API,
    enable_dialogs,
    read_dialog,
    submit_dialog,
    send_text,
    frames: &FRAME_API,
    output_clock: &OUTPUT_CLOCK_API,
    open_project_io,
};

/// # Safety
/// `out_size` must be null or writable for a `usize`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_siglus_get_host_api_v2(
    out_size: *mut usize,
) -> *const SiglusHostApiV2 {
    if !out_size.is_null() {
        unsafe { out_size.write(std::mem::size_of::<SiglusHostApiV2>()) };
    }
    &HOST_API_V2
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    const GAMEEXE: &str = "#SCREEN_SIZE = 320, 240\n#MENU_SCENE = \"_start\", 0\n";

    fn scene_pack() -> Vec<u8> {
        const PACK_HEADER: i32 = 23 * 4;
        const SCENE_HEADER: i32 = 33 * 4;
        let name: Vec<u16> = "_start".encode_utf16().collect();
        let name_list = PACK_HEADER + 8;
        let data_index = name_list + name.len() as i32 * 2;
        let data_list = data_index + 8;
        let mut pack = [0i32; 23];
        pack[0] = PACK_HEADER;
        pack[13..21].copy_from_slice(&[PACK_HEADER, 1, name_list, 1, data_index, 1, data_list, 1]);
        let mut scene = [0i32; 33];
        scene[..3].copy_from_slice(&[SCENE_HEADER, SCENE_HEADER + 4, 1]);
        scene[9..11].copy_from_slice(&[SCENE_HEADER, 1]);
        let mut bytes: Vec<_> = pack.into_iter().flat_map(i32::to_le_bytes).collect();
        bytes.extend(
            [0, name.len() as i32]
                .into_iter()
                .flat_map(i32::to_le_bytes),
        );
        bytes.extend(name.into_iter().flat_map(u16::to_le_bytes));
        bytes.extend([0, SCENE_HEADER + 5].into_iter().flat_map(i32::to_le_bytes));
        bytes.extend(scene.into_iter().flat_map(i32::to_le_bytes));
        bytes.extend(0i32.to_le_bytes());
        bytes.push(art3m1s_siglus::siglus_scene_vm::runtime::constants::cd::EOF);
        bytes
    }

    #[test]
    fn callback_io_boots_and_reopens_with_host_saves() {
        use art3m1s_siglus::siglus_scene_vm::runtime::forms::codes::ELM_GLOBAL_G;

        struct Project {
            data: Vec<u8>,
            saves: Mutex<HashMap<String, Vec<u8>>>,
            reads: AtomicU64,
        }
        impl Project {
            fn file(&self, space: u32, key: &str) -> Option<Vec<u8>> {
                if space == 1 {
                    return self.saves.lock().unwrap().get(key).cloned();
                }
                match key.to_ascii_lowercase().as_str() {
                    "chapter/gameexe.ini" => Some(GAMEEXE.as_bytes().to_vec()),
                    "chapter/scene.pck" => Some(self.data.clone()),
                    _ => None,
                }
            }
        }
        unsafe extern "C" fn retain(user: *mut c_void) {
            unsafe { Arc::increment_strong_count(user.cast::<Project>()) };
        }
        unsafe extern "C" fn release(user: *mut c_void) {
            unsafe { Arc::decrement_strong_count(user.cast::<Project>()) };
        }
        unsafe extern "C" fn query(
            user: *mut c_void,
            space: u32,
            key: *const c_char,
            out: *mut u64,
        ) -> i32 {
            let project = unsafe { &*user.cast::<Project>() };
            let key = unsafe { CStr::from_ptr(key) }.to_str().unwrap();
            let Some(bytes) = project.file(space, key) else {
                return 1;
            };
            unsafe { out.write(bytes.len() as u64) };
            0
        }
        unsafe extern "C" fn read(
            user: *mut c_void,
            space: u32,
            key: *const c_char,
            offset: u64,
            output: *mut u8,
            capacity: usize,
            out: *mut usize,
        ) -> i32 {
            let project = unsafe { &*user.cast::<Project>() };
            let key = unsafe { CStr::from_ptr(key) }.to_str().unwrap();
            let Some(bytes) = project.file(space, key) else {
                return 1;
            };
            let bytes = &bytes[offset as usize..];
            let count = bytes.len().min(capacity).min(13);
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), output, count);
                out.write(count);
            }
            project.reads.fetch_add(1, Ordering::Relaxed);
            0
        }
        unsafe extern "C" fn write(
            user: *mut c_void,
            key: *const c_char,
            bytes: *const u8,
            len: usize,
        ) -> i32 {
            let project = unsafe { &*user.cast::<Project>() };
            let key = unsafe { CStr::from_ptr(key) }.to_str().unwrap();
            let bytes = unsafe { std::slice::from_raw_parts(bytes, len) };
            project
                .saves
                .lock()
                .unwrap()
                .insert(key.to_owned(), bytes.to_vec());
            0
        }
        unsafe extern "C" fn remove(user: *mut c_void, key: *const c_char) -> i32 {
            let project = unsafe { &*user.cast::<Project>() };
            let key = unsafe { CStr::from_ptr(key) }.to_str().unwrap();
            project.saves.lock().unwrap().remove(key);
            0
        }

        let project = Arc::new(Project {
            data: scene_pack(),
            saves: Mutex::new(HashMap::new()),
            reads: AtomicU64::new(0),
        });
        let callbacks = SiglusProjectIoV1 {
            struct_size: std::mem::size_of::<SiglusProjectIoV1>() as u32,
            abi_version: 1,
            user_data: Arc::as_ptr(&project).cast_mut().cast(),
            retain: Some(retain),
            release: Some(release),
            query_size: Some(query),
            read_range: Some(read),
            write_save: Some(write),
            remove_save: Some(remove),
        };
        let open = || {
            let mut handle = 0;
            let status = unsafe {
                (HOST_API_V2.open_project_io)(&callbacks, c"chapter".as_ptr(), &mut handle)
            };
            assert_eq!(
                status,
                STATUS_OK,
                "{}",
                LAST_ERROR.with(|e| e.borrow().clone())
            );
            assert_eq!(Arc::strong_count(&project), 2);
            handle
        };
        let first = open();
        assert_eq!(unsafe { runtime_stage_width(first) }, 320);
        assert!(project.reads.load(Ordering::Relaxed) > 2);
        with_runtime(first, |rt| {
            rt.adapter.with_host(|host| {
                host.vm_mut()
                    .ctx
                    .globals
                    .int_lists
                    .insert(ELM_GLOBAL_G as u32, vec![531]);
            });
            STATUS_OK
        });
        unsafe { runtime_destroy(first) };
        assert_eq!(Arc::strong_count(&project), 1);
        assert!(!project.saves.lock().unwrap().is_empty());
        let second = open();
        with_runtime(second, |rt| {
            rt.adapter.with_host(|host| {
                assert_eq!(
                    host.vm_mut().ctx.globals.int_lists[&(ELM_GLOBAL_G as u32)][0],
                    531
                );
            });
            STATUS_OK
        });
        unsafe { runtime_destroy(second) };
        assert_eq!(Arc::strong_count(&project), 1);
    }

    #[test]
    fn host_ui_resume_and_text() {
        use art3m1s_siglus::siglus_scene_vm::runtime::globals::{
            EditBoxListState, SystemMessageBoxButton,
        };

        struct Project(PathBuf);
        impl Drop for Project {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let project = Project(std::env::temp_dir().join(format!(
            "art3m1s-siglus-ui-{}-{}",
            std::process::id(),
            NEXT_HANDLE.fetch_add(1, Ordering::Relaxed),
        )));
        let chapter = project.0.join("Chapter");
        std::fs::create_dir_all(chapter.join("Fonts")).unwrap();
        std::fs::write(project.0.join("Select.ini"), "chapter\tChapter\n").unwrap();
        std::fs::write(chapter.join("gameexe.INI"), GAMEEXE).unwrap();
        std::fs::write(chapter.join("scene.PCK"), scene_pack()).unwrap();

        let path = CString::new(project.0.to_str().unwrap()).unwrap();
        let ui = unsafe { &*art3m1s_siglus_get_host_api_v2(std::ptr::null_mut()) };
        let api = &ui.base.runtime.base;
        let open = || {
            let mut handle = 0;
            assert_eq!(
                unsafe { (ui.frames.create_runtime)(path.as_ptr(), std::ptr::null(), &mut handle) },
                STATUS_OK,
                "{}",
                LAST_ERROR.with(|e| e.borrow().clone())
            );
            assert_eq!(unsafe { (ui.enable_dialogs)(handle) }, STATUS_OK);
            handle
        };
        let request = |handle| {
            with_runtime(handle, |rt| {
                rt.adapter.with_host(|host| {
                    let ctx = &mut host.vm_mut().ctx;
                    ctx.request_system_messagebox(
                        20,
                        false,
                        "続けますか？".into(),
                        ["YES", "NO", "CANCEL"]
                            .into_iter()
                            .enumerate()
                            .map(|(value, label)| SystemMessageBoxButton {
                                label: label.into(),
                                value: value as i64,
                            })
                            .collect(),
                    );
                    let mut list = EditBoxListState::new(1);
                    list.boxes[0].created = true;
                    list.boxes[0].visible = true;
                    ctx.globals.editbox_lists.insert(0, list);
                    ctx.globals.focused_editbox = Some((0, 0));
                });
                STATUS_OK
            });
            let mut required = 0;
            assert_eq!(
                unsafe { (ui.read_dialog)(handle, std::ptr::null_mut(), 0, &mut required) },
                STATUS_OK
            );
            assert!(required > 0);
            let mut payload = vec![0; required];
            assert_eq!(
                unsafe {
                    (ui.read_dialog)(handle, payload.as_mut_ptr(), payload.len(), &mut required)
                },
                STATUS_OK
            );
            let dialog: serde_json::Value = serde_json::from_slice(&payload).unwrap();
            assert_eq!(dialog["message"], "続けますか？");
            assert_eq!(dialog["buttons"][2]["value"], 2);
            dialog["id"].as_u64().unwrap()
        };
        let handle = open();
        with_runtime(handle, |rt| {
            rt.adapter.with_host(|_| {
                let folder = art3m1s_siglus::siglus_scene_vm::resource::resolve_game_path(
                    &project.0.join("chapter/fonts"),
                )
                .unwrap()
                .unwrap();
                assert!(folder.is_dir());
            });
            STATUS_OK
        });
        let id = request(handle);
        let text = "雪あかり".as_bytes();
        assert_eq!(
            unsafe { (ui.send_text)(handle, text.as_ptr(), text.len()) },
            0
        );
        assert_eq!(
            unsafe { (ui.submit_dialog)(handle, id + 1, 2) },
            STATUS_ARGUMENT
        );
        assert_eq!(
            unsafe { (ui.submit_dialog)(handle, id, 3) },
            STATUS_ARGUMENT
        );
        assert_eq!(
            unsafe {
                (api.runtime_tick)(handle, 16, 0, std::ptr::null_mut(), 0, std::ptr::null_mut())
            },
            STATUS_OK
        );
        assert_eq!(unsafe { (api.runtime_is_exit_requested)(handle) }, 0);
        assert_eq!(unsafe { (ui.submit_dialog)(handle, id, 2) }, STATUS_OK);
        assert_eq!(
            unsafe { (ui.send_text)(handle, text.as_ptr(), text.len()) },
            1
        );
        with_runtime(handle, |rt| {
            rt.adapter.with_host(|host| {
                let ctx = &host.vm_mut().ctx;
                assert_eq!(ctx.globals.system.messagebox_modal_result, Some(2));
                assert_eq!(ctx.globals.editbox_lists[&0].boxes[0].text, "雪あかり");
            });
            STATUS_OK
        });
        for _ in 0..5 {
            assert_eq!(
                unsafe {
                    (api.runtime_tick)(handle, 16, 0, std::ptr::null_mut(), 0, std::ptr::null_mut())
                },
                STATUS_OK,
                "{}",
                LAST_ERROR.with(|e| e.borrow().clone())
            );
        }
        assert_eq!(unsafe { (api.runtime_is_exit_requested)(handle) }, 1);
        let mut lease = 0;
        assert_eq!(
            unsafe { (ui.frames.acquire)(handle, &mut lease) },
            STATUS_OK
        );
        assert_ne!(lease, 0);
        #[cfg(all(target_os = "macos", feature = "metal-backend"))]
        {
            let len = 320 * 240 * 4;
            let read_recreated_presenter = || {
                assert_eq!(
                    unsafe { (ui.frames.configure_presenter)(handle, 3) },
                    STATUS_OK
                );
                let mut rgba = vec![0; len];
                let mut written = 0;
                assert_eq!(
                    unsafe {
                        (ui.frames.readback_latest)(handle, rgba.as_mut_ptr(), len, &mut written)
                    },
                    STATUS_OK
                );
                assert_eq!(written, len);
                rgba
            };
            assert_eq!(read_recreated_presenter(), read_recreated_presenter());
        }
        unsafe { (api.runtime_destroy)(handle) };
        let view = std::thread::spawn(move || {
            let mut view = std::mem::MaybeUninit::<FrameView>::uninit();
            unsafe { (FRAME_API.view)(lease, view.as_mut_ptr()) }
        })
        .join()
        .unwrap();
        assert_eq!(view, STATUS_HANDLE);
        let mut view = std::mem::MaybeUninit::<FrameView>::uninit();
        assert_eq!(
            unsafe { (ui.frames.view)(lease, view.as_mut_ptr()) },
            STATUS_OK
        );
        let view = unsafe { view.assume_init() };
        assert_eq!((view.width, view.height), (320, 240));
        assert_eq!(unsafe { (ui.frames.release)(lease) }, STATUS_OK);
        let replacement = open();
        assert_eq!(request(replacement), id);
        assert_eq!(unsafe { (ui.submit_dialog)(handle, id, 2) }, STATUS_HANDLE);
        assert_eq!(unsafe { (ui.submit_dialog)(replacement, id, 1) }, STATUS_OK);
        unsafe { (api.runtime_destroy)(replacement) };
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn optional_real_game_host_abi_smoke() {
        let Some(path) = std::env::var_os("ART3M1S_SIGLUS_TEST_GAME") else {
            return;
        };
        let path = CString::new(path.to_string_lossy().as_bytes()).unwrap();
        let mut size = 0usize;
        let api = unsafe { &*art3m1s_siglus_get_api_v1(&mut size) };
        assert_eq!(size, std::mem::size_of::<SiglusApiV1>());
        assert_eq!(unsafe { (api.probe_project)(path.as_ptr()) }, 1);
        let mut handle = 0;
        let status = unsafe { (api.runtime_create)(path.as_ptr(), 0, &mut handle) };
        let mut error = vec![0; 1024];
        let length = unsafe { (api.last_error)(error.as_mut_ptr(), error.len()) };
        assert_eq!(
            status,
            STATUS_OK,
            "{}",
            String::from_utf8_lossy(&error[..length.min(error.len())])
        );
        assert_ne!(handle, 0);
        let width = unsafe { (api.runtime_stage_width)(handle) };
        let height = unsafe { (api.runtime_stage_height)(handle) };
        let mut pixels = vec![0; width as usize * height as usize * 4];
        for _ in 0..150 {
            let status = unsafe {
                (api.runtime_tick)(
                    handle,
                    16,
                    1,
                    pixels.as_mut_ptr(),
                    pixels.len(),
                    std::ptr::null_mut(),
                )
            };
            assert_eq!(status, STATUS_OK);
            if pixels.chunks_exact(4).any(|pixel| pixel[..3] != [0, 0, 0]) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        assert!(pixels.chunks_exact(4).any(|pixel| pixel[..3] != [0, 0, 0]));
        assert_eq!(
            unsafe { (api.runtime_input)(handle, 0, 0, 100, 100, 0) },
            STATUS_OK
        );
        unsafe { (api.runtime_destroy)(handle) };
        assert_eq!(
            unsafe { (api.runtime_is_exit_requested)(handle) },
            STATUS_HANDLE
        );
    }
}

#[cfg(test)]
mod output_contract_tests {
    use super::*;

    #[test]
    fn public_audio_tables_preserve_host_layout_and_reject_invalid_handles() {
        assert_eq!(
            std::mem::size_of::<SiglusApiV1>(),
            16 + 10 * std::mem::size_of::<usize>()
        );
        let mut size = 0;
        let host1 = unsafe { &*art3m1s_siglus_get_host_api_v1(&mut size) };
        assert_eq!(size, 16 + 4 * std::mem::size_of::<usize>());
        let host2 = unsafe { &*art3m1s_siglus_get_host_api_v2(&mut size) };
        assert_eq!(size, 16 + 8 * std::mem::size_of::<usize>());
        assert!(std::ptr::eq(host1, host2.base));
        let output = unsafe { &*art3m1s_siglus_get_output_api_v1(&mut size) };
        assert_eq!(size, 16 + 5 * std::mem::size_of::<usize>());
        assert_eq!(output.struct_size as usize, size);
        assert_eq!(output.abi_version, 1);
        assert_eq!(output.magic, 0x3156_4f53_4d33_4152);
        assert_eq!(unsafe { (output.runtime_start)(u64::MAX) }, STATUS_HANDLE);
        assert_eq!(unsafe { (output.runtime_stop)(u64::MAX) }, STATUS_HANDLE);
        assert_eq!(
            unsafe { (output.runtime_suspend)(u64::MAX, 1) },
            STATUS_HANDLE
        );
        assert_eq!(
            unsafe { (output.runtime_volume)(u64::MAX, 0.5) },
            STATUS_HANDLE
        );
        assert_eq!(
            unsafe { (output.runtime_volume)(u64::MAX, f32::NAN) },
            STATUS_ARGUMENT
        );
        assert_eq!(
            unsafe { (output.initialize_android)(std::ptr::null_mut(), std::ptr::null_mut()) },
            STATUS_ARGUMENT
        );
    }
}
