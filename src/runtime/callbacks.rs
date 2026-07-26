//! Runtime [`EngineCallbacks`] implementation backed by the FFI bridge.
//!
//! This module is runtime plumbing: it adapts script-engine callbacks to host
//! FFI services for input, file access, magic paths, and volume changes.

use asb_interpreter::lua_engine::{EmoteLayerCommand, EngineCallbacks};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use super::magic_path;
use crate::ffi;

pub(super) type LayerInfoTable =
    std::sync::Arc<std::sync::Mutex<HashMap<String, HashMap<String, String>>>>;

/// Engine callbacks that use the FFI bridge for all file access.
pub(super) struct FfiCallbacks {
    pub input: std::sync::Arc<std::sync::Mutex<InputSnapshot>>,
    pub magic_paths: std::sync::Arc<magic_path::MagicPathTable>,
    pub layer_info: LayerInfoTable,
    pub volumes: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, f32>>>,
    pub debug_skip_active: Arc<AtomicBool>,
    pub script_status: Arc<AtomicU8>,
    pub emote: super::emote::SharedEmoteState,
}

/// overrideKey 状态位集合（docs/lua/engine/overrideKey.txt）。
pub(super) const OVERRIDE_IS_PUSH: u32 = 2;
pub(super) const OVERRIDE_IS_DOWN: u32 = 4;
pub(super) const OVERRIDE_IS_DOWN_EDGE: u32 = 8;
pub(super) const OVERRIDE_IS_UP_EDGE: u32 = 16;
pub(super) const OVERRIDE_IS_DECIDE: u32 = 32;

/// isPush 的按键重复阈值：按下 0.5s 后转为持续 true（docs/lua/engine/isPush.txt）。
const PUSH_REPEAT_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// isDecide 的键盘确认键（docs/spec/key_id.md）：回车 13、空格 32。
/// 用于阅读剧情的确认键在 Windows 上是鼠标左键（键 1），键盘上的 Enter/Space
/// 同样触发确认（isDecide.txt：确认键随平台而异）。
pub(super) const DECIDE_ENTER_KEY: u32 = 13;
pub(super) const DECIDE_SPACE_KEY: u32 = 32;

/// 触摸阶段（art3m1s_runtime_feed_touch 的 phase 参数）。
pub(super) const TOUCH_PHASE_DOWN: u8 = 0;
pub(super) const TOUCH_PHASE_MOVE: u8 = 1;
pub(super) const TOUCH_PHASE_UP: u8 = 2;

/// 单个触摸点的当前状态：位置 + 最近一次阶段。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TouchPoint {
    pub x: i32,
    pub y: i32,
    pub phase: u8,
    /// 触摸开始（down）时的起点，供 flick 位移阈值判定。
    pub start_x: i32,
    pub start_y: i32,
}

/// 触摸控制态：多点上限 / 长按开关 / flick 阈值（setUseMultiTouch /
/// setUseTouchHold / setFlickSensitivity）。
#[derive(Clone, Copy, Debug)]
pub(super) struct TouchControl {
    /// 多点触控上限：None = 无限（setUseMultiTouch(-1)）。
    pub multi_touch_limit: Option<u32>,
    /// 长按是否启用（setUseTouchHold）。缺省启用。
    pub touch_hold_enabled: bool,
    /// flick 位移阈值（像素）：None = 禁用滑动（setFlickSensitivity(-1)）。
    pub flick_sensitivity: Option<f64>,
}

impl Default for TouchControl {
    fn default() -> Self {
        Self {
            multi_touch_limit: None,
            touch_hold_enabled: true,
            flick_sensitivity: None,
        }
    }
}

/// Minimal input state snapshot mirrored from the host event loop.
#[derive(Default)]
pub(super) struct InputSnapshot {
    pub mouse_x: i32,
    pub mouse_y: i32,
    pub clicked: bool,
    pub mouse_buttons_down: std::collections::HashSet<u32>,
    pub mouse_buttons_down_edge: std::collections::HashSet<u32>,
    pub mouse_buttons_up_edge: std::collections::HashSet<u32>,
    pub keys_down: std::collections::HashSet<u32>,
    pub keys_down_edge: std::collections::HashSet<u32>,
    pub keys_up_edge: std::collections::HashSet<u32>,
    /// 帧作用域按键覆盖：键 → 状态位集合（见 OVERRIDE_* 常量）。
    /// 位集合 0 = 该键所有查询强制为 false（点击无效化）。
    pub key_overrides: HashMap<u32, u32>,
    /// `e:overrideKey{status=...}` 省略 key 时的全键覆盖位集合。
    pub override_all_keys: Option<u32>,
    /// 每键按下时刻，isPush 的 0.5s 重复触发语义依赖它。
    /// 由宿主每帧经 [`note_frame_for_push`](Self::note_frame_for_push) 维护。
    pub keys_pressed_at: HashMap<u32, std::time::Instant>,
    /// 当前活跃触摸点：唯一 id → 位置/阶段。宿主经 feed_touch 维护，
    /// getTouchCount / getTouchPoint 从这里读真实数据。
    pub touches: HashMap<u32, TouchPoint>,
    /// 触摸控制态（多点上限 / 长按 / flick 阈值）。
    pub touch_control: TouchControl,
    /// 本帧检测到的 flick 位移向量（up 时按阈值判定后置入），供脚本查询/清帧。
    pub flick_edge: Option<(i32, i32)>,
}

