//! Core-side implementations used by the versioned C ABI table.
//!
//! These functions are address-taken by [`crate::ffi::api::Art3m1sApiV1`].
//! They are intentionally not exported as individual dynamic-library symbols;
//! hosts must obtain them through [`crate::ffi::api::art3m1s_get_api_v1`].

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub mod api;

#[cfg(feature = "rfvp-engine")]
pub mod rfvp_api;

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
use std::ffi::c_void;
use std::ffi::{c_char, c_int};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

// ── Global debug flag ──────────────────────────────────────────

static DEBUG: AtomicBool = AtomicBool::new(false);
static DAMAGE_VISUALIZATION: AtomicBool = AtomicBool::new(false);
static PROFILE_IO: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct HostFfiProfile {
    pub calls: u64,
    pub elapsed_ns: u64,
    pub bytes: u64,
}

thread_local! {
    static HOST_FFI_PROFILE: std::cell::Cell<HostFfiProfile> =
        const { std::cell::Cell::new(HostFfiProfile { calls: 0, elapsed_ns: 0, bytes: 0 }) };
}

pub(crate) fn set_profile_io_enabled(enabled: bool) {
    PROFILE_IO.store(enabled, Ordering::Relaxed);
    if !enabled {
        let _ = take_profile_io_counters();
    }
}

pub(crate) fn take_profile_io_counters() -> HostFfiProfile {
    HOST_FFI_PROFILE.with(|cell| cell.replace(HostFfiProfile::default()))
}

fn begin_profile_io() -> Option<std::time::Instant> {
    PROFILE_IO
        .load(Ordering::Relaxed)
        .then(std::time::Instant::now)
}

fn finish_profile_io(started: Option<std::time::Instant>, bytes: usize) {
    let Some(started) = started else {
        return;
    };
    HOST_FFI_PROFILE.with(|cell| {
        let mut value = cell.get();
        value.calls = value.calls.saturating_add(1);
        value.elapsed_ns = value
            .elapsed_ns
            .saturating_add(started.elapsed().as_nanos().min(u64::MAX as u128) as u64);
        value.bytes = value.bytes.saturating_add(bytes as u64);
        cell.set(value);
    });
}

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

pub unsafe extern "C" fn art3m1s_set_debug(enabled: c_int) {
    let enabled = enabled != 0;
    DEBUG.store(enabled, Ordering::Relaxed);
    if !enabled {
        DAMAGE_VISUALIZATION.store(false, Ordering::Relaxed);
    }
}

pub fn debug_enabled() -> bool {
    DEBUG.load(Ordering::Relaxed)
}

pub unsafe extern "C" fn art3m1s_set_damage_visualization(enabled: c_int) {
    DAMAGE_VISUALIZATION.store(enabled != 0 && debug_enabled(), Ordering::Relaxed);
}

pub fn damage_visualization_enabled() -> bool {
    debug_enabled() && DAMAGE_VISUALIZATION.load(Ordering::Relaxed)
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
    if crate::host_events::enabled() {
        crate::host_events::push_log(level, msg);
    }
}

// ── Media / UI command events ──────────────────────────────────

fn emit_json_command(event_kind: u32, kind: &str, payload: serde_json::Value) {
    let payload = payload.to_string();
    if crate::host_events::enabled() {
        match event_kind {
            crate::host_events::EVENT_KIND_MEDIA => {
                crate::host_events::push_media(kind, &payload);
            }
            crate::host_events::EVENT_KIND_UI => {
                crate::host_events::push_ui(kind, &payload);
            }
            _ => {}
        }
    }
}

pub fn media_command_callback_registered() -> bool {
    crate::host_events::enabled()
}

pub fn emit_media_command(kind: &str, payload: serde_json::Value) {
    emit_json_command(crate::host_events::EVENT_KIND_MEDIA, kind, payload);
}

pub fn ui_command_callback_registered() -> bool {
    crate::host_events::enabled()
}

