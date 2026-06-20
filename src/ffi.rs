//! C FFI bridge — all host ↔ core communication.
//!
//! The Flutter frontend registers callbacks at startup; afterwards every
//! filesystem operation inside the core is routed through those callbacks,
//! keeping the core entirely free of direct I/O.

use std::ffi::{c_char, c_int, c_longlong, CString};
use std::sync::OnceLock;

// ── Global debug flag ──────────────────────────────────────────

static DEBUG: OnceLock<bool> = OnceLock::new();

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_set_debug(enabled: c_int) {
    let _ = DEBUG.set(enabled != 0);
}

pub fn debug_enabled() -> bool {
    DEBUG.get().copied().unwrap_or(false)
}

// ── Log callback ───────────────────────────────────────────────

type LogCallback = unsafe extern "C" fn(level: *const c_char, msg: *const c_char);

static LOG_CB: OnceLock<LogCallback> = OnceLock::new();

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_register_log_callback(cb: LogCallback) {
    let _ = LOG_CB.set(cb);
}

pub fn log(level: &str, msg: &str) {
    if let Some(cb) = LOG_CB.get() {
        if let (Ok(l), Ok(m)) = (CString::new(level), CString::new(msg)) {
            unsafe { cb(l.as_ptr(), m.as_ptr()); }
        }
    }
}

#[macro_export]
macro_rules! core_info {
    ($($arg:tt)*) => { $crate::ffi::log("I", &format!($($arg)*)); };
}
#[macro_export]
macro_rules! core_warn {
    ($($arg:tt)*) => { $crate::ffi::log("W", &format!($($arg)*)); };
}
#[macro_export]
macro_rules! core_debug {
    ($($arg:tt)*) => {
        if $crate::ffi::debug_enabled() {
            $crate::ffi::log("D", &format!($($arg)*));
        }
    };
}
#[macro_export]
macro_rules! core_error {
    ($($arg:tt)*) => { $crate::ffi::log("E", &format!($($arg)*)); };
}

// ── ANGLE library search path ──────────────────────────────────

static ANGLE_PATH: OnceLock<String> = OnceLock::new();

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_set_angle_path(path: *const c_char) {
    if let Ok(s) = unsafe { std::ffi::CStr::from_ptr(path).to_str() } {
        let _ = ANGLE_PATH.set(s.to_string());
    }
}

pub fn angle_lib_path(name: &str) -> String {
    if let Some(prefix) = ANGLE_PATH.get() {
        format!("{prefix}/{name}")
    } else {
        name.to_string()
    }
}

// ── File reader callback ────────────────────────────────────────

type FileReaderCallback = unsafe extern "C" fn(
    path: *const c_char,
    buf: *mut u8,
    buf_size: c_int,
    offset: c_longlong,
) -> c_int;

static FILE_READER: OnceLock<FileReaderCallback> = OnceLock::new();

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_register_file_reader(cb: FileReaderCallback) {
    let _ = FILE_READER.set(cb);
}

pub fn file_reader_registered() -> bool {
    FILE_READER.get().is_some()
}

// ── Save directory ───────────────────────────────────────────────

static SAVE_DIR: OnceLock<String> = OnceLock::new();

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_set_save_dir(dir: *const c_char) {
    if let Ok(s) = unsafe { std::ffi::CStr::from_ptr(dir).to_str() } {
        let _ = SAVE_DIR.set(s.to_string());
    }
}

pub fn save_dir() -> Option<&'static str> {
    SAVE_DIR.get().map(|s| s.as_str())
}

// ── Query helpers ────────────────────────────────────────────────

fn query_size(path: &str) -> Option<u64> {
    let cb = FILE_READER.get()?;
    let c_path = CString::new(path).ok()?;
    let size = unsafe { cb(c_path.as_ptr(), std::ptr::null_mut(), 0, -1) };
    if size < 0 { None } else { Some(size as u64) }
}

fn read_chunk(path: &str, offset: u64, buf: &mut [u8]) -> Option<usize> {
    let cb = FILE_READER.get()?;
    let c_path = CString::new(path).ok()?;
    let n = unsafe {
        cb(
            c_path.as_ptr(),
            buf.as_mut_ptr(),
            buf.len() as c_int,
            offset as c_longlong,
        )
    };
    if n < 0 { None } else { Some(n as usize) }
}

const CHUNK: usize = 65536;
const MAX_SINGLE: u64 = 16 * 1024 * 1024;

pub fn request_file(path: &str) -> Result<Vec<u8>, String> {
    let total = query_size(path).ok_or_else(|| format!("not found: {path}"))?;
    if total == 0 { return Ok(Vec::new()); }
    if total <= MAX_SINGLE {
        let mut buf = vec![0u8; total as usize];
        let n = read_chunk(path, 0, &mut buf).unwrap_or(0);
        buf.truncate(n);
        return Ok(buf);
    }
    let mut buf = Vec::with_capacity(total as usize);
    let mut off = 0u64;
    while off < total {
        let take = ((total - off) as usize).min(CHUNK);
        let mut chunk = vec![0u8; take];
        let n = read_chunk(path, off, &mut chunk).unwrap_or(0);
        if n == 0 { break; }
        buf.extend_from_slice(&chunk[..n]);
        off += n as u64;
    }
    Ok(buf)
}

