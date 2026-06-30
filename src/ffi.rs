//! C FFI bridge — all host ↔ core communication.
//!
//! The Flutter frontend registers callbacks at startup; afterwards every
//! filesystem operation inside the core is routed through those callbacks,
//! keeping the core entirely free of direct I/O.
use std::ffi::{CString, c_char, c_int, c_longlong};
use std::sync::{Mutex, OnceLock};

// ── Global debug flag ──────────────────────────────────────────

static DEBUG: OnceLock<bool> = OnceLock::new();

/// 从 catch_unwind 的 payload 提取 panic message。
fn panic_msg(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

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
            unsafe {
                cb(l.as_ptr(), m.as_ptr());
            }
        }
    }
}

// ── Media command callback ─────────────────────────────────────

type MediaCommandCallback = unsafe extern "C" fn(kind: *const c_char, payload_json: *const c_char);

static MEDIA_COMMAND_CB: Mutex<Option<MediaCommandCallback>> = Mutex::new(None);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_register_media_command_callback(cb: MediaCommandCallback) {
    *MEDIA_COMMAND_CB.lock().unwrap() = Some(cb);
}

pub fn media_command_callback_registered() -> bool {
    MEDIA_COMMAND_CB.lock().unwrap().is_some()
}

pub fn emit_media_command(kind: &str, payload: serde_json::Value) {
    let Some(cb) = *MEDIA_COMMAND_CB.lock().unwrap() else {
        return;
    };
    let Ok(kind) = CString::new(kind) else {
        return;
    };
    let Ok(payload) = CString::new(payload.to_string()) else {
        return;
    };
    unsafe {
        cb(kind.as_ptr(), payload.as_ptr());
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

static FILE_READER: Mutex<Option<FileReaderCallback>> = Mutex::new(None);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_register_file_reader(cb: FileReaderCallback) {
    *FILE_READER.lock().unwrap() = Some(cb);
}

pub fn file_reader_registered() -> bool {
    FILE_READER.lock().unwrap().is_some()
}

// ── File writer / delete callbacks ──────────────────────────────
//
// 方案 B：通过宿主（Flutter）注册的回调落盘到应用沙箱目录。
// core 只传脚本相对路径；物理路径由宿主决定，core 不直接读写文件系统。

/// 写文件回调：`path` 相对路径，`buf`/`len` 为待写字节。返回写入字节数，<0 表失败。
type FileWriterCallback =
    unsafe extern "C" fn(path: *const c_char, buf: *const u8, len: c_int) -> c_int;

/// 删除文件回调：`path` 相对路径。返回 0 成功，<0 失败。
type FileDeleteCallback = unsafe extern "C" fn(path: *const c_char) -> c_int;

static FILE_WRITER: Mutex<Option<FileWriterCallback>> = Mutex::new(None);
static FILE_DELETE: Mutex<Option<FileDeleteCallback>> = Mutex::new(None);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_register_file_writer(cb: FileWriterCallback) {
    *FILE_WRITER.lock().unwrap() = Some(cb);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_register_file_delete(cb: FileDeleteCallback) {
    *FILE_DELETE.lock().unwrap() = Some(cb);
}

/// 通过宿主回调写入文件。`path` 为相对路径。
pub fn request_write(path: &str, data: &[u8]) -> Result<(), String> {
    let cb = FILE_WRITER
        .lock()
        .unwrap()
        .ok_or_else(|| "file writer not registered".to_string())?;
    let c_path = CString::new(path).map_err(|e| e.to_string())?;
    let n = unsafe { cb(c_path.as_ptr(), data.as_ptr(), data.len() as c_int) };
    if n < 0 {
        return Err(format!("write failed: {path}"));
    }
    Ok(())
}

/// 通过宿主回调删除文件。`path` 为相对路径。
pub fn request_delete(path: &str) -> Result<(), String> {
    let cb = FILE_DELETE
        .lock()
        .unwrap()
        .ok_or_else(|| "file delete not registered".to_string())?;
    let c_path = CString::new(path).map_err(|e| e.to_string())?;
    let r = unsafe { cb(c_path.as_ptr()) };
    if r < 0 {
        return Err(format!("delete failed: {path}"));
    }
    Ok(())
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
    let cb = FILE_READER.lock().unwrap().clone()?;
    let c_path = CString::new(path).ok()?;
    let size = unsafe { cb(c_path.as_ptr(), std::ptr::null_mut(), 0, -1) };
    if size >= 0 { Some(size as u64) } else { None }
}

fn read_chunk(path: &str, offset: u64, buf: &mut [u8]) -> Option<usize> {
    let cb = FILE_READER.lock().unwrap().clone()?;
    let c_path = CString::new(path).ok()?;
    let n = unsafe {
        cb(
            c_path.as_ptr(),
            buf.as_mut_ptr(),
            buf.len() as c_int,
            offset as c_longlong,
        )
    };
    if n >= 0 { Some(n as usize) } else { None }
}

const CHUNK: usize = 65536;
const MAX_SINGLE: u64 = 16 * 1024 * 1024;

pub fn request_file(path: &str) -> Result<Vec<u8>, String> {
    let total = query_size(path).ok_or_else(|| format!("not found: {path}"))?;
    if total == 0 {
        return Ok(Vec::new());
    }
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
        if n == 0 {
            break;
        }
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

pub fn request_asset(path: &str) -> Option<Vec<u8>> {
    request_file(path).ok()
}
pub fn request_asset_range(path: &str, offset: u64, len: usize) -> Option<Vec<u8>> {
    request_file_range(path, offset, len)
}
pub fn query_asset_size(path: &str) -> Option<u64> {
    query_size(path)
}

// ── File operations ──────────────────────────────────────────────

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_file_exists(path: *const c_char) -> c_int {
    if path.is_null() {
        return 0;
    }
    let Ok(s) = (unsafe { std::ffi::CStr::from_ptr(path).to_str() }) else {
        return 0;
    };
    if query_size(s).is_some() { 1 } else { 0 }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_copy_file(src: *const c_char, dst: *const c_char) -> c_int {
    if src.is_null() || dst.is_null() {
        return -1;
    }
    let Ok(s) = (unsafe { std::ffi::CStr::from_ptr(src).to_str() }) else {
        return -1;
    };
    let Ok(_d) = (unsafe { std::ffi::CStr::from_ptr(dst).to_str() }) else {
        return -1;
    };
    let _data = match request_file(s) {
        Ok(v) => v,
        Err(_) => return -1,
    };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_delete_file(path: *const c_char) -> c_int {
    if path.is_null() {
        return -1;
    }
    let Ok(s) = (unsafe { std::ffi::CStr::from_ptr(path).to_str() }) else {
        return -1;
    };
    match request_delete(s) {
        Ok(()) => 0,
        Err(e) => {
            core_warn!("art3m1s_delete_file: {e}");
            -1
        }
    }
}

// ── Runtime control FFI ─────────────────────────────────────────

#[cfg(feature = "gl-backend")]
use crate::runtime::CoreRuntime;

#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_create(w: u32, h: u32, backend: i32) -> *mut CoreRuntime {
    // catch_unwind 防止 panic 跨越 extern "C" 边界导致 abort，
    // 同时把 panic message 打印到日志方便定位。
    let b = crate::backend::gl::platform::GfxBackend::from_int(backend);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        CoreRuntime::create(w, h, b)
    }));
    match result {
        Ok(Ok(rt)) => Box::into_raw(Box::new(rt)),
        Ok(Err(e)) => {
            core_error!("art3m1s_runtime_create: {e}");
            std::ptr::null_mut()
        }
        Err(panic_info) => {
            core_error!(
                "art3m1s_runtime_create panicked: {}",
                panic_msg(&panic_info)
            );
            std::ptr::null_mut()
        }
    }
}

#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_load_project(
    rt: *mut CoreRuntime,
    ini_content: *const c_char,
    platform: *const c_char,
) -> i32 {
    if rt.is_null() || ini_content.is_null() || platform.is_null() {
        return -1;
    }
    let rt = unsafe { &mut *rt };
    let Ok(ini) = (unsafe { std::ffi::CStr::from_ptr(ini_content).to_str() }) else {
        return -1;
    };
    let Ok(plat) = (unsafe { std::ffi::CStr::from_ptr(platform).to_str() }) else {
        return -1;
    };
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rt.load_project(ini, plat)));
    match result {
        Ok(Ok(())) => 0,
        Ok(Err(e)) => {
            core_error!("art3m1s_runtime_load_project: {e}");
            -1
        }
        Err(panic_info) => {
            let msg = panic_msg(&panic_info);
            core_error!("art3m1s_runtime_load_project panicked: {msg}");
            -1
        }
    }
}