impl InputSnapshot {
    pub fn clear_edges(&mut self) {
        self.clicked = false;
        self.mouse_buttons_down_edge.clear();
        self.mouse_buttons_up_edge.clear();
        self.keys_down_edge.clear();
        self.keys_up_edge.clear();
        self.key_overrides.clear();
        self.override_all_keys = None;
        // flick 是本帧边沿事件，帧末清除。已松开（up 阶段）的触摸点也在此移除，
        // 使 getTouchCount 只反映当前仍按住的手指。
        self.flick_edge = None;
        self.touches.retain(|_, t| t.phase != TOUCH_PHASE_UP);
    }

    /// 宿主投喂一次触摸事件。`phase`：0=down / 1=move / 2=up。
    ///
    /// - down：受 setUseMultiTouch 上限约束，超出上限的新手指被忽略（不入表）。
    /// - move：更新位置，保留 down 时记录的起点。
    /// - up：先按 flick 阈值判定滑动（位移超过 setFlickSensitivity 则置 flick_edge），
    ///   触摸点标记为 up，帧末由 [`clear_edges`](Self::clear_edges) 移除。
    pub fn feed_touch(&mut self, id: u32, phase: u8, x: i32, y: i32) {
        match phase {
            TOUCH_PHASE_DOWN => {
                // 已在表内的同 id（宿主重复 down）视为位置更新，不占新配额。
                if !self.touches.contains_key(&id) {
                    if let Some(limit) = self.touch_control.multi_touch_limit {
                        if self.touches.len() as u32 >= limit {
                            // 达到多点上限：忽略这根新手指。
                            return;
                        }
                    }
                }
                self.touches.insert(
                    id,
                    TouchPoint {
                        x,
                        y,
                        phase: TOUCH_PHASE_DOWN,
                        start_x: x,
                        start_y: y,
                    },
                );
            }
            TOUCH_PHASE_MOVE => {
                if let Some(point) = self.touches.get_mut(&id) {
                    point.x = x;
                    point.y = y;
                    point.phase = TOUCH_PHASE_MOVE;
                }
            }
            TOUCH_PHASE_UP => {
                if let Some(point) = self.touches.get_mut(&id) {
                    point.x = x;
                    point.y = y;
                    point.phase = TOUCH_PHASE_UP;
                    // flick 判定：move→up 的总位移超过灵敏度阈值即视为滑动。
                    if let Some(threshold) = self.touch_control.flick_sensitivity {
                        let dx = x - point.start_x;
                        let dy = y - point.start_y;
                        let dist = ((dx * dx + dy * dy) as f64).sqrt();
                        if dist >= threshold {
                            self.flick_edge = Some((dx, dy));
                        }
                    }
                }
            }
            // 未知阶段：忽略（协议只定义 0/1/2）。
            _ => {}
        }
    }

    /// getTouchCount：当前活跃触摸点数量（不含本帧已松开待清除的？—— up 点在
    /// 帧末才清，本帧内仍计入，与 getTouchPoint 的可枚举集合保持一致）。
    pub(super) fn touch_count(&self) -> u32 {
        self.touches.len() as u32
    }

    /// getTouchPoint(index)：按 id 升序取第 `index`（0 基）个触摸点的坐标。
    ///
    /// 解释器绑定用序号索引取位置（旧规范形态）；这里按 id 排序保证枚举稳定。
    /// 越界返回 (0, 0)。
    pub(super) fn touch_point(&self, index: u32) -> (i32, i32) {
        let mut ids: Vec<&u32> = self.touches.keys().collect();
        ids.sort_unstable();
        ids.get(index as usize)
            .and_then(|id| self.touches.get(id))
            .map(|p| (p.x, p.y))
            .unwrap_or((0, 0))
    }

    /// 每帧记录新按下按键的时间戳，并清理已松开的键（isPush 数据源）。
    pub fn note_frame_for_push(&mut self, now: std::time::Instant) {
        for key in &self.keys_down_edge {
            self.keys_pressed_at.entry(*key).or_insert(now);
        }
        let down = self.keys_down.clone();
        self.keys_pressed_at.retain(|key, _| down.contains(key));
    }

    /// 单键覆盖优先于全键覆盖。
    fn override_mask(&self, vk: u32) -> Option<u32> {
        self.key_overrides.get(&vk).copied().or(self.override_all_keys)
    }

    fn key_down(&self, vk: u32) -> bool {
        match self.override_mask(vk) {
            Some(mask) => mask & OVERRIDE_IS_DOWN != 0,
            None => self.keys_down.contains(&vk),
        }
    }

