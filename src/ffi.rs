//! C FFI bridge — all host ↔ core communication.
//!
//! The Flutter frontend registers callbacks at startup; afterwards every
//! filesystem operation inside the core is routed through those callbacks,
//! keeping the core entirely free of direct I/O.
use std::ffi::{CString, c_char, c_int, c_longlong};
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

// ── Global debug flag ──────────────────────────────────────────

static DEBUG: AtomicBool = AtomicBool::new(false);

// ── 脚本 [debug] 标签的日志模式/级别 ───────────────────────────
//
// 文档语义：mode 0=禁用日志（产品版）、1=输出到控制台、2=IPC 输出（Windows
// 遗留，这里与 1 同样走宿主日志回调）；level 控制输出级别。启动默认
// mode=0 level=0。[debugprint] 只有在 mode!=0 且自身 level 不超过当前
// level 设置时才输出。存成进程级原子量：日志配置本就是全局的，且
// CoreRuntime 结构体不归本模块管。

static SCRIPT_DEBUG_MODE: AtomicI32 = AtomicI32::new(0);
static SCRIPT_DEBUG_LEVEL: AtomicI32 = AtomicI32::new(0);

/// 应用 `[debug]` 标签：mode/level 缺省时保持之前设置（文档行为）。
pub fn set_script_debug_config(mode: Option<i32>, level: Option<i32>) {
    if let Some(mode) = mode {
        SCRIPT_DEBUG_MODE.store(mode, Ordering::Relaxed);
    }
    if let Some(level) = level {
        SCRIPT_DEBUG_LEVEL.store(level, Ordering::Relaxed);
    }
}

pub fn script_debug_mode() -> i32 {
    SCRIPT_DEBUG_MODE.load(Ordering::Relaxed)
}

pub fn script_debug_level() -> i32 {
    SCRIPT_DEBUG_LEVEL.load(Ordering::Relaxed)
}

/// `[debugprint level=N]` 是否应输出。
///
/// mode=0 一律不输出；level=0 是"仅脚本日志"档，故门控取
/// `N <= 当前 level`（debugprint 属于脚本日志，level=0 时也放行 N=0）。
pub fn script_debug_print_allowed(level: i32) -> bool {
    script_debug_mode() != 0 && level <= script_debug_level()
}

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
    DEBUG.store(enabled != 0, Ordering::Relaxed);
}

pub fn debug_enabled() -> bool {
    DEBUG.load(Ordering::Relaxed)
}

// ── Log callback ───────────────────────────────────────────────
//
// 回调指针一律用 Mutex<Option<..>> 而非 OnceLock：Flutter 热重启后会用新的
// trampoline 地址重新注册，旧指针必须允许被覆盖，否则调用悬垂指针。

type LogCallback = unsafe extern "C" fn(level: *const c_char, msg: *const c_char);

static LOG_CB: Mutex<Option<LogCallback>> = Mutex::new(None);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_register_log_callback(cb: LogCallback) {
    *LOG_CB.lock().unwrap() = Some(cb);
}

// ── 日志过滤钩子（setLogFilter 的 core 侧）────────────────────────
//
// Artemis 的 e:setLogFilter 允许 Lua 函数在日志输出前拦截：返回 1 抑制
// 原始日志（过滤器内可用 e:debug 输出改写后的日志）。core 的日志输出
// 汇聚在 [`log`]，这里提供进程级过滤钩子；解释器侧注册 Lua 过滤函数的
// 绑定落地后，把"调用 Lua 过滤器"的闭包装进来即可。
//
// 重入保护：过滤器自身输出的日志（e:debug）不再进过滤器，防递归。

/// 过滤钩子：`(level, msg) -> true 表示抑制该条日志`。
pub type LogFilterHook = Box<dyn Fn(&str, &str) -> bool + Send + Sync>;

static LOG_FILTER: Mutex<Option<LogFilterHook>> = Mutex::new(None);