#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_load_project_bytes(
    rt: *mut CoreRuntime,
    ini_content: *const u8,
    ini_len: usize,
    platform: *const c_char,
) -> i32 {
    if rt.is_null() || ini_content.is_null() || platform.is_null() {
        return -1;
    }
    let rt = unsafe { &mut *rt };
    let ini = unsafe { std::slice::from_raw_parts(ini_content, ini_len) };
    let Ok(plat) = (unsafe { std::ffi::CStr::from_ptr(platform).to_str() }) else {
        return -1;
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.load_project_bytes(ini, plat)
    }));
    match result {
        Ok(Ok(())) => 0,
        Ok(Err(e)) => {
            core_error!("art3m1s_runtime_load_project_bytes: {e}");
            -1
        }
        Err(panic_info) => {
            let msg = panic_msg(&panic_info);
            core_error!("art3m1s_runtime_load_project_bytes panicked: {msg}");
            -1
        }
    }
}

#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_feed_mouse(rt: *mut CoreRuntime, x: i32, y: i32) {
    if rt.is_null() {
        return;
    }
    let rt = unsafe { &*rt };
    rt.feed_mouse(x, y);
}

#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_feed_click(rt: *mut CoreRuntime) {
    if rt.is_null() {
        return;
    }
    let rt = unsafe { &*rt };
    rt.feed_click();
}