    fn key_down_edge(&self, vk: u32) -> bool {
        match self.override_mask(vk) {
            Some(mask) => mask & OVERRIDE_IS_DOWN_EDGE != 0,
            None => self.keys_down_edge.contains(&vk),
        }
    }

    fn key_up_edge(&self, vk: u32) -> bool {
        match self.override_mask(vk) {
            Some(mask) => mask & OVERRIDE_IS_UP_EDGE != 0,
            None => self.keys_up_edge.contains(&vk),
        }
    }

    /// isPush：按下瞬间 true → 0.5s 内 false → 0.5s 后持续 true。
    fn push(&self, vk: u32, now: std::time::Instant) -> bool {
        if let Some(mask) = self.override_mask(vk) {
            return mask & OVERRIDE_IS_PUSH != 0;
        }
        if self.keys_down_edge.contains(&vk) {
            return true;
        }
        if !self.keys_down.contains(&vk) {
            return false;
        }
        self.keys_pressed_at
            .get(&vk)
            .is_some_and(|pressed_at| now.duration_since(*pressed_at) >= PUSH_REPEAT_DELAY)
    }

    /// isDecide：键 1（鼠标左键 / 阅读剧情确认键）取宿主 click 事件，
    /// 键盘 Enter(13)/Space(32) 的按下边沿同样算确认（isDecide.txt：确认键随
    /// 平台而异，键盘上以回车/空格确认）；其余键取自身按下边沿。
    /// 覆盖存在时只看 isDecide 位——`overrideKey{key=1,status=0}` 应使点击无效。
    fn decide(&self, vk: u32) -> bool {
        if let Some(mask) = self.override_mask(vk) {
            return mask & OVERRIDE_IS_DECIDE != 0;
        }
        if vk == 1 {
            // 确认键：鼠标点击 或 键盘回车/空格边沿。
            self.clicked
                || self.keys_down_edge.contains(&DECIDE_ENTER_KEY)
                || self.keys_down_edge.contains(&DECIDE_SPACE_KEY)
        } else {
            self.keys_down_edge.contains(&vk)
        }
    }

    /// 是否存在脚本注入的「决定/按下边沿」覆盖（[stop] 唤醒的判定来源）。
    pub(super) fn scripted_down_edge(&self) -> bool {
        let edge_bits = OVERRIDE_IS_DOWN_EDGE | OVERRIDE_IS_DECIDE;
        self.key_overrides.values().any(|mask| mask & edge_bits != 0)
            || self
                .override_all_keys
                .is_some_and(|mask| mask & edge_bits != 0)
    }
}

impl EngineCallbacks for FfiCallbacks {
    fn debug(&self, _level: i32, data: &str, _raw: bool) {
        crate::core_info!("{data}");
    }

    fn enqueue_tag(&self, _tag: String, _params: HashMap<String, String>) {}
    fn set_event_handler(&self, _handlers: HashMap<String, String>) {}

    fn get_script_status(&self) -> u8 {
        self.script_status.load(Ordering::SeqCst)
    }

    fn is_key_down(&self, key_id: u32) -> bool {
        self.input.lock().unwrap().key_down(key_id)
    }

    fn is_key_down_edge(&self, key_id: u32) -> bool {
        self.input.lock().unwrap().key_down_edge(key_id)
    }

    fn is_key_up_edge(&self, key_id: u32) -> bool {
        self.input.lock().unwrap().key_up_edge(key_id)
    }

    fn is_decide(&self) -> bool {
        self.is_decide_key(1)
    }

    fn is_decide_key(&self, key_id: u32) -> bool {
        self.input.lock().unwrap().decide(key_id)
    }

    fn is_push(&self, key_id: u32) -> bool {
        self.input
            .lock()
            .unwrap()
            .push(key_id, std::time::Instant::now())
    }

    fn get_mouse_point(&self) -> (i32, i32) {
        let s = self.input.lock().unwrap();
        (s.mouse_x, s.mouse_y)
    }

    fn get_touch_count(&self) -> u32 {
        self.input.lock().unwrap().touch_count()
    }

    fn get_touch_point(&self, index: u32) -> (i32, i32) {
        self.input.lock().unwrap().touch_point(index)
    }

    fn is_file_exists(&self, path: &str) -> bool {
        let resolved = magic_path::resolve_path(&self.magic_paths, path);
        ffi::query_asset_size(&resolved).is_some()
    }

    fn file_write(&self, path: &str, data: &[u8]) -> asb_interpreter::Result<()> {
        let resolved = magic_path::resolve_path(&self.magic_paths, path);
        ffi::request_write(&resolved, data)
            .map_err(|m| asb_interpreter::Error::IoError(std::io::Error::other(m)))
    }

    fn file_operation(&self, command: &str, params: HashMap<String, String>) {
        let _ = (command, params);
    }

    fn include(&self, _path: &str) {}

    fn override_key(&self, from: u32, to: u32) {
        // 旧签名回退路径：等价于显式指定 key + status。
        self.override_key_status(Some(from), Some(to));
    }