pub fn emit_ui_command(kind: &str, payload: serde_json::Value) {
    emit_json_command(crate::host_events::EVENT_KIND_UI, kind, payload);
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

// ── Text replacement state ─────────────────────────────────────
//
// 汉化/本地化补丁入口：Host 预先提交精确替换表；未命中且在线翻译开启时，
// core 保留原文并通过 UI event 发送 `text_translate` 请求，完成后由
// `art3m1s_runtime_submit_text_translation` 尝试热替换当前页面文本。

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextInjectResult {
    Unchanged,
    Replaced(String),
    Pending,
}

/// 把一段文本交给宿主注入回调；Pending 只表示排队，不阻塞 runtime。
pub fn request_text_injection(text: &str) -> TextInjectResult {
    if let Some(replaced) = crate::host_events::text_replacement(text) {
        return TextInjectResult::Replaced(replaced);
    }
    if crate::host_events::text_translation_enabled() {
        TextInjectResult::Pending
    } else {
        TextInjectResult::Unchanged
    }
}

/// 旧同步接口：异步 pending 对旧调用方表现为不替换。
pub fn inject_text(text: &str) -> Option<String> {
    match request_text_injection(text) {
        TextInjectResult::Replaced(text) => Some(text),
        TextInjectResult::Unchanged | TextInjectResult::Pending => None,
    }
}

// ── Runtime font override ──────────────────────────────────────
//
// 汉化/本地化补丁入口：游戏脚本指定的字体可能缺少译文字形（显示为空白）。
// 宿主安装一份覆盖字体后，所有脚本 face 请求都光栅化到该字体；清除后恢复
// 脚本字体。进程级全局，对所有 runtime 生效；世代号在每次 set 时递增，
// runtime 据此检测变更并立即重解当前字体。覆盖只作用于之后光栅化的文本
// （含异步译文热替换），不回溯已排版的既有字形。

struct FontOverride {
    generation: u64,
    bytes: Arc<[u8]>,
}

static FONT_OVERRIDE: Mutex<Option<FontOverride>> = Mutex::new(None);
static FONT_OVERRIDE_GENERATION: AtomicU64 = AtomicU64::new(0);

/// 校验并安装覆盖字体（Rust 宿主入口）。字节必须是合法的 sfnt（TTF/OTF）。
pub fn set_font_override(bytes: Vec<u8>) -> Result<(), String> {
    ab_glyph::FontRef::try_from_slice(&bytes)
        .map_err(|error| format!("覆盖字体不是合法的 sfnt 字体: {error}"))?;
    let generation = FONT_OVERRIDE_GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    *FONT_OVERRIDE.lock().unwrap() = Some(FontOverride {
        generation,
        bytes: bytes.into(),
    });
    Ok(())
}

/// 清除覆盖字体，恢复脚本指定字体（Rust 宿主入口）。
pub fn clear_font_override() {
    *FONT_OVERRIDE.lock().unwrap() = None;
}

/// 当前覆盖字体及其世代号（runtime 内部读取入口）。
pub(crate) fn font_override() -> Option<(u64, Arc<[u8]>)> {
    FONT_OVERRIDE
        .lock()
        .unwrap()
        .as_ref()
        .map(|override_| (override_.generation, Arc::clone(&override_.bytes)))
}

/// 安装运行时覆盖字体。`data`/`len` 为字体文件字节（TTF/OTF），core 内部复制。
/// 返回 1 成功；0 参数无效或字体解析失败。
pub unsafe extern "C" fn art3m1s_set_font_override(data: *const u8, len: c_int) -> c_int {
    if data.is_null() || len <= 0 {
        return 0;
    }
    let bytes = unsafe { std::slice::from_raw_parts(data, len as usize) }.to_vec();
    match set_font_override(bytes) {
        Ok(()) => 1,
        Err(error) => {
            core_warn!("art3m1s_set_font_override: {error}");
            0
        }
    }
}

/// 清除运行时覆盖字体，恢复脚本指定字体。
pub unsafe extern "C" fn art3m1s_clear_font_override() {
    clear_font_override();
}

// ── ANGLE library search path ──────────────────────────────────

pub unsafe extern "C" fn art3m1s_set_angle_path(path: *const c_char) {
    if let Ok(s) = unsafe { std::ffi::CStr::from_ptr(path).to_str() } {
        let _ = crate::backend::set_angle_path_prefix(s);
    }
}

// ── Native file host ───────────────────────────────────────────

pub fn clear_file_size_cache() {
    crate::host_files::clear_overrides();
}

pub fn file_reader_registered() -> bool {
    crate::host_files::is_mounted()
}

pub fn request_write(path: &str, data: &[u8]) -> Result<(), String> {
    crate::host_files::write(path, data)
}

pub fn request_delete(path: &str) -> Result<(), String> {
    crate::host_files::delete(path)
}

pub fn request_file_mtime(path: &str) -> Option<[i64; 6]> {
    crate::host_files::file_mtime(path)
}

// ── Clipboard ────────────────────────────────────────────────────

/// `e:writeClipboard` 的核心侧出口：经 ui_command 转发宿主
/// （Flutter Clipboard.setData）。原版仅 Windows；非 Windows 宿主可忽略。
pub fn write_clipboard(text: &str) {
    emit_ui_command("write_clipboard", serde_json::json!({ "string": text }));
}

// ── 字体枚举 / 窗口状态 ────────────────────────────────────────────
//
// `var system=get_font` 与 `fullscreen`/`minimize` 读取 Host 经
// `art3m1s_set_font_list_v1` / `art3m1s_set_window_state_v1` 推送的状态。

/// 查询可用字体族列表。Host 未推送时返回空列表。
pub fn query_font_list(monospace: bool, vertical: bool) -> Vec<String> {
    crate::host_events::query_font_list(monospace, vertical).unwrap_or_default()
}

/// 查询窗口状态：`(全屏, 最小化)`。Host 未推送时返回 `(false, false)`。
pub fn query_window_state() -> (bool, bool) {
    crate::host_events::query_window_state().unwrap_or((false, false))
}

// ── Query helpers ────────────────────────────────────────────────

fn query_size(path: &str) -> Option<u64> {
    let started = begin_profile_io();
    let result = crate::host_files::query_size(path).ok().flatten();
    finish_profile_io(started, 0);
    result
}

fn read_chunk(path: &str, offset: u64, buf: &mut [u8]) -> Option<usize> {
    let started = begin_profile_io();
    let result = crate::host_files::read_range(path, offset, buf).ok();
    finish_profile_io(started, result.unwrap_or(0));
    result
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

// ── Runtime control FFI ─────────────────────────────────────────

use crate::runtime::CoreRuntime;

/// Creates a runtime using the platform default GPU backend.
///
/// Value 0 selects the production platform default: Metal on Darwin and
/// experimental Vulkan on Android/Windows/Linux, with GL as initialization
/// fallback when it is built. Explicit native overrides are 2=Vulkan and
/// 3=Metal. Values 1/4/5/6 retain GL/ANGLE debug and A/B paths.
pub unsafe extern "C" fn art3m1s_runtime_create(w: u32, h: u32, backend: i32) -> *mut CoreRuntime {
    // catch_unwind 防止 panic 跨越 extern "C" 边界导致 abort，
    // 同时把 panic message 打印到日志方便定位。
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let b = crate::backend::BackendSelection::try_from_legacy_int(backend)?;
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

pub unsafe extern "C" fn art3m1s_runtime_set_resources(
    rt: *mut CoreRuntime,
    resources: *mut crate::host_files::HostResources,
) -> c_int {
    if rt.is_null() {
        return 0;
    }
    let resources = if resources.is_null() {
        crate::host_files::default_resources().clone()
    } else {
        unsafe { &*resources }.clone()
    };
    unsafe { &mut *rt }.set_resources(resources);
    1
}

/// Returns the active backend kind: 1=Metal, 2=Vulkan, 3=GL reference.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_backend_kind(rt: *const CoreRuntime) -> i32 {
    if rt.is_null() {
        return 0;
    }
    unsafe { &*rt }.backend_info().kind.ffi_value()
}

/// Returns the active stability level: 1=Production, 2=Experimental, 3=Legacy.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_backend_stability(rt: *const CoreRuntime) -> i32 {
    if rt.is_null() {
        return 0;
    }
    unsafe { &*rt }.backend_info().stability.ffi_value()
}

/// Returns the `BackendCapabilities` stable bit mask for the active backend.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_backend_capabilities(rt: *const CoreRuntime) -> u64 {
    if rt.is_null() {
        return 0;
    }
    unsafe { &*rt }.backend_info().capabilities.bits()
}