#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_feed_mouse_button(
    rt: *mut CoreRuntime,
    button: u32,
    pressed: i32,
) {
    if rt.is_null() {
        return;
    }
    let rt = unsafe { &*rt };
    rt.feed_mouse_button(button, pressed != 0);
}

#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_feed_key(rt: *mut CoreRuntime, vk: u32, pressed: i32) {
    if rt.is_null() {
        return;
    }
    let rt = unsafe { &*rt };
    if pressed != 0 {
        rt.feed_key_down(vk);
    } else {
        rt.feed_key_up(vk);
    }
}

#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_destroy(rt: *mut CoreRuntime) {
    if !rt.is_null() {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drop(unsafe { Box::from_raw(rt) });
        }));
    }
}

#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_stage_width(rt: *const CoreRuntime) -> u32 {
    if rt.is_null() {
        return 0;
    }
    unsafe { &*rt }.stage_width()
}

#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_stage_height(rt: *const CoreRuntime) -> u32 {
    if rt.is_null() {
        return 0;
    }
    unsafe { &*rt }.stage_height()
}

#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_pixel_buffer_size(rt: *const CoreRuntime) -> u32 {
    if rt.is_null() {
        return 0;
    }
    unsafe { &*rt }.pixel_buffer_size() as u32
}

#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_advance_and_render(
    rt: *mut CoreRuntime,
    delta_ms: u32,
    out_pixels: *mut u8,
    out_capacity: u32,
) -> u32 {
    if rt.is_null() || out_pixels.is_null() {
        return 0;
    }
    let rt = unsafe { &mut *rt };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.advance_and_render(delta_ms as u64)
    }));
    match result {
        Ok(pixels) => {
            let to_copy = pixels.len().min(out_capacity as usize);
            unsafe {
                std::ptr::copy_nonoverlapping(pixels.as_ptr(), out_pixels, to_copy);
            }
            to_copy as u32
        }
        Err(panic_info) => {
            core_error!(
                "art3m1s_runtime_advance_and_render panicked: {}",
                panic_msg(&panic_info)
            );
            0
        }
    }
}

#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_set_volume(
    rt: *mut CoreRuntime,
    volume_type: *const c_char,
    value: f32,
) {
    if rt.is_null() || volume_type.is_null() {
        return;
    }
    let rt = unsafe { &mut *rt };
    let Ok(ty) = (unsafe { std::ffi::CStr::from_ptr(volume_type).to_str() }) else {
        return;
    };
    rt.set_volume(ty, value);
}

#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_notify_video_finished(
    rt: *mut CoreRuntime,
    id: *const c_char,
) {
    if rt.is_null() {
        return;
    }
    let rt = unsafe { &mut *rt };
    let id = if id.is_null() {
        None
    } else {
        unsafe { std::ffi::CStr::from_ptr(id).to_str().ok() }
    };
    rt.notify_video_finished(id);
}

#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_notify_sound_finished(
    rt: *mut CoreRuntime,
    id: *const c_char,
) {
    if rt.is_null() {
        return;
    }
    let rt = unsafe { &mut *rt };
    let id = if id.is_null() {
        None
    } else {
        unsafe { std::ffi::CStr::from_ptr(id).to_str().ok() }
    };
    rt.notify_sound_finished(id);
}

#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_is_exit_requested(rt: *const CoreRuntime) -> i32 {
    if rt.is_null() {
        return 0;
    }
    let rt = unsafe { &*rt };
    if rt.is_exit_requested() { 1 } else { 0 }
}