    fn override_key_status(&self, key: Option<u32>, status: Option<u32>) {
        let mut s = self.input.lock().unwrap();
        match (key, status) {
            // 指定键 + 位集合（含显式 0 = 该键所有查询无效化）。
            (Some(key), Some(mask)) => {
                s.key_overrides.insert(key, mask);
            }
            // status 省略 = 取消该键的覆盖。
            (Some(key), None) => {
                s.key_overrides.remove(&key);
            }
            // key 省略 = 覆盖所有键。
            (None, Some(mask)) => {
                s.override_all_keys = Some(mask);
            }
            // 两者都省略 = 取消全部覆盖。
            (None, None) => {
                s.key_overrides.clear();
                s.override_all_keys = None;
            }
        }
    }

    fn set_flick_sensitivity(&self, sensitivity: f64) {
        // setFlickSensitivity.txt：-1 禁用滑动；否则为触发滑动的像素位移阈值。
        let mut s = self.input.lock().unwrap();
        s.touch_control.flick_sensitivity = if sensitivity < 0.0 {
            None
        } else {
            Some(sensitivity)
        };
    }

    fn set_use_multi_touch(&self, mode: i64) {
        // setUseMultiTouch.txt：-1 无限；否则处理至多 mode 个触摸点。
        let mut s = self.input.lock().unwrap();
        s.touch_control.multi_touch_limit = if mode < 0 {
            None
        } else {
            Some(mode as u32)
        };
    }

    fn set_use_touch_hold(&self, enabled: bool) {
        // setUseTouchHold.txt：长按开关（自动模式下脚本可能每帧改写）。
        self.input.lock().unwrap().touch_control.touch_hold_enabled = enabled;
    }

    fn get_script_block(&self) -> HashMap<String, String> {
        HashMap::new()
    }

    fn get_script_stack(&self) -> Vec<HashMap<String, String>> {
        vec![]
    }

    fn get_script_wait_reason(&self) -> u8 {
        0
    }

    fn get_layer_info(&self, id: &str) -> Option<HashMap<String, String>> {
        self.layer_info.lock().unwrap().get(id).cloned()
    }

    fn get_layer_info_all(&self) -> Vec<(String, HashMap<String, String>)> {
        // get_layer_info.md：省略 id 的全图层枚举按 id 升序返回。
        let table = self.layer_info.lock().unwrap();
        let mut all: Vec<(String, HashMap<String, String>)> = table
            .iter()
            .map(|(id, info)| (id.clone(), info.clone()))
            .collect();
        all.sort_by(|a, b| a.0.cmp(&b.0));
        all
    }

    fn get_font_list(&self, monospace: bool, vertical: bool) -> Vec<String> {
        // 宿主（Flutter）经 art3m1s_register_font_query 回答可用字体族；
        // 未注册时返回空列表（脚本读到"无可选字体"，退化安全）。
        ffi::query_font_list(monospace, vertical)
    }

    fn get_window_state(&self) -> (bool, bool) {
        // (全屏, 最小化)。宿主经 art3m1s_register_window_state_query 回答。
        ffi::query_window_state()
    }

    // ── surface 绑定族（bindSurface / unbindSurface / …）───────────────
    //
    // 语义：按路径的引用计数内存缓存（bindSurface.txt：同一路径多次 bind
    // 需相同次数 unbind 才释放）。这里把文件字节预取进程内缓存；GPU 纹理
    // 仍由 TextureProvider 按需上传（provider 不在本层可达范围）。
    // 无独立加载线程：bindSurfaceAsync 同步完成，加载队列视为即刻清空，
    // 因此 isLoadingSurface 恒 false、clearSurfaceLoadQueue 无需等待，自洽。

    fn bind_surface(&self, key: &str) {
        let resolved = magic_path::resolve_path(&self.magic_paths, key);
        let mut cache = surface_cache().lock().unwrap();
        surface_cache_bind(&mut cache, &resolved, || ffi::request_asset(&resolved));
    }

    fn bind_surface_async(&self, key: &str) {
        self.bind_surface(key);
    }

    fn unbind_surface(&self, key: &str) {
        let resolved = magic_path::resolve_path(&self.magic_paths, key);
        let mut cache = surface_cache().lock().unwrap();
        surface_cache_unbind(&mut cache, &resolved);
    }

    fn clear_surface_load_queue(&self) {
        // 同步加载模型下队列恒空，无待清任务。
    }

    fn is_loading_surface(&self) -> bool {
        false
    }

    fn set_script_status(&self, status: u8) {
        self.script_status.store(status, Ordering::SeqCst);
        if status == 0 {
            self.debug_skip_active.store(false, Ordering::SeqCst);
        }
    }

    fn set_magic_path(&self, name: &str, path: &str) {
        let mut table = self.magic_paths.lock().unwrap();
        if path.is_empty() {
            // 文档（setMagicPath.txt）：路径为空字符串时解除分配。若照常插表，
            // resolve 会得到 "/tail" 而非未注册时的 image/ 回退。
            table.remove(name);
        } else {
            table.insert(name.to_string(), path.to_string());
        }
    }