/// Registers an Artemis fragment HLSL shader. Returns its stable logical
/// ShaderId, or -1 when the name/source is invalid or compilation fails.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_register_hlsl_shader(
    rt: *mut CoreRuntime,
    name: *const c_char,
    source: *const u8,
    source_len: c_int,
) -> i64 {
    if rt.is_null() || name.is_null() || source.is_null() || source_len < 0 {
        return -1;
    }
    let Some(name) = (unsafe { std::ffi::CStr::from_ptr(name).to_str().ok() }) else {
        return -1;
    };
    let source = unsafe { std::slice::from_raw_parts(source, source_len as usize) };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        unsafe { &mut *rt }.register_hlsl_shader(name, source)
    }));
    match result {
        Ok(Ok(id)) => id.opaque() as i64,
        Ok(Err(error)) => {
            core_error!("runtime HLSL shader registration failed: {error}");
            -1
        }
        Err(panic_info) => {
            core_error!(
                "runtime HLSL shader registration panicked: {}",
                panic_msg(&panic_info)
            );
            -1
        }
    }
}

/// Replaces an Artemis HLSL shader and invalidates native pipelines derived
/// from the previous generation. Returns the stable ShaderId or -1.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_replace_hlsl_shader(
    rt: *mut CoreRuntime,
    name: *const c_char,
    source: *const u8,
    source_len: c_int,
) -> i64 {
    if rt.is_null() || name.is_null() || source.is_null() || source_len < 0 {
        return -1;
    }
    let Some(name) = (unsafe { std::ffi::CStr::from_ptr(name).to_str().ok() }) else {
        return -1;
    };
    let source = unsafe { std::slice::from_raw_parts(source, source_len as usize) };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        unsafe { &mut *rt }.replace_hlsl_shader(name, source)
    }));
    match result {
        Ok(Ok(id)) => id.opaque() as i64,
        Ok(Err(error)) => {
            core_error!("runtime HLSL shader replacement failed: {error}");
            -1
        }
        Err(panic_info) => {
            core_error!(
                "runtime HLSL shader replacement panicked: {}",
                panic_msg(&panic_info)
            );
            -1
        }
    }
}