thread_local! {
    /// 当前线程是否正在过滤器内（此时产生的日志绕过过滤，防递归）。
    static IN_LOG_FILTER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// 安装/卸载日志过滤钩子（None=卸载）。
pub fn set_log_filter(hook: Option<LogFilterHook>) {
    *LOG_FILTER.lock().unwrap() = hook;
}

/// 日志是否应被过滤器抑制。过滤器内产生的日志一律放行。
fn log_suppressed_by_filter(level: &str, msg: &str) -> bool {
    if IN_LOG_FILTER.with(|flag| flag.get()) {
        return false;
    }
    // 把钩子从槽里短暂取出来调用，避免过滤器内再打日志时死锁 Mutex。
    let Some(hook) = LOG_FILTER.lock().unwrap().take() else {
        return false;
    };
    IN_LOG_FILTER.with(|flag| flag.set(true));
    let suppressed = hook(level, msg);
    IN_LOG_FILTER.with(|flag| flag.set(false));
    // 归还钩子（期间若有人重装了新钩子，以新钩子为准）。
    let mut slot = LOG_FILTER.lock().unwrap();
    if slot.is_none() {
        *slot = Some(hook);
    }
    suppressed
}

pub fn log(level: &str, msg: &str) {
    if log_suppressed_by_filter(level, msg) {
        return;
    }
    let Some(cb) = *LOG_CB.lock().unwrap() else {
        return;
    };
    if let (Ok(l), Ok(m)) = (CString::new(level), CString::new(msg)) {
        unsafe {
            cb(l.as_ptr(), m.as_ptr());
        }
    }
}

// ── Media / UI command callbacks ───────────────────────────────

/// `(kind, payload_json)` 形式的宿主命令回调，media 与 ui 通道共用同一签名。
type JsonCommandCallback = unsafe extern "C" fn(kind: *const c_char, payload_json: *const c_char);

static MEDIA_COMMAND_CB: Mutex<Option<JsonCommandCallback>> = Mutex::new(None);
static UI_COMMAND_CB: Mutex<Option<JsonCommandCallback>> = Mutex::new(None);

fn emit_json_command(
    slot: &Mutex<Option<JsonCommandCallback>>,
    kind: &str,
    payload: serde_json::Value,
) {
    let Some(cb) = *slot.lock().unwrap() else {
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

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_register_media_command_callback(cb: JsonCommandCallback) {
    *MEDIA_COMMAND_CB.lock().unwrap() = Some(cb);
}

pub fn media_command_callback_registered() -> bool {
    MEDIA_COMMAND_CB.lock().unwrap().is_some()
}

pub fn emit_media_command(kind: &str, payload: serde_json::Value) {
    emit_json_command(&MEDIA_COMMAND_CB, kind, payload);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_register_ui_command_callback(cb: JsonCommandCallback) {
    *UI_COMMAND_CB.lock().unwrap() = Some(cb);
}

pub fn ui_command_callback_registered() -> bool {
    UI_COMMAND_CB.lock().unwrap().is_some()
}

pub fn emit_ui_command(kind: &str, payload: serde_json::Value) {
    emit_json_command(&UI_COMMAND_CB, kind, payload);
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

// ── Text inject callback ───────────────────────────────────────
//
// 汉化/本地化补丁入口：宿主注册回调后，每段剧本文本在光栅化前都会先经过它。
// 协议：`text` 为原文（UTF-8，NUL 结尾）；替换文本写入 `buf`（UTF-8，不含
// NUL，最多 `buf_cap` 字节），返回写入的字节数；-1 表示不替换，-2 表示
// 宿主需要后台翻译。core 会立即显示原文并继续派发事件，再经 ui_command 的
// `text_translate` 下发请求；完成后由 `art3m1s_runtime_submit_text_translation`
// 尝试热替换仍位于当前页面、且已完成逐字显示的文本片段。

type TextInjectCallback =
    unsafe extern "C" fn(text: *const c_char, buf: *mut u8, buf_cap: c_int) -> c_int;

static TEXT_INJECT_CB: Mutex<Option<TextInjectCallback>> = Mutex::new(None);

/// 替换文本的最大字节数。单段剧本文本远小于此值。
const TEXT_INJECT_CAP: usize = 8192;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextInjectResult {
    Unchanged,
    Replaced(String),
    Pending,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_register_text_inject_callback(cb: TextInjectCallback) {
    *TEXT_INJECT_CB.lock().unwrap() = Some(cb);
}

/// 把一段文本交给宿主注入回调；Pending 只表示排队，不阻塞 runtime。
pub fn request_text_injection(text: &str) -> TextInjectResult {
    let Some(cb) = *TEXT_INJECT_CB.lock().unwrap() else {
        return TextInjectResult::Unchanged;
    };
    let Ok(c_text) = CString::new(text) else {
        return TextInjectResult::Unchanged;
    };
    let mut buf = vec![0u8; TEXT_INJECT_CAP];
    let n = unsafe { cb(c_text.as_ptr(), buf.as_mut_ptr(), buf.len() as c_int) };
    if n == -2 {
        if ui_command_callback_registered() {
            return TextInjectResult::Pending;
        }
        core_warn!("text inject 请求异步翻译，但宿主未注册 UI 回调，保持原文");
        return TextInjectResult::Unchanged;
    }
    if n < 0 || n as usize > buf.len() {
        return TextInjectResult::Unchanged;
    }
    buf.truncate(n as usize);
    match String::from_utf8(buf) {
        Ok(s) => TextInjectResult::Replaced(s),
        Err(_) => {
            core_warn!("text inject 回调返回了非 UTF-8 内容，忽略替换");
            TextInjectResult::Unchanged
        }
    }
}

/// 旧同步接口：异步 pending 对旧调用方表现为不替换。
pub fn inject_text(text: &str) -> Option<String> {
    match request_text_injection(text) {
        TextInjectResult::Replaced(text) => Some(text),
        TextInjectResult::Unchanged | TextInjectResult::Pending => None,
    }
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
///
/// 回调没有 offset 参数、无法续写，所以部分写入视为失败。
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
    if n as usize != data.len() {
        return Err(format!(
            "partial write: {path} ({n} of {} bytes)",
            data.len()
        ));
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

// ── File stat callback（存档文件更新时间查询）────────────────────
//
// `var system=file_update_time` 需要存档文件的修改时间。存档在应用沙箱内由
// 宿主管理，core 不直接 stat 文件系统；宿主注册回调，把本地时间分量
// [年,月,日,时,分,秒] 写入 out（时区换算由宿主完成）。返回写入的分量数
// （应为 6），文件不存在或失败时返回 <0。

type FileStatCallback =
    unsafe extern "C" fn(path: *const c_char, out_components: *mut i64, out_len: c_int) -> c_int;

static FILE_STAT: Mutex<Option<FileStatCallback>> = Mutex::new(None);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_register_file_stat(cb: FileStatCallback) {
    *FILE_STAT.lock().unwrap() = Some(cb);
}

/// 查询文件更新时间的本地时间分量 [年,月,日,时,分,秒]。
/// 未注册回调或文件不存在时返回 None。
pub fn request_file_mtime(path: &str) -> Option<[i64; 6]> {
    let cb = (*FILE_STAT.lock().unwrap())?;
    let c_path = CString::new(path).ok()?;
    let mut out = [0i64; 6];
    let n = unsafe { cb(c_path.as_ptr(), out.as_mut_ptr(), out.len() as c_int) };
    if n >= 6 { Some(out) } else { None }
}

// ── Clipboard ────────────────────────────────────────────────────

/// `e:writeClipboard` 的核心侧出口：经 ui_command 转发宿主
/// （Flutter Clipboard.setData）。原版仅 Windows；非 Windows 宿主可忽略。
pub fn write_clipboard(text: &str) {
    emit_ui_command("write_clipboard", serde_json::json!({ "string": text }));
}

// ── 字体枚举 / 窗口状态查询 ────────────────────────────────────────
//
// `var system=get_font` 与 `fullscreen`/`minimize` 需要宿主（Flutter）回答
// 可用字体族与窗口状态。宿主注册这两个查询回调后即返回真实数据；未注册时
// 保持保守默认（空字体列表 / 非全屏非最小化）。

/// 字体列表查询：`monospace`/`vertical` 为过滤标志（非 0 表示只要等宽/竖排）。
/// 结果为换行分隔的字体族名写入 `buf`（UTF-8，最多 `buf_cap` 字节），返回写入
/// 字节数；<0 表示无结果。
type FontQueryCallback =
    unsafe extern "C" fn(monospace: c_int, vertical: c_int, buf: *mut u8, buf_cap: c_int) -> c_int;

/// 窗口状态查询：返回位标志 bit0=全屏、bit1=最小化。
type WindowStateCallback = unsafe extern "C" fn() -> c_int;

static FONT_QUERY_CB: Mutex<Option<FontQueryCallback>> = Mutex::new(None);
static WINDOW_STATE_CB: Mutex<Option<WindowStateCallback>> = Mutex::new(None);

const FONT_LIST_CAP: usize = 16384;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_register_font_query(cb: FontQueryCallback) {
    *FONT_QUERY_CB.lock().unwrap() = Some(cb);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_register_window_state_query(cb: WindowStateCallback) {
    *WINDOW_STATE_CB.lock().unwrap() = Some(cb);
}

/// 查询可用字体族列表。未注册宿主回调时返回空列表。
pub fn query_font_list(monospace: bool, vertical: bool) -> Vec<String> {
    let Some(cb) = *FONT_QUERY_CB.lock().unwrap() else {
        return Vec::new();
    };
    let mut buf = vec![0u8; FONT_LIST_CAP];
    let n = unsafe {
        cb(
            monospace as c_int,
            vertical as c_int,
            buf.as_mut_ptr(),
            buf.len() as c_int,
        )
    };
    if n < 0 || n as usize > buf.len() {
        return Vec::new();
    }
    buf.truncate(n as usize);
    String::from_utf8_lossy(&buf)
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// 查询窗口状态：`(全屏, 最小化)`。未注册宿主回调时返回 `(false, false)`。
pub fn query_window_state() -> (bool, bool) {
    let Some(cb) = *WINDOW_STATE_CB.lock().unwrap() else {
        return (false, false);
    };
    let flags = unsafe { cb() };
    (flags & 0b01 != 0, flags & 0b10 != 0)
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
        if n as u64 != total {
            return Err(format!("short read: {path} ({n} of {total} bytes)"));
        }
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
    if off != total {
        return Err(format!("short read: {path} ({off} of {total} bytes)"));
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
    let Ok(d) = (unsafe { std::ffi::CStr::from_ptr(dst).to_str() }) else {
        return -1;
    };
    let data = match request_file(s) {
        Ok(v) => v,
        Err(e) => {
            core_warn!("art3m1s_copy_file: read {s}: {e}");
            return -1;
        }
    };
    match request_write(d, &data) {
        Ok(()) => 0,
        Err(e) => {
            core_warn!("art3m1s_copy_file: write {d}: {e}");
            -1
        }
    }
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

/// Headless 探测游戏 caption（导入时用；见 [`crate::probe_caption_from_bytes`]）。
/// 只跑解释器到发出第一个 `[caption]` 即停，不建 GL/compositor，近乎瞬时。把 caption 的
/// UTF-8 写入 `out_buf`（≤`out_cap`），返回写入字节数；无 caption / 缓冲不足 / 出错返回 0。
/// 宿主须在调用前把文件供给（目录/pfs）指向该游戏，否则 boot 脚本读不到直接返回 0。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_probe_caption(
    ini_content: *const u8,
    ini_len: usize,
    platform: *const c_char,
    out_buf: *mut u8,
    out_cap: c_int,
) -> c_int {
    if ini_content.is_null() || platform.is_null() || out_buf.is_null() || out_cap <= 0 {
        return 0;
    }
    let ini = unsafe { std::slice::from_raw_parts(ini_content, ini_len) };
    let Ok(plat) = (unsafe { std::ffi::CStr::from_ptr(platform).to_str() }) else {
        return 0;
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::probe_caption_from_bytes(ini, plat)
    }));
    let caption = match result {
        Ok(Some(c)) => c,
        Ok(None) => return 0,
        Err(panic_info) => {
            core_error!("art3m1s_probe_caption panicked: {}", panic_msg(&panic_info));
            return 0;
        }
    };
    let bytes = caption.as_bytes();
    if bytes.is_empty() || bytes.len() > out_cap as usize {
        return 0;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf, bytes.len());
    }
    bytes.len() as c_int
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

/// 宿主投喂一次触摸事件：`id` 触摸点唯一标识（手指），`phase` 0=down/1=move/2=up，
/// `x`/`y` 为舞台坐标。getTouchCount / getTouchPoint 从这些数据读真实触摸态。
#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_feed_touch(
    rt: *mut CoreRuntime,
    id: u32,
    phase: u8,
    x: i32,
    y: i32,
) {
    if rt.is_null() {
        return;
    }
    let rt = unsafe { &*rt };
    rt.feed_touch(id, phase, x, y);
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
pub unsafe extern "C" fn art3m1s_runtime_submit_dialog(
    rt: *mut CoreRuntime,
    accepted: i32,
    text: *const c_char,
) -> i32 {
    if rt.is_null() {
        return 0;
    }
    let text = if text.is_null() {
        None
    } else {
        unsafe { std::ffi::CStr::from_ptr(text).to_str().ok() }
    };
    let rt = unsafe { &mut *rt };
    i32::from(rt.submit_dialog_response(accepted != 0, text))
}

/// 回填宿主异步翻译结果。`text == NULL` 表示翻译失败，按原文继续。
#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_submit_text_translation(
    rt: *mut CoreRuntime,
    serial: u64,
    text: *const c_char,
) -> i32 {
    if rt.is_null() {
        return 0;
    }
    let text = if text.is_null() {
        None
    } else {
        unsafe { std::ffi::CStr::from_ptr(text).to_str().ok() }
    };
    let rt = unsafe { &mut *rt };
    i32::from(rt.submit_text_translation(serial, text))
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
    let out_capacity = out_capacity as usize;
    if out_capacity < rt.pixel_buffer_size() {
        return 0;
    }
    let out_pixels = unsafe { std::slice::from_raw_parts_mut(out_pixels, out_capacity) };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.advance_and_render_into(delta_ms as u64, out_pixels)
    }));
    match result {
        Ok(written) => written as u32,
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

/// Upload one RGBA8 frame for a currently playing video layer.
///
/// This call is synchronous. `rgba` is borrowed only for the duration of the
/// call and is passed directly to GL without an intermediate CPU-side copy.
/// The host must serialize this with other calls using the same runtime.
///
/// Returns 1 on success and 0 for invalid arguments, a stale layer, or failure.
#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_upload_video_layer_frame(
    rt: *mut CoreRuntime,
    id: *const c_char,
    width: u32,
    height: u32,
    rgba: *const u8,
    rgba_len: usize,
) -> c_int {
    if rt.is_null() || id.is_null() || rgba.is_null() || width == 0 || height == 0 {
        return 0;
    }
    let Ok(id) = (unsafe { std::ffi::CStr::from_ptr(id).to_str() }) else {
        return 0;
    };
    if id.is_empty() {
        return 0;
    }
    let Some(expected_len) = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
    else {
        return 0;
    };
    if rgba_len < expected_len {
        return 0;
    }

    let rgba = unsafe { std::slice::from_raw_parts(rgba, expected_len) };
    let rt = unsafe { &mut *rt };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.upload_video_layer_frame(id, width, height, rgba)
    })) {
        Ok(true) => 1,
        Ok(false) => 0,
        Err(panic_info) => {
            core_error!(
                "art3m1s_runtime_upload_video_layer_frame panicked: {}",
                panic_msg(&panic_info)
            );
            0
        }
    }
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

/// 宿主生命周期通知：state 0=引擎退出前、1=切到后台、2=回到前台。
/// [autosave allow=1] 时核心在退出/切后台时自动保存。
#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_notify_lifecycle(rt: *mut CoreRuntime, state: c_int) {
    if rt.is_null() {
        return;
    }
    let rt = unsafe { &mut *rt };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.notify_lifecycle(state);
    }));
    if let Err(panic_info) = result {
        core_error!(
            "art3m1s_runtime_notify_lifecycle panicked: {}",
            panic_msg(&panic_info)
        );
    }
}