pub fn request_file_range(path: &str, offset: u64, len: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; len];
    let n = read_chunk(path, offset, &mut buf)?;
    buf.truncate(n);
    Some(buf)
}

pub fn request_asset(path: &str) -> Option<Vec<u8>> { request_file(path).ok() }
pub fn request_asset_range(path: &str, offset: u64, len: usize) -> Option<Vec<u8>> {
    request_file_range(path, offset, len)
}
pub fn query_asset_size(path: &str) -> Option<u64> { query_size(path) }

// ── File operations ──────────────────────────────────────────────

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_file_exists(path: *const c_char) -> c_int {
    if path.is_null() { return 0; }
    let Ok(s) = (unsafe { std::ffi::CStr::from_ptr(path).to_str() }) else { return 0 };
    if query_size(s).is_some() { 1 } else { 0 }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_copy_file(
    src: *const c_char,
    dst: *const c_char,
) -> c_int {
    if src.is_null() || dst.is_null() { return -1; }
    let Ok(s) = (unsafe { std::ffi::CStr::from_ptr(src).to_str() }) else { return -1 };
    let Ok(_d) = (unsafe { std::ffi::CStr::from_ptr(dst).to_str() }) else { return -1 };
    let _data = match request_file(s) {
        Ok(v) => v,
        Err(_) => return -1,
    };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_delete_file(path: *const c_char) -> c_int {
    if path.is_null() { return -1; }
    0
}

// ── Runtime control FFI ─────────────────────────────────────────

use crate::runtime::CoreRuntime;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_create(
    w: u32,
    h: u32,
    backend: i32,
) -> *mut CoreRuntime {
    let b = crate::backend::gl::platform::GfxBackend::from_int(backend);
    match CoreRuntime::create(w, h, b) {
        Ok(rt) => Box::into_raw(Box::new(rt)),
        Err(e) => {
            core_error!("art3m1s_runtime_create: {e}");
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_load_project(
    rt: *mut CoreRuntime,
    ini_content: *const c_char,
    platform: *const c_char,
) -> i32 {
    if rt.is_null() || ini_content.is_null() || platform.is_null() { return -1; }
    let rt = unsafe { &mut *rt };
    let Ok(ini) = (unsafe { std::ffi::CStr::from_ptr(ini_content).to_str() }) else { return -1 };
    let Ok(plat) = (unsafe { std::ffi::CStr::from_ptr(platform).to_str() }) else { return -1 };
    match rt.load_project(ini, plat) {
        Ok(()) => 0,
        Err(e) => {
            core_error!("art3m1s_runtime_load_project: {e}");
            -1
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_feed_mouse(
    rt: *mut CoreRuntime,
    x: i32,
    y: i32,
) {
    if rt.is_null() { return; }
    let rt = unsafe { &*rt };
    rt.feed_mouse(x, y);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_feed_click(rt: *mut CoreRuntime) {
    if rt.is_null() { return; }
    let rt = unsafe { &*rt };
    rt.feed_click();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_feed_key(
    rt: *mut CoreRuntime,
    vk: u32,
    pressed: i32,
) {
    if rt.is_null() { return; }
    let rt = unsafe { &*rt };
    if pressed != 0 {
        rt.feed_key_down(vk);
    } else {
        rt.feed_key_up(vk);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_destroy(rt: *mut CoreRuntime) {
    if !rt.is_null() {
        drop(unsafe { Box::from_raw(rt) });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_advance_and_render(
    rt: *mut CoreRuntime,
    delta_ms: u32,
    out_pixels: *mut u8,
    out_capacity: u32,
) -> u32 {
    if rt.is_null() || out_pixels.is_null() { return 0; }
    let rt = unsafe { &mut *rt };
    let pixels = rt.advance_and_render(delta_ms as u64);
    let to_copy = pixels.len().min(out_capacity as usize);
    unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), out_pixels, to_copy); }
    to_copy as u32
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_set_volume(
    rt: *mut CoreRuntime,
    volume_type: *const c_char,
    value: f32,
) {
    if rt.is_null() || volume_type.is_null() { return; }
    let rt = unsafe { &*rt };
    let Ok(ty) = (unsafe { std::ffi::CStr::from_ptr(volume_type).to_str() }) else { return };
    rt.set_volume(ty, value);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_is_exit_requested(rt: *const CoreRuntime) -> i32 {
    if rt.is_null() { return 0; }
    let rt = unsafe { &*rt };
    if rt.is_exit_requested() { 1 } else { 0 }
}