/// Alias for replacement used by hosts that model shader updates as reloads.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_reload_hlsl_shader(
    rt: *mut CoreRuntime,
    name: *const c_char,
    source: *const u8,
    source_len: c_int,
) -> i64 {
    unsafe { art3m1s_runtime_replace_hlsl_shader(rt, name, source, source_len) }
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_unregister_hlsl_shader(
    rt: *mut CoreRuntime,
    name: *const c_char,
) -> c_int {
    if rt.is_null() || name.is_null() {
        return 0;
    }
    let Some(name) = (unsafe { std::ffi::CStr::from_ptr(name).to_str().ok() }) else {
        return 0;
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        (&mut *rt).unregister_hlsl_shader(name)
    }));
    match result {
        Ok(true) => 1,
        Ok(false) => 0,
        Err(panic_info) => {
            core_error!(
                "runtime HLSL shader removal panicked: {}",
                panic_msg(&panic_info)
            );
            0
        }
    }
}

/// Selects the E-Mote implementation before project loading.
///
/// `backend=0` keeps the built-in renderer. `backend=1` enables the optional
/// Eluna adapter when this core was built with `experimental-eluna`.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_set_emote_backend(
    rt: *mut CoreRuntime,
    backend: i32,
) -> c_int {
    if rt.is_null() {
        return 0;
    }
    let backend = crate::runtime::emote::EmoteBackend::from_int(backend);
    if backend == crate::runtime::emote::EmoteBackend::ElunaExperimental
        && !cfg!(feature = "experimental-eluna")
    {
        core_warn!(
            "Eluna E-Mote backend requested but this core lacks the experimental-eluna feature"
        );
        return 0;
    }
    let rt = unsafe { &mut *rt };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.set_emote_backend(backend)
    })) {
        Ok(()) => 1,
        Err(panic_info) => {
            core_error!(
                "art3m1s_runtime_set_emote_backend panicked: {}",
                panic_msg(&panic_info)
            );
            0
        }
    }
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
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

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
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
pub unsafe extern "C" fn art3m1s_probe_caption(
    resources: *mut crate::host_files::HostResources,
    ini_content: *const u8,
    ini_len: usize,
    platform: *const c_char,
    out_buf: *mut u8,
    out_cap: c_int,
) -> c_int {
    if resources.is_null()
        || ini_content.is_null()
        || platform.is_null()
        || out_buf.is_null()
        || out_cap <= 0
    {
        return 0;
    }
    let resources = unsafe { &*resources };
    let ini = unsafe { std::slice::from_raw_parts(ini_content, ini_len) };
    let Ok(plat) = (unsafe { std::ffi::CStr::from_ptr(platform).to_str() }) else {
        return 0;
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::probe_caption_from_bytes_with_resources(ini, plat, Some(resources))
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

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_feed_mouse(rt: *mut CoreRuntime, x: i32, y: i32) {
    if rt.is_null() {
        return;
    }
    let rt = unsafe { &*rt };
    rt.feed_mouse(x, y);
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_feed_click(rt: *mut CoreRuntime) {
    if rt.is_null() {
        return;
    }
    let rt = unsafe { &*rt };
    rt.feed_click();
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
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
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
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

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
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

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
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
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
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

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_destroy(rt: *mut CoreRuntime) {
    if !rt.is_null() {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drop(unsafe { Box::from_raw(rt) });
        }));
    }
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_stage_width(rt: *const CoreRuntime) -> u32 {
    if rt.is_null() {
        return 0;
    }
    unsafe { &*rt }.stage_width()
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_stage_height(rt: *const CoreRuntime) -> u32 {
    if rt.is_null() {
        return 0;
    }
    unsafe { &*rt }.stage_height()
}

/// Scene render target width. This may be lower than the native output width.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_render_width(rt: *const CoreRuntime) -> u32 {
    if rt.is_null() {
        return 0;
    }
    unsafe { &*rt }.render_dimensions().map_or_else(
        || unsafe { &*rt }.stage_width(),
        |dimensions| dimensions.render_size.width,
    )
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_render_height(rt: *const CoreRuntime) -> u32 {
    if rt.is_null() {
        return 0;
    }
    unsafe { &*rt }.render_dimensions().map_or_else(
        || unsafe { &*rt }.stage_height(),
        |dimensions| dimensions.render_size.height,
    )
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_output_width(rt: *const CoreRuntime) -> u32 {
    if rt.is_null() {
        return 0;
    }
    unsafe { &*rt }.render_dimensions().map_or_else(
        || unsafe { &*rt }.stage_width(),
        |dimensions| dimensions.output_size.width,
    )
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_output_height(rt: *const CoreRuntime) -> u32 {
    if rt.is_null() {
        return 0;
    }
    unsafe { &*rt }.render_dimensions().map_or_else(
        || unsafe { &*rt }.stage_height(),
        |dimensions| dimensions.output_size.height,
    )
}

/// Configures the current upscale pass. `mode=0` selects linear sampling;
/// `mode=1` requests a backend-native spatial upscaler when available.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_set_upscale_mode(
    rt: *mut CoreRuntime,
    mode: c_int,
    sharpness: f32,
) -> c_int {
    if rt.is_null() {
        return 0;
    }
    let mode = match mode {
        0 => crate::render_pipeline::post_process::UpscaleMode::Linear,
        1 => crate::render_pipeline::post_process::UpscaleMode::Spatial,
        _ => return 0,
    };
    let mut pipeline = crate::render_pipeline::post_process::PostProcessPipeline::default();
    pipeline.passes[0] = crate::render_pipeline::post_process::PostProcessPass::Upscale(
        crate::render_pipeline::post_process::UpscaleConfig { mode, sharpness },
    );
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        (&mut *rt).configure_post_process(pipeline)
    })) {
        Ok(Ok(())) => 1,
        Ok(Err(error)) => {
            core_warn!("post-process configuration rejected: {error}");
            0
        }
        Err(panic_info) => {
            core_error!(
                "post-process configuration panicked: {}",
                panic_msg(&panic_info)
            );
            0
        }
    }
}

/// Changes SceneColor scale relative to the native output. The value must be
/// finite and in `[0.1, 1.0]`; native backends clamp the resolved render size
/// to at least the logical game size.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_set_render_scale(
    rt: *mut CoreRuntime,
    scale: f32,
) -> c_int {
    if rt.is_null() {
        return 0;
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        (&mut *rt).set_render_scale(scale)
    })) {
        Ok(Ok(())) => 1,
        Ok(Err(error)) => {
            core_warn!("render scale configuration rejected: {error}");
            0
        }
        Err(panic_info) => {
            core_error!(
                "render scale configuration panicked: {}",
                panic_msg(&panic_info)
            );
            0
        }
    }
}