/// 宿主窗口按钮按下（setonwindowbutton，仅 Windows）：
/// button 0=关闭(×) / 1=最大化 / 2=最小化。
#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_notify_window_button(rt: *mut CoreRuntime, button: c_int) {
    if rt.is_null() {
        return;
    }
    let rt = unsafe { &mut *rt };
    rt.notify_window_button(button);
}

/// 宿主屏幕方向变化（setondirchg，仅 iOS）：
/// direction 0=纵向 / 1=横向Home右 / 2=倒置纵向 / 3=横向Home左。
#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_notify_direction_changed(
    rt: *mut CoreRuntime,
    direction: c_int,
) {
    if rt.is_null() {
        return;
    }
    let rt = unsafe { &mut *rt };
    rt.notify_direction_changed(direction);
}

/// 宿主回填 httpget/httppost 的结果：status_code 为 HTTP 响应码（失败传 0），
/// body 为响应体字节（可为 NULL）。返回 1 表示有挂起请求被完成。
#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_submit_http_result(
    rt: *mut CoreRuntime,
    status_code: c_int,
    body: *const u8,
    body_len: c_int,
) -> c_int {
    if rt.is_null() {
        return 0;
    }
    let body = if body.is_null() || body_len <= 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(body, body_len as usize) }
    };
    let rt = unsafe { &mut *rt };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.submit_http_result(status_code, body)
    }));
    match result {
        Ok(true) => 1,
        Ok(false) => 0,
        Err(panic_info) => {
            core_error!(
                "art3m1s_runtime_submit_http_result panicked: {}",
                panic_msg(&panic_info)
            );
            0
        }
    }
}

