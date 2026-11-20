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

#[cfg(any(feature = "rfvp-engine", feature = "krkr-engine"))]
pub(crate) mod handles;

#[cfg(feature = "rfvp-engine")]
pub mod rfvp_api;

#[cfg(feature = "krkr-engine")]
pub mod krkr_api;
#[cfg(feature = "krkr-engine")]
mod krkr_renderer;

#[cfg(feature = "siglus-engine")]
pub mod siglus_api;
#[cfg(feature = "siglus-engine")]
mod siglus_output;

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
use std::ffi::c_void;
use std::ffi::{c_char, c_int};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    if let Some(host) = crate::host::current_session() {
        host.set_profile_io_enabled(enabled);
        return;
    }
    PROFILE_IO.store(enabled, Ordering::Relaxed);
    if !enabled {
        let _ = take_profile_io_counters();
    }
}

pub(crate) fn take_profile_io_counters() -> HostFfiProfile {
    if let Some(host) = crate::host::current_session() {
        return host.take_profile_io_counters();
    }
    HOST_FFI_PROFILE.with(|cell| cell.replace(HostFfiProfile::default()))
}

fn begin_profile_io() -> Option<std::time::Instant> {
    let enabled = crate::host::current_session()
        .map(|host| host.profile_io_enabled())
        .unwrap_or_else(|| PROFILE_IO.load(Ordering::Relaxed));
    enabled.then(std::time::Instant::now)
}