/// Configures a backend-native spatial pass with an explicit SceneColor scale
/// relative to the output surface. This combines mode and scale atomically so
/// one setting cannot reset the other. Returns 1 on success, otherwise 0.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_configure_spatial_upscale(
    rt: *mut CoreRuntime,
    render_scale: f32,
    sharpness: f32,
) -> c_int {
    if rt.is_null() {
        return 0;
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        (&mut *rt).configure_spatial_upscale(render_scale, sharpness)
    })) {
        Ok(Ok(())) => 1,
        Ok(Err(error)) => {
            core_warn!("spatial upscale configuration rejected: {error}");
            0
        }
        Err(panic_info) => {
            core_error!(
                "spatial upscale configuration panicked: {}",
                panic_msg(&panic_info)
            );
            0
        }
    }
}

/// 设置统一渲染质量：0 Native，1 Quality，2 Balanced，3 Performance。
/// MetalFX 不可用时返回成功并由 Metal backend 回退到 native。
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_set_render_quality_preset(
    rt: *mut CoreRuntime,
    preset: c_int,
) -> c_int {
    if rt.is_null() {
        return 0;
    }
    let Some(preset) = crate::render_pipeline::post_process::RenderQualityPreset::from_ffi(preset)
    else {
        return 0;
    };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        (&mut *rt).set_render_quality_preset(preset)
    })) {
        Ok(Ok(())) => 1,
        Ok(Err(error)) => {
            core_warn!("render quality configuration rejected: {error}");
            0
        }
        Err(panic_info) => {
            core_error!(
                "render quality configuration panicked: {}",
                panic_msg(&panic_info)
            );
            0
        }
    }
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_pixel_buffer_size(rt: *const CoreRuntime) -> u32 {
    if rt.is_null() {
        return 0;
    }
    unsafe { &*rt }.pixel_buffer_size() as u32
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
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

/// 推进一个引擎 tick，但不合成或回读像素。
///
/// 宿主上一帧仍在异步解码时调用，避免阻塞显示链导致 `onEnterFrame`
/// （包括 E-Mote 口型采样）漏帧。返回 1 表示成功，0 表示参数无效或 panic。
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_advance_without_render(
    rt: *mut CoreRuntime,
    delta_ms: u32,
) -> i32 {
    if rt.is_null() {
        return 0;
    }
    let rt = unsafe { &mut *rt };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.advance_without_render(delta_ms as u64);
    }));
    match result {
        Ok(()) => 1,
        Err(panic_info) => {
            core_error!(
                "art3m1s_runtime_advance_without_render panicked: {}",
                panic_msg(&panic_info)
            );
            0
        }
    }
}