/// 宿主把字符串结果写回解释器变量（callnative/purchase 的结果回注通道，
/// 支持 `result.title` 等子键路径）。
#[cfg(feature = "gl-backend")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_runtime_set_string_variable(
    rt: *mut CoreRuntime,
    name: *const c_char,
    value: *const c_char,
) {
    if rt.is_null() || name.is_null() || value.is_null() {
        return;
    }
    let Ok(name) = (unsafe { std::ffi::CStr::from_ptr(name).to_str() }) else {
        return;
    };
    let Ok(value) = (unsafe { std::ffi::CStr::from_ptr(value).to_str() }) else {
        return;
    };
    let rt = unsafe { &mut *rt };
    rt.set_string_variable(name, value);
}

#[cfg(test)]
mod tests {
    use super::{
        log_suppressed_by_filter, script_debug_print_allowed, set_log_filter,
        set_script_debug_config,
    };

    /// 日志过滤钩子是进程级状态，单测里串行验证后卸载，避免影响其它测试。
    #[test]
    fn log_filter_hook_suppresses_and_guards_reentrancy() {
        // 未安装钩子：一律放行
        assert!(!log_suppressed_by_filter("I", "hello"));

        // 安装：返回 true 抑制含 "noisy" 的日志；过滤器内再打日志不得递归。
        set_log_filter(Some(Box::new(|_level, msg| {
            // 过滤器内的日志输出（等价 e:debug）应绕过过滤器直接放行
            assert!(!super::log_suppressed_by_filter("D", "inner log"));
            msg.contains("noisy")
        })));
        assert!(log_suppressed_by_filter("I", "noisy line"));
        assert!(!log_suppressed_by_filter("I", "normal line"));

        // 卸载后恢复放行
        set_log_filter(None);
        assert!(!log_suppressed_by_filter("I", "noisy line"));
    }