fn finish_profile_io(started: Option<std::time::Instant>, bytes: usize) {
    let Some(started) = started else {
        return;
    };
    let elapsed_ns = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
    if let Some(host) = crate::host::current_session() {
        host.record_profile_io(elapsed_ns, bytes as u64);
        return;
    }
    HOST_FFI_PROFILE.with(|cell| {
        let mut value = cell.get();
        value.calls = value.calls.saturating_add(1);
        value.elapsed_ns = value.elapsed_ns.saturating_add(elapsed_ns);
        value.bytes = value.bytes.saturating_add(bytes as u64);
        cell.set(value);
    });
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

pub(crate) unsafe extern "C" fn art3m1s_set_debug(enabled: c_int) {
    let enabled = enabled != 0;
    DEBUG.store(enabled, Ordering::Relaxed);
    if !enabled {
        DAMAGE_VISUALIZATION.store(false, Ordering::Relaxed);
    }
}

pub fn debug_enabled() -> bool {
    if let Some(host) = crate::host::current_session() {
        return host.debug_enabled();
    }
    legacy_debug_enabled()
}

pub(crate) fn legacy_debug_enabled() -> bool {
    DEBUG.load(Ordering::Relaxed)
}

pub(crate) unsafe extern "C" fn art3m1s_set_damage_visualization(enabled: c_int) {
    DAMAGE_VISUALIZATION.store(enabled != 0 && legacy_debug_enabled(), Ordering::Relaxed);
}

pub fn damage_visualization_enabled() -> bool {
    legacy_debug_enabled() && DAMAGE_VISUALIZATION.load(Ordering::Relaxed)
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
    if let Some(host) = crate::host::current_session() {
        host.log(level, msg);
        return;
    }
    if log_suppressed_by_filter(level, msg) {
        return;
    }
    if let Some(events) = crate::host_events::legacy_active_events() {
        events.push_log(level, msg);
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
pub(crate) unsafe extern "C" fn art3m1s_set_font_override(data: *const u8, len: c_int) -> c_int {
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
pub(crate) unsafe extern "C" fn art3m1s_clear_font_override() {
    clear_font_override();
}

// ── ANGLE library search path ──────────────────────────────────

pub(crate) unsafe extern "C" fn art3m1s_set_angle_path(path: *const c_char) {
    if let Ok(s) = unsafe { std::ffi::CStr::from_ptr(path).to_str() } {
        let _ = crate::backend::set_angle_path_prefix(s);
    }
}

// ── Query helpers ────────────────────────────────────────────────

fn query_size(path: &str) -> Option<u64> {
    let started = begin_profile_io();
    let result = crate::host_files::default_resources()
        .query_size(path)
        .ok()
        .flatten();
    finish_profile_io(started, 0);
    result
}

fn read_chunk(path: &str, offset: u64, buf: &mut [u8]) -> Option<usize> {
    let started = begin_profile_io();
    let result = crate::host_files::default_resources()
        .read_range(path, offset, buf)
        .ok();
    finish_profile_io(started, result.unwrap_or(0));
    result
}

const CHUNK: usize = 65536;
const MAX_SINGLE: u64 = 16 * 1024 * 1024;

pub(crate) fn request_file(path: &str) -> Result<Vec<u8>, String> {
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

// ── Runtime control FFI ─────────────────────────────────────────

use crate::runtime::CoreRuntime;

/// Creates a runtime using the platform default GPU backend.
///
/// Value 0 selects the production platform default: Metal on Darwin and
/// experimental Vulkan on Android/Windows/Linux, with GL as initialization
/// fallback when it is built. Explicit native overrides are 2=Vulkan and
/// 3=Metal. Values 1/4/5/6 retain GL/ANGLE debug and A/B paths.
pub(crate) unsafe extern "C" fn art3m1s_runtime_create(
    w: u32,
    h: u32,
    backend: i32,
) -> *mut CoreRuntime {
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

pub(crate) unsafe extern "C" fn art3m1s_runtime_set_resources(
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

/// Returns the `BackendCapabilities` stable bit mask for the active backend.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub(crate) unsafe extern "C" fn art3m1s_runtime_backend_capabilities(
    rt: *const CoreRuntime,
) -> u64 {
    if rt.is_null() {
        return 0;
    }
    unsafe { &*rt }.backend_info().capabilities.bits()
}

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub(crate) unsafe extern "C" fn art3m1s_runtime_load_project(
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_load_project_bytes(
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

/// Headless 探测游戏 caption（导入时用）。
/// 只跑解释器到发出第一个 `[caption]` 即停，不建 GL/compositor，近乎瞬时。把 caption 的
/// UTF-8 写入 `out_buf`（≤`out_cap`），返回写入字节数；无 caption / 缓冲不足 / 出错返回 0。
/// 宿主须在调用前把文件供给（目录/pfs）指向该游戏，否则 boot 脚本读不到直接返回 0。
unsafe extern "C" fn art3m1s_probe_caption(
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
        crate::legacy_project::probe_caption_from_bytes_with_resources(ini, plat, resources)
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_feed_mouse(rt: *mut CoreRuntime, x: i32, y: i32) {
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_feed_click(rt: *mut CoreRuntime) {
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_feed_mouse_button(
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_feed_touch(
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_feed_key(
    rt: *mut CoreRuntime,
    vk: u32,
    pressed: i32,
) {
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_submit_dialog(
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_submit_text_translation(
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_destroy(rt: *mut CoreRuntime) {
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_stage_width(rt: *const CoreRuntime) -> u32 {
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_stage_height(rt: *const CoreRuntime) -> u32 {
    if rt.is_null() {
        return 0;
    }
    unsafe { &*rt }.stage_height()
}

/// Configures a backend-native spatial pass with an explicit SceneColor scale
/// relative to the output surface. This combines mode and scale atomically so
/// one setting cannot reset the other. Returns 1 on success, otherwise 0.
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub(crate) unsafe extern "C" fn art3m1s_runtime_configure_spatial_upscale(
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_set_render_quality_preset(
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_pixel_buffer_size(rt: *const CoreRuntime) -> u32 {
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_advance_and_render(
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_advance_without_render(
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_set_external_surface(
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_clear_external_surface(rt: *mut CoreRuntime) {
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_advance_and_present(
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_set_profiler_enabled(
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_profiler_snapshot(
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_set_volume(
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_set_runtime_media_enabled_v1(
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_notify_video_finished(
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_upload_video_layer_frame(
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

#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub(crate) unsafe extern "C" fn art3m1s_runtime_notify_sound_finished(
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_is_exit_requested(rt: *const CoreRuntime) -> i32 {
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_notify_lifecycle(
    rt: *mut CoreRuntime,
    state: c_int,
) {
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

/// 宿主回填 httpget/httppost 的结果：status_code 为 HTTP 响应码（失败传 0），
/// body 为响应体字节（可为 NULL）。返回 1 表示有挂起请求被完成。
#[cfg(any(
    feature = "gl-backend",
    feature = "metal-backend",
    feature = "vulkan-backend"
))]
pub(crate) unsafe extern "C" fn art3m1s_runtime_submit_http_result(
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_set_string_variable(
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
pub(crate) unsafe extern "C" fn art3m1s_runtime_set_reported_os(
    rt: *mut CoreRuntime,
    os: *const c_char,
) {
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
    #[test]
    fn io_counters_and_resets_belong_to_the_current_session() {
        let a = crate::host::HostSession::new();
        let b = crate::host::HostSession::new();
        let _scope = a.enter();
        super::set_profile_io_enabled(true);
        super::finish_profile_io(super::begin_profile_io(), 11);
        {
            let _scope = b.enter();
            super::set_profile_io_enabled(true);
            super::finish_profile_io(super::begin_profile_io(), 22);
            let b_sample = super::take_profile_io_counters();
            assert_eq!((b_sample.calls, b_sample.bytes), (1, 22));
            super::set_profile_io_enabled(false);
        }
        let a_sample = super::take_profile_io_counters();
        assert_eq!((a_sample.calls, a_sample.bytes), (1, 11));
        assert!(super::begin_profile_io().is_some());
    }

    use super::{font_override, log_suppressed_by_filter, set_font_override, set_log_filter};

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