/// Attaches a host platform texture to the runtime.
/// `kind`: 1 = Android ANativeWindow, 2 = Apple IOSurface,
/// 3 = Apple MTLTexture (legacy GL/ANGLE import), 4 = Apple CAMetalLayer.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_set_external_surface(
    rt: *mut CoreRuntime,
    kind: i32,
    handle: *mut c_void,
    width: u32,
    height: u32,
) -> i32 {
    if rt.is_null() || handle.is_null() || width == 0 || height == 0 {
        return 0;
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let surface =
            crate::backend::NativeSurface::from_legacy_parts(kind, handle, width, height)?;
        unsafe { &mut *rt }.set_native_surface(surface)
    }));
    match result {
        Ok(Ok(())) => 1,
        Ok(Err(error)) => {
            core_warn!("external surface unavailable: {error}");
            0
        }
        Err(panic_info) => {
            core_error!(
                "art3m1s_runtime_set_external_surface panicked: {}",
                panic_msg(&panic_info)
            );
            0
        }
    }
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_clear_external_surface(rt: *mut CoreRuntime) {
    if rt.is_null() {
        return;
    }
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        unsafe { &mut *rt }.clear_native_surface();
    }));
}

/// Advances and presents through the configured host texture.
/// Returns 1 for a newly presented frame, 0 for an unchanged frame, and -1 on error.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_advance_and_present(
    rt: *mut CoreRuntime,
    delta_ms: u32,
) -> i32 {
    if rt.is_null() {
        return -1;
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        unsafe { &mut *rt }.advance_and_present(delta_ms as u64)
    }));
    match result {
        Ok(Ok(changed)) => i32::from(changed),
        Ok(Err(error)) => {
            core_warn!("external surface present failed: {error}");
            -1
        }
        Err(panic_info) => {
            core_error!(
                "art3m1s_runtime_advance_and_present panicked: {}",
                panic_msg(&panic_info)
            );
            -1
        }
    }
}

/// Enables the per-runtime asynchronous profiler. The render thread only
/// records timestamps and performs a non-blocking queue send; aggregation is
/// performed by a dedicated worker.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_set_profiler_enabled(
    rt: *const CoreRuntime,
    enabled: c_int,
) {
    if !rt.is_null() {
        unsafe { &*rt }.set_profiler_enabled(enabled != 0);
    }
}

/// Copies the latest profiler snapshot as UTF-8 JSON. With a null/zero buffer,
/// returns the required byte count. A too-small buffer returns the negated
/// required count, so hosts can retry without imposing a fixed ABI struct.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_profiler_snapshot(
    rt: *const CoreRuntime,
    out: *mut u8,
    capacity: u32,
) -> i32 {
    if rt.is_null() {
        return -1;
    }
    let json = unsafe { &*rt }.profiler_snapshot_json();
    let required = i32::try_from(json.len()).unwrap_or(i32::MAX);
    if out.is_null() || capacity == 0 {
        return required;
    }
    if (capacity as usize) < json.len() {
        return -required;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(json.as_ptr(), out, json.len());
    }
    required
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
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

/// Selects the callback-free runtime video session for subsequent video tags.
///
/// When enabled, `video` events are decoded by the runtime and uploaded to
/// renderer-owned textures. Audio transport and final presentation remain
/// host-owned. The switch is explicit so hosts can migrate one media path at a
/// time without mixing decoder ownership.
#[cfg(all(
    feature = "ffmpeg",
    any(
        feature = "gl-backend",
        feature = "metal-backend",
        feature = "vulkan-backend"
    )
))]
pub unsafe extern "C" fn art3m1s_runtime_set_runtime_media_enabled_v1(
    rt: *mut CoreRuntime,
    enabled: c_int,
) {
    if rt.is_null() {
        return;
    }
    unsafe { &mut *rt }.set_runtime_media_enabled(enabled != 0);
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
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

/// libmpv OpenGL resolver callback. `ctx` must be the runtime pointer supplied
/// as `mpv_opengl_init_params.get_proc_address_ctx` by the host.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
/// Deprecated GL-only compatibility shim. Prefer `art3m1s_runtime_import_video_frame`
/// or `art3m1s_runtime_upload_video_layer_frame`. Metal returns NULL.
pub unsafe extern "C" fn art3m1s_runtime_video_gl_get_proc_address(
    ctx: *mut std::ffi::c_void,
    name: *const c_char,
) -> *mut std::ffi::c_void {
    if ctx.is_null() || name.is_null() {
        return std::ptr::null_mut();
    }
    let Some(name) = (unsafe { std::ffi::CStr::from_ptr(name).to_str().ok() }) else {
        return std::ptr::null_mut();
    };
    let rt = unsafe { &*(ctx.cast::<CoreRuntime>()) };
    rt.external_renderer_proc_address(name).cast_mut()
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
/// Deprecated GL-only lease. Metal/Vulkan return 0 so hosts fall back to import/RGBA.
pub unsafe extern "C" fn art3m1s_runtime_video_gl_begin(rt: *mut CoreRuntime) -> c_int {
    if rt.is_null() {
        return 0;
    }
    let rt = unsafe { &mut *rt };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rt.begin_external_render())) {
        Ok(Ok(())) => 1,
        Ok(Err(error)) => {
            core_warn!("video GL begin failed: {error}");
            0
        }
        Err(panic_info) => {
            core_error!("video GL begin panicked: {}", panic_msg(&panic_info));
            0
        }
    }
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
/// Deprecated: returns a GLuint FBO name on the GL backend, otherwise 0.
pub unsafe extern "C" fn art3m1s_runtime_video_gl_framebuffer(
    rt: *mut CoreRuntime,
    id: *const c_char,
    width: u32,
    height: u32,
) -> u32 {
    if rt.is_null() || id.is_null() {
        return 0;
    }
    let Some(id) = (unsafe { std::ffi::CStr::from_ptr(id).to_str().ok() }) else {
        return 0;
    };
    let rt = unsafe { &mut *rt };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.video_layer_external_render_target(id, width, height)
            .ok()
            .and_then(|handle| u32::try_from(handle).ok())
            .unwrap_or(0)
    }))
    .unwrap_or(0)
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_video_gl_commit(
    rt: *mut CoreRuntime,
    id: *const c_char,
) -> c_int {
    if rt.is_null() || id.is_null() {
        return 0;
    }
    let Some(id) = (unsafe { std::ffi::CStr::from_ptr(id).to_str().ok() }) else {
        return 0;
    };
    let rt = unsafe { &mut *rt };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.commit_video_layer_external_frame(id)
    }))
    .map_or(0, i32::from)
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_video_gl_end(rt: *mut CoreRuntime) {
    if rt.is_null() {
        return;
    }
    let rt = unsafe { &mut *rt };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.end_external_render();
    }));
}