    /// 全局原子量的门控逻辑放在同一个测试里串行验证，避免并行测试互踩。
    #[test]
    fn script_debug_config_gates_debugprint_output() {
        // 启动默认 mode=0 level=0：任何 debugprint 都不输出。
        set_script_debug_config(Some(0), Some(0));
        assert!(!script_debug_print_allowed(0));
        assert!(!script_debug_print_allowed(3));

        // mode=1 level=0：仅放行 level<=0 的脚本日志。
        set_script_debug_config(Some(1), None);
        assert!(script_debug_print_allowed(0));
        assert!(!script_debug_print_allowed(1));

        // level=2：放行 0..=2，拦下 3。
        set_script_debug_config(None, Some(2));
        assert!(script_debug_print_allowed(2));
        assert!(!script_debug_print_allowed(3));

        // mode/level 缺省时保持之前设置（文档："缺省=保持之前设置"）。
        set_script_debug_config(None, None);
        assert!(script_debug_print_allowed(2));

        // mode=2（IPC 模式）也按"非 0 即输出"处理。
        set_script_debug_config(Some(2), Some(3));
        assert!(script_debug_print_allowed(3));

        // 回到禁用态，避免影响其它依赖默认值的行为。
        set_script_debug_config(Some(0), Some(0));
        assert!(!script_debug_print_allowed(0));
    }
}