    fn debug_skip(&self, index: i64) {
        if index > 0 {
            self.debug_skip_active.store(true, Ordering::SeqCst);
            self.script_status.store(4, Ordering::SeqCst);
        }
    }
    fn set_master_volume(&self, volume: f32) {
        self.volumes
            .lock()
            .unwrap()
            .insert("master".to_string(), volume);
    }
    fn set_bgm_volume(&self, volume: f32) {
        self.volumes
            .lock()
            .unwrap()
            .insert("bgm".to_string(), volume);
    }
    fn set_se_volume(&self, volume: f32) {
        self.volumes
            .lock()
            .unwrap()
            .insert("se".to_string(), volume);
    }
    fn set_voice_volume(&self, volume: f32) {
        self.volumes
            .lock()
            .unwrap()
            .insert("voice".to_string(), volume);
    }

    fn load_png_comments(&self, path: &str) -> Option<HashMap<String, String>> {
        let resolved = magic_path::resolve_path(&self.magic_paths, path);
        let bytes = ffi::request_asset(&resolved)?;
        let comments = parse_png_text_chunks(&bytes);
        if comments.is_empty() {
            None
        } else {
            Some(comments)
        }
    }

    fn create_emote_layer(
        &self,
        id: &str,
        files: &[String],
        width: u32,
        height: u32,
    ) -> asb_interpreter::Result<bool> {
        let mut loaded = Vec::with_capacity(files.len());
        for file in files {
            let resolved = magic_path::resolve_path(&self.magic_paths, file);
            let bytes = ffi::request_file(&resolved).map_err(|message| {
                asb_interpreter::Error::IoError(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    message,
                ))
            })?;
            loaded.push((resolved, bytes));
        }
        self.emote
            .lock()
            .unwrap()
            .create_layer(id, loaded, width, height)
            .map_err(|message| asb_interpreter::Error::RuntimeError { line: 0, message })
    }

    fn get_emote_layer(&self, id: &str, next: bool) -> Option<bool> {
        self.emote.lock().unwrap().get_layer(id, next)
    }

    fn command_emote_layer(
        &self,
        id: &str,
        next: bool,
        command: EmoteLayerCommand,
    ) -> asb_interpreter::Result<()> {
        self.emote
            .lock()
            .unwrap()
            .command(id, next, command)
            .map_err(|message| asb_interpreter::Error::RuntimeError { line: 0, message })
    }
}

// ── surface 绑定缓存 ────────────────────────────────────────────────
//
// FfiCallbacks 的字段集合被 project.rs 的构造点固定，故缓存表用进程级
// 静态量持有（单运行时进程模型下等价于运行时字段）。

/// 单条 surface 绑定：引用计数 + 预取的文件字节（读取失败时为 None，
/// 但计数语义照常生效）。
pub(super) struct SurfaceEntry {
    pub refs: usize,
    /// 预取的文件字节。当前只做内存驻留（bindSurface 的引用计数语义已生效）；
    /// GPU 侧预上传需 TextureProvider 配合，接线后此字段即被消费。
    #[allow(dead_code)]
    pub bytes: Option<Vec<u8>>,
}

type SurfaceCache = HashMap<String, SurfaceEntry>;

fn surface_cache() -> &'static std::sync::Mutex<SurfaceCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<SurfaceCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// bind：引用计数 +1；首次绑定时经 loader 预取字节。返回新的计数。
pub(super) fn surface_cache_bind(
    cache: &mut SurfaceCache,
    path: &str,
    loader: impl FnOnce() -> Option<Vec<u8>>,
) -> usize {
    if let Some(entry) = cache.get_mut(path) {
        entry.refs += 1;
        return entry.refs;
    }
    cache.insert(
        path.to_string(),
        SurfaceEntry {
            refs: 1,
            bytes: loader(),
        },
    );
    1
}