/// Upload one RGBA8 frame for a currently playing video layer.
///
/// This call is synchronous. `rgba` is borrowed only for the duration of the
/// call and is passed directly to the active GPU backend without an intermediate
/// CPU-side copy.
/// The host must serialize this with other calls using the same runtime.
///
/// Returns 1 on success and 0 for invalid arguments, a stale layer, or failure.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
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

/// Preferred video import kind for the active backend.
/// 0=CPU RGBA, 1=CVPixelBuffer, 2=MTLTexture, 3=IOSurface,
/// 4=AHardwareBuffer, 5=OpenGL framebuffer (legacy).
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_video_import_kind(rt: *const CoreRuntime) -> i32 {
    if rt.is_null() {
        return 0;
    }
    unsafe { &*rt }.video_import_kind()
}

/// Import one decoded video frame as a compositor-sampled external texture.
///
/// `image_kind`: 0=CPU RGBA, 1=CVPixelBuffer, 2=MTLTexture, 3=IOSurface, 4=AHardwareBuffer.
/// `ownership`: 0=Borrowed, 1=Imported, 2=Owned.
/// `wait_kind`: 0=none, 1=MTLSharedEvent, 2=Vulkan semaphore. Reserved; 0 is the
/// host-synchronized path.
/// Returns 1 and writes an opaque `ExternalTextureHandle` when `out_texture` is
/// non-null. Failure leaves the previous frame bound.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_import_video_frame(
    rt: *mut CoreRuntime,
    id: *const c_char,
    image_kind: i32,
    native_handle: *mut c_void,
    width: u32,
    height: u32,
    ownership: i32,
    wait_kind: i32,
    wait_handle: *mut c_void,
    wait_value: u64,
    rgba: *const u8,
    rgba_len: usize,
    out_texture: *mut u64,
) -> c_int {
    if rt.is_null() || id.is_null() || width == 0 || height == 0 {
        return 0;
    }
    let Some(id) = (unsafe { std::ffi::CStr::from_ptr(id).to_str().ok() }) else {
        return 0;
    };
    if id.is_empty() {
        return 0;
    }
    let rgba = if rgba.is_null() {
        None
    } else {
        Some(unsafe { std::slice::from_raw_parts(rgba, rgba_len) })
    };
    let image = match crate::backend::ExternalImage::from_ffi_parts(
        image_kind,
        native_handle,
        width,
        height,
        ownership,
        wait_kind,
        wait_handle,
        wait_value,
        rgba,
    ) {
        Ok(image) => image,
        Err(error) => {
            core_warn!("import video frame rejected: {error}");
            return 0;
        }
    };
    let rt = unsafe { &mut *rt };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.import_video_layer_frame(id, image)
    })) {
        Ok(Ok(handle)) => {
            if !out_texture.is_null() {
                unsafe { *out_texture = handle.opaque() };
            }
            1
        }
        Ok(Err(error)) => {
            core_warn!("import video frame failed: {error}");
            0
        }
        Err(panic_info) => {
            core_error!(
                "art3m1s_runtime_import_video_frame panicked: {}",
                panic_msg(&panic_info)
            );
            0
        }
    }
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_release_video_frame(
    rt: *mut CoreRuntime,
    texture_handle: u64,
) -> c_int {
    if rt.is_null() || texture_handle == 0 {
        return 0;
    }
    let rt = unsafe { &mut *rt };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.release_video_layer_frame(crate::backend::ExternalTextureHandle::from_opaque(
            texture_handle,
        ))
    }))
    .map_or(0, |released| i32::from(released))
}

/// Returns 1 when the producer may recycle the native object for this handle.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_video_frame_consumed(
    rt: *mut CoreRuntime,
    surface_handle: u64,
) -> c_int {
    if rt.is_null() || surface_handle == 0 {
        return 1;
    }
    let rt = unsafe { &mut *rt };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.video_layer_frame_consumed(crate::backend::VideoSurfaceHandle::from_opaque(
            surface_handle,
        ))
    }))
    .map_or(0, |consumed| i32::from(consumed))
}

/// Acquire a core-owned writable video surface. The returned handle is opaque
/// and is not a GLuint. GL hosts that still need an FBO should keep using
/// `video_gl_framebuffer`.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_acquire_video_surface(
    rt: *mut CoreRuntime,
    id: *const c_char,
    width: u32,
    height: u32,
    out_surface: *mut u64,
) -> c_int {
    if rt.is_null() || id.is_null() || out_surface.is_null() || width == 0 || height == 0 {
        return 0;
    }
    let Some(id) = (unsafe { std::ffi::CStr::from_ptr(id).to_str().ok() }) else {
        return 0;
    };
    let rt = unsafe { &mut *rt };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.acquire_video_layer_surface(id, width, height)
    })) {
        Ok(Ok(handle)) => {
            unsafe { *out_surface = handle.opaque() };
            1
        }
        Ok(Err(error)) => {
            core_warn!("acquire video surface failed: {error}");
            0
        }
        Err(panic_info) => {
            core_error!(
                "art3m1s_runtime_acquire_video_surface panicked: {}",
                panic_msg(&panic_info)
            );
            0
        }
    }
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_commit_video_surface(
    rt: *mut CoreRuntime,
    surface_handle: u64,
) -> c_int {
    if rt.is_null() || surface_handle == 0 {
        return 0;
    }
    let rt = unsafe { &mut *rt };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.commit_video_layer_surface(crate::backend::VideoSurfaceHandle::from_opaque(
            surface_handle,
        ))
    }))
    .map_or(0, |ok| i32::from(ok))
}

/// Capture the current scene into an RGBA8 buffer without advancing the engine.
/// This is the screenshot path and does not use video framebuffer ABI.
/// Returns bytes written, or 0 on failure.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_capture_screenshot(
    rt: *mut CoreRuntime,
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
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.capture_screenshot_from_host(out_pixels)
    }))
    .unwrap_or(0) as u32
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
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

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_is_exit_requested(rt: *const CoreRuntime) -> i32 {
    if rt.is_null() {
        return 0;
    }
    let rt = unsafe { &*rt };
    if rt.is_exit_requested() { 1 } else { 0 }
}

/// 宿主生命周期通知：state 0=引擎退出前、1=切到后台、2=回到前台。
/// [autosave allow=1] 时核心在退出/切后台时自动保存。
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
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
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_notify_window_button(rt: *mut CoreRuntime, button: c_int) {
    if rt.is_null() {
        return;
    }
    let rt = unsafe { &mut *rt };
    rt.notify_window_button(button);
}

/// 宿主屏幕方向变化（setondirchg，仅 iOS）：
/// direction 0=纵向 / 1=横向Home右 / 2=倒置纵向 / 3=横向Home左。
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
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
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
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
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
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

/// 设置上报给脚本的机种串覆盖（`var system="os"` 的返回值），如
/// "switch"/"ps4"。NULL 或空串清除覆盖，回到项目平台。对运行中的
/// runtime 立即生效；移植版游戏把关键功能（如存档）开关在机种判断上时，
/// 宿主用它在桌面环境伪装目标机种。
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub unsafe extern "C" fn art3m1s_runtime_set_reported_os(rt: *mut CoreRuntime, os: *const c_char) {
    if rt.is_null() {
        return;
    }
    let reported = if os.is_null() {
        None
    } else {
        match unsafe { std::ffi::CStr::from_ptr(os) }.to_str() {
            Ok(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            _ => None,
        }
    };
    let rt = unsafe { &mut *rt };
    rt.set_reported_os(reported);
}

#[cfg(test)]
mod tests {
    use super::{
        font_override, log_suppressed_by_filter, script_debug_print_allowed, set_font_override,
        set_log_filter, set_script_debug_config,
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

    #[test]
    fn font_override_rejects_invalid_bytes_without_storing() {
        // 非法字体必须被拒绝且不进全局状态（校验先于存储，世代号不变）。
        let before = font_override().map(|(generation, _)| generation);
        assert!(set_font_override(vec![0, 1, 2, 3]).is_err());
        assert!(set_font_override(Vec::new()).is_err());
        let after = font_override().map(|(generation, _)| generation);
        assert_eq!(before, after);
    }
}