/// unbind：引用计数 -1，归零时移除缓存。返回剩余计数（未绑定时 None）。
pub(super) fn surface_cache_unbind(cache: &mut SurfaceCache, path: &str) -> Option<usize> {
    let entry = cache.get_mut(path)?;
    entry.refs = entry.refs.saturating_sub(1);
    if entry.refs == 0 {
        cache.remove(path);
        Some(0)
    } else {
        Some(entry.refs)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DECIDE_ENTER_KEY, DECIDE_SPACE_KEY, InputSnapshot, OVERRIDE_IS_DECIDE, OVERRIDE_IS_DOWN,
        OVERRIDE_IS_DOWN_EDGE, OVERRIDE_IS_PUSH, OVERRIDE_IS_UP_EDGE, TOUCH_PHASE_DOWN,
        TOUCH_PHASE_MOVE, TOUCH_PHASE_UP, surface_cache_bind, surface_cache_unbind,
    };
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    #[test]
    fn key_override_zero_masks_all_queries_for_one_frame() {
        let mut input = InputSnapshot::default();
        input.keys_down.insert(1);
        input.keys_down_edge.insert(1);
        input.clicked = true;
        assert!(input.key_down(1));
        assert!(input.decide(1));

        // e:overrideKey{key=1, status=0}：该键所有查询无效化。
        input.key_overrides.insert(1, 0);
        assert!(!input.key_down(1));
        assert!(!input.key_down_edge(1));
        assert!(!input.decide(1));
        assert!(!input.push(1, Instant::now()));

        // 帧末清除后恢复物理状态。
        input.clear_edges();
        assert!(input.key_down(1));
    }

    #[test]
    fn key_override_bits_map_to_individual_queries() {
        let mut input = InputSnapshot::default();
        input.key_overrides.insert(32, OVERRIDE_IS_DOWN);
        assert!(input.key_down(32));
        assert!(!input.key_down_edge(32));
        assert!(!input.key_up_edge(32));
        assert!(!input.decide(32));
        assert!(!input.push(32, Instant::now()));

        // 组合位（如 36 = isDown|isDecide）逐位生效。
        input
            .key_overrides
            .insert(32, OVERRIDE_IS_DOWN | OVERRIDE_IS_DECIDE);
        assert!(input.key_down(32));
        assert!(input.decide(32));

        input.key_overrides.insert(
            32,
            OVERRIDE_IS_PUSH | OVERRIDE_IS_DOWN_EDGE | OVERRIDE_IS_UP_EDGE,
        );
        assert!(input.push(32, Instant::now()));
        assert!(input.key_down_edge(32));
        assert!(input.key_up_edge(32));
    }

    #[test]
    fn omitted_key_override_applies_to_all_keys() {
        let mut input = InputSnapshot::default();
        input.keys_down.insert(13);
        input.clicked = true;

        // e:overrideKey{status=0}：全键无效化。
        input.override_all_keys = Some(0);
        assert!(!input.key_down(13));
        assert!(!input.decide(1));

        // 单键覆盖优先于全键覆盖。
        input.key_overrides.insert(13, OVERRIDE_IS_DOWN);
        assert!(input.key_down(13));
    }

    #[test]
    fn key_override_status_32_creates_a_scripted_edge() {
        let mut input = InputSnapshot::default();
        input.key_overrides.insert(124, OVERRIDE_IS_DECIDE);
        assert!(input.scripted_down_edge());

        let mut input = InputSnapshot::default();
        input.override_all_keys = Some(OVERRIDE_IS_DECIDE);
        assert!(input.scripted_down_edge());

        // 仅 isDown 位不构成脚本决定边沿。
        let mut input = InputSnapshot::default();
        input.key_overrides.insert(124, OVERRIDE_IS_DOWN);
        assert!(!input.scripted_down_edge());
    }

    #[test]
    fn push_repeats_after_half_a_second_of_holding() {
        let t0 = Instant::now();
        let mut input = InputSnapshot::default();

        // 按下瞬间：edge 存在 → true。
        input.keys_down.insert(7);
        input.keys_down_edge.insert(7);
        input.note_frame_for_push(t0);
        assert!(input.push(7, t0));

        // 0.5s 内（edge 已清）→ false。
        input.keys_down_edge.clear();
        assert!(!input.push(7, t0 + Duration::from_millis(200)));

        // 0.5s 后持续 true。
        assert!(input.push(7, t0 + Duration::from_millis(500)));
        assert!(input.push(7, t0 + Duration::from_millis(900)));

        // 松开后 false，且时间戳被清理。
        input.keys_down.remove(&7);
        input.note_frame_for_push(t0 + Duration::from_millis(1000));
        assert!(!input.push(7, t0 + Duration::from_millis(1000)));
        assert!(!input.keys_pressed_at.contains_key(&7));
    }

    #[test]
    fn decide_uses_click_for_mouse_and_edge_for_keys() {
        let mut input = InputSnapshot::default();
        input.clicked = true;
        input.keys_down_edge.insert(13);
        assert!(input.decide(1));
        assert!(input.decide(13));
        // 键盘回车按下边沿存在时，键 32（空格）自身无边沿仍非确认。
        assert!(!input.decide(99));
    }

    #[test]
    fn decide_key1_also_triggers_on_keyboard_enter_or_space() {
        // 确认键（键 1）：无鼠标点击、无键盘边沿 → false。
        let mut input = InputSnapshot::default();
        assert!(!input.decide(1));

        // 键盘回车(13)按下边沿 → 确认。
        input.keys_down_edge.insert(DECIDE_ENTER_KEY);
        assert!(input.decide(1));

        // 键盘空格(32)按下边沿 → 确认。
        let mut input = InputSnapshot::default();
        input.keys_down_edge.insert(DECIDE_SPACE_KEY);
        assert!(input.decide(1));

        // 鼠标点击 → 确认。
        let mut input = InputSnapshot::default();
        input.clicked = true;
        assert!(input.decide(1));

        // overrideKey{key=1,status=0} 覆盖仍使确认无效（回车边沿也被压掉）。
        let mut input = InputSnapshot::default();
        input.keys_down_edge.insert(DECIDE_ENTER_KEY);
        input.key_overrides.insert(1, 0);
        assert!(!input.decide(1));
    }

    #[test]
    fn touch_feed_tracks_points_and_counts() {
        let mut input = InputSnapshot::default();
        assert_eq!(input.touch_count(), 0);
        assert_eq!(input.touch_point(0), (0, 0));

        // 两根手指按下，按 id 升序枚举。
        input.feed_touch(10, TOUCH_PHASE_DOWN, 100, 200);
        input.feed_touch(3, TOUCH_PHASE_DOWN, 40, 50);
        assert_eq!(input.touch_count(), 2);
        assert_eq!(input.touch_point(0), (40, 50)); // id=3
        assert_eq!(input.touch_point(1), (100, 200)); // id=10
        assert_eq!(input.touch_point(2), (0, 0)); // 越界

        // move 更新位置。
        input.feed_touch(3, TOUCH_PHASE_MOVE, 45, 55);
        assert_eq!(input.touch_point(0), (45, 55));

        // up 标记后本帧仍可枚举，帧末 clear_edges 移除。
        input.feed_touch(3, TOUCH_PHASE_UP, 46, 56);
        assert_eq!(input.touch_count(), 2);
        input.clear_edges();
        assert_eq!(input.touch_count(), 1);
        assert_eq!(input.touch_point(0), (100, 200)); // 仅剩 id=10
    }

    #[test]
    fn multi_touch_limit_truncates_extra_fingers() {
        let mut input = InputSnapshot::default();
        // setUseMultiTouch(2)：只处理 2 根手指。
        input.touch_control.multi_touch_limit = Some(2);
        input.feed_touch(1, TOUCH_PHASE_DOWN, 0, 0);
        input.feed_touch(2, TOUCH_PHASE_DOWN, 1, 1);
        input.feed_touch(3, TOUCH_PHASE_DOWN, 2, 2); // 超上限，忽略
        assert_eq!(input.touch_count(), 2);
        assert_eq!(input.touch_point(2), (0, 0)); // 第三根未入表

        // 同 id 重复 down 不占新配额（视为位置更新）。
        input.feed_touch(1, TOUCH_PHASE_DOWN, 9, 9);
        assert_eq!(input.touch_count(), 2);
        assert_eq!(input.touch_point(0), (9, 9));

        // 无限模式（-1 → None）不截断。
        let mut input = InputSnapshot::default();
        input.touch_control.multi_touch_limit = None;
        for id in 0..5 {
            input.feed_touch(id, TOUCH_PHASE_DOWN, 0, 0);
        }
        assert_eq!(input.touch_count(), 5);
    }

    #[test]
    fn flick_detected_when_up_displacement_exceeds_threshold() {
        let mut input = InputSnapshot::default();
        input.touch_control.flick_sensitivity = Some(30.0);

        // 位移不足阈值 → 无 flick。
        input.feed_touch(1, TOUCH_PHASE_DOWN, 0, 0);
        input.feed_touch(1, TOUCH_PHASE_MOVE, 10, 10);
        input.feed_touch(1, TOUCH_PHASE_UP, 10, 10); // 距离约 14.1 < 30
        assert_eq!(input.flick_edge, None);
        input.clear_edges();

        // 位移超过阈值 → flick，向量为总位移。
        input.feed_touch(2, TOUCH_PHASE_DOWN, 0, 0);
        input.feed_touch(2, TOUCH_PHASE_MOVE, 40, 0);
        input.feed_touch(2, TOUCH_PHASE_UP, 40, 0); // 距离 40 >= 30
        assert_eq!(input.flick_edge, Some((40, 0)));
        // flick 是本帧边沿，帧末清除。
        input.clear_edges();
        assert_eq!(input.flick_edge, None);

        // 禁用滑动（-1 → None）时永不触发 flick。
        let mut input = InputSnapshot::default();
        input.touch_control.flick_sensitivity = None;
        input.feed_touch(1, TOUCH_PHASE_DOWN, 0, 0);
        input.feed_touch(1, TOUCH_PHASE_UP, 999, 999);
        assert_eq!(input.flick_edge, None);
    }

    #[test]
    fn surface_cache_refcount_semantics() {
        let mut cache = HashMap::new();

        // 同一路径 bind 两次只加载一次，需要 unbind 两次才真正释放。
        assert_eq!(surface_cache_bind(&mut cache, "image/a", || Some(vec![1])), 1);
        assert_eq!(
            surface_cache_bind(&mut cache, "image/a", || panic!("不应重复加载")),
            2
        );
        assert_eq!(cache["image/a"].bytes.as_deref(), Some(&[1u8][..]));

        assert_eq!(surface_cache_unbind(&mut cache, "image/a"), Some(1));
        assert!(cache.contains_key("image/a"));
        assert_eq!(surface_cache_unbind(&mut cache, "image/a"), Some(0));
        assert!(!cache.contains_key("image/a"));

        // 未绑定路径的 unbind 是空操作。
        assert_eq!(surface_cache_unbind(&mut cache, "image/a"), None);
    }

    #[test]
    fn set_magic_path_empty_string_unbinds_the_prefix() {
        use super::FfiCallbacks;
        use crate::runtime::magic_path;
        use asb_interpreter::lua_engine::EngineCallbacks;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU8};

        let magic_paths: Arc<magic_path::MagicPathTable> =
            Arc::new(std::sync::Mutex::new(HashMap::new()));
        let callbacks = FfiCallbacks {
            input: Arc::new(std::sync::Mutex::new(InputSnapshot::default())),
            magic_paths: Arc::clone(&magic_paths),
            layer_info: Arc::new(std::sync::Mutex::new(HashMap::new())),
            volumes: Arc::new(std::sync::Mutex::new(HashMap::new())),
            debug_skip_active: Arc::new(AtomicBool::new(false)),
            script_status: Arc::new(AtomicU8::new(0)),
            emote: Arc::new(std::sync::Mutex::new(
                crate::runtime::emote::EmoteState::default(),
            )),
        };

        callbacks.set_magic_path("bg", "background");
        assert_eq!(
            magic_path::resolve_path(&magic_paths, ":bg/title"),
            "background/title"
        );

        // setMagicPath.txt：路径空串 = 解除分配，解析回退到 image/ 约定。
        callbacks.set_magic_path("bg", "");
        assert_eq!(
            magic_path::resolve_path(&magic_paths, ":bg/title"),
            "image/bg/title"
        );
    }

    /// 构造一个仅关注 input 快照的 FfiCallbacks（其余字段用空占位）。
    fn callbacks_with_input(
        input: std::sync::Arc<std::sync::Mutex<InputSnapshot>>,
    ) -> super::FfiCallbacks {
        use super::FfiCallbacks;
        use crate::runtime::magic_path;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU8};

        FfiCallbacks {
            input,
            magic_paths: Arc::new(std::sync::Mutex::new(HashMap::new()))
                as Arc<magic_path::MagicPathTable>,
            layer_info: Arc::new(std::sync::Mutex::new(HashMap::new())),
            volumes: Arc::new(std::sync::Mutex::new(HashMap::new())),
            debug_skip_active: Arc::new(AtomicBool::new(false)),
            script_status: Arc::new(AtomicU8::new(0)),
            emote: Arc::new(std::sync::Mutex::new(
                crate::runtime::emote::EmoteState::default(),
            )),
        }
    }

    #[test]
    fn touch_callbacks_read_snapshot_and_controls_write_snapshot() {
        use asb_interpreter::lua_engine::EngineCallbacks;
        use std::sync::Arc;

        let input = Arc::new(std::sync::Mutex::new(InputSnapshot::default()));
        let callbacks = callbacks_with_input(Arc::clone(&input));

        // 控制态：setUseMultiTouch / setUseTouchHold / setFlickSensitivity 落进快照。
        callbacks.set_use_multi_touch(1);
        callbacks.set_use_touch_hold(false);
        callbacks.set_flick_sensitivity(25.0);
        {
            let s = input.lock().unwrap();
            assert_eq!(s.touch_control.multi_touch_limit, Some(1));
            assert!(!s.touch_control.touch_hold_enabled);
            assert_eq!(s.touch_control.flick_sensitivity, Some(25.0));
        }
        // -1：无限多点 / 禁用滑动。
        callbacks.set_use_multi_touch(-1);
        callbacks.set_flick_sensitivity(-1.0);
        {
            let s = input.lock().unwrap();
            assert_eq!(s.touch_control.multi_touch_limit, None);
            assert_eq!(s.touch_control.flick_sensitivity, None);
        }

        // getTouchCount / getTouchPoint 读快照真实触摸数据（受上限 1 约束前先设无限）。
        input
            .lock()
            .unwrap()
            .feed_touch(7, TOUCH_PHASE_DOWN, 320, 240);
        assert_eq!(callbacks.get_touch_count(), 1);
        assert_eq!(callbacks.get_touch_point(0), (320, 240));
        assert_eq!(callbacks.get_touch_point(1), (0, 0));
    }
}

/// Parse PNG tEXt chunks into `keyword -> text` map.
fn parse_png_text_chunks(bytes: &[u8]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    const SIG: usize = 8;
    if bytes.len() < SIG || &bytes[..SIG] != b"\x89PNG\r\n\x1a\n" {
        return out;
    }
    let mut i = SIG;
    while i + 8 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]) as usize;
        let typ = &bytes[i + 4..i + 8];
        let data_start = i + 8;
        let data_end = data_start + len;
        if data_end > bytes.len() {
            break;
        }
        if typ == b"tEXt" {
            let data = &bytes[data_start..data_end];
            if let Some(nul) = data.iter().position(|&b| b == 0) {
                let keyword: String = data[..nul].iter().map(|&b| b as char).collect();
                let text: String = data[nul + 1..].iter().map(|&b| b as char).collect();
                out.insert(keyword, text);
            }
        }
        if typ == b"IEND" {
            break;
        }
        i = data_end + 4;
    }
    out
}
