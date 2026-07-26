use super::CoreRuntime;
use super::input::enqueue_handler_tags;
use asb_interpreter::Value;
use std::collections::{HashMap, HashSet};

const AUTO_ADVANCE_DELAY_MS: u64 = 900;

/// [rclick] 缺省的右键单击脚本（文档 rclick.md）。
pub(super) const DEFAULT_RCLICK_SCRIPT: &str = "rclick.iet";

/// 右键脚本内再次右键时跳转的标签（文档 rclick.md）。
const RCLICK_LEAVE_LABEL: &str = "leave";

/// 挂起中的 HTTP 请求（httpget/httppost 的阻塞等待协议）。
///
/// 标签发出请求后脚本停在原地，宿主经
/// `art3m1s_runtime_submit_http_result` 回填结果后继续。
#[derive(Debug, Clone)]
pub(super) struct PendingHttp {
    /// 存储响应码的变量名
    pub varname_code: Option<String>,
    /// 存储响应体的变量名（filename 指定时忽略）
    pub varname_data: Option<String>,
    /// 结果落盘的文件路径（存档目录相对路径）
    pub filename: Option<String>,
    /// 请求序号（与宿主回执对账，防串号）
    pub serial: u64,
}

#[derive(Debug, Clone)]
pub(super) struct RuntimeControlState {
    skip_allowed: bool,
    skip_unread: bool,
    skip_active: bool,
    automode_allowed: bool,
    automode_active: bool,
    automode_layer: Option<String>,
    /// 左键单击是否停止自动模式（默认 true）
    automode_stop_by_click: bool,
    /// [stop] 标签是否停止自动模式（默认 true）
    automode_stop_by_stop: bool,
    /// [automode syncse=]：自动前进前需等播放结束的 SE/语音 ID 列表。
    /// 空=用"任意语音在播"的通用门控。
    automode_sync_se: Vec<String>,
    /// [alreadyread mode]：是否进行已读/未读判定（默认 true）。
    /// 关闭时已读跳过遇未读剧情不停止。
    already_read_enabled: bool,
    /// 已读记录：脚本名 → 已读的剧情文本行集合。跨会话持久化（aread.dat）。
    read_lines: HashMap<String, HashSet<usize>>,
    auto_wait_elapsed_ms: u64,
    skip_wait_revealed: bool,
    skip_hold_frames: u8,
    was_skipping: bool,
    // ── controlskip：按住 Ctrl（keyconfig role 14）期间的强制跳过 ──
    control_skip_active: bool,
    // ── hide：临时隐藏消息窗 ──
    hide_allowed: bool,
    /// [hide window=] 配置的同时隐藏的图层 ID 列表
    hide_window: Vec<String>,
    hide_active: bool,
    /// 进入隐藏时记录的 (图层, 原可见性)，退出时按此恢复
    hidden_layers: Vec<(String, bool)>,
    /// 当前活动消息层（跟踪 [chgmsg]），隐藏时随 window 列表一起隐藏
    pub(super) active_message_layer: Option<String>,
    // ── rclick：右键单击脚本 ──
    rclick_allowed: bool,
    /// 右键脚本路径；None=默认 rclick.iet
    rclick_file: Option<String>,
    // ── avoid：紧急回避（keyconfig role 15/16 触发）──
    /// [avoid] 配置的回避覆盖图路径；None=未配置（回避键无效）
    avoid_file: Option<String>,
    /// [avoid] 配置的 windowbutton 行为：0=禁用/1=默认/2=退出回避并执行处理器
    avoid_windowbutton: i32,
    /// 当前是否处于回避状态（覆盖显示中 + 音频静音）
    avoid_active: bool,
    // ── autosave ──
    /// 0=禁用；1=退出/切后台自动保存；2=每次输入等待时自动保存
    pub(super) autosave_allow: i32,
    // ── keyconfig：role → 键 ID 列表 ──
    keymap: HashMap<i32, Vec<u32>>,
    // ── httpget/httppost 挂起请求 ──
    pub(super) pending_http: Option<PendingHttp>,
    pub(super) http_serial: u64,
    // ── 宿主查询钩子是否已安装（file_exists/file_crc/get_sound_info 等）──
    pub(super) host_hooks_installed: bool,
}

/// keyconfig 的默认按键分配（docs/spec/key_assign.md，Windows 缺省）：
/// Enter=前进、Space=隐藏、↑=日志、A=自动、Shift=跳过切换、Ctrl=强制跳过。
fn default_keymap() -> HashMap<i32, Vec<u32>> {
    HashMap::from([
        (ROLE_ADVANCE, vec![13]),
        (ROLE_HIDE_IN, vec![32]),
        (ROLE_HIDE_OUT, vec![32]),
        (ROLE_BACKLOG_IN, vec![38]),
        (ROLE_AUTOMODE_IN, vec![65]),
        (ROLE_SKIP_IN, vec![16]),
        (ROLE_SKIP_OUT, vec![16]),
        (ROLE_CONTROL_SKIP, vec![17]),
    ])
}

// keyconfig 的 role 取值（docs/tag/system/keyconfig.md）
const ROLE_ADVANCE: i32 = 0;
const ROLE_HIDE_IN: i32 = 3;
const ROLE_HIDE_OUT: i32 = 4;
const ROLE_BACKLOG_IN: i32 = 5;
const ROLE_BACKLOG_OUT: i32 = 6;
const ROLE_AUTOMODE_IN: i32 = 9;
const ROLE_AUTOMODE_OUT: i32 = 10;
const ROLE_AUTOMODE_OUT_NOCLICK: i32 = 11;
const ROLE_SKIP_IN: i32 = 12;
const ROLE_SKIP_OUT: i32 = 13;
const ROLE_CONTROL_SKIP: i32 = 14;
const ROLE_AVOID_IN: i32 = 15;
const ROLE_AVOID_OUT: i32 = 16;

impl Default for RuntimeControlState {
    fn default() -> Self {
        Self {
            skip_allowed: true,
            skip_unread: true,
            skip_active: false,
            automode_allowed: true,
            automode_active: false,
            automode_stop_by_click: true,
            automode_stop_by_stop: true,
            automode_sync_se: Vec::new(),
            already_read_enabled: true,
            read_lines: HashMap::new(),
            automode_layer: None,
            auto_wait_elapsed_ms: 0,
            skip_wait_revealed: false,
            skip_hold_frames: 0,
            was_skipping: false,
            control_skip_active: false,
            hide_allowed: true,
            hide_window: Vec::new(),
            hide_active: false,
            hidden_layers: Vec::new(),
            active_message_layer: None,
            rclick_allowed: false,
            rclick_file: None,
            avoid_file: None,
            avoid_windowbutton: 0,
            avoid_active: false,
            autosave_allow: 0,
            keymap: default_keymap(),
            pending_http: None,
            http_serial: 0,
            host_hooks_installed: false,
        }
    }
}

impl RuntimeControlState {
    pub(super) fn skip_active(&self) -> bool {
        (self.skip_active && self.skip_allowed) || self.control_skip_active
    }

    pub(super) fn control_skip_active(&self) -> bool {
        self.control_skip_active
    }

    pub(super) fn hide_active(&self) -> bool {
        self.hide_active
    }

    /// key 边沿命中的 role 列表（升序，保证派发顺序稳定）。
    fn roles_for_key(&self, key: u32) -> Vec<i32> {
        let mut roles: Vec<i32> = self
            .keymap
            .iter()
            .filter(|(_, keys)| keys.contains(&key))
            .map(|(role, _)| *role)
            .collect();
        roles.sort_unstable();
        roles
    }

    pub(super) fn automode_active(&self) -> bool {
        self.automode_active && self.automode_allowed
    }

    /// syncse 列表（为空表示用通用"任意语音在播"门控）。
    pub(super) fn automode_sync_se(&self) -> &[String] {
        &self.automode_sync_se
    }

    /// 该剧情文本行是否已读。
    pub(super) fn is_read(&self, script: &str, line: usize) -> bool {
        self.read_lines
            .get(script)
            .is_some_and(|lines| lines.contains(&line))
    }

    /// 标记剧情文本行为已读。返回是否是新标记（用于决定是否需要持久化）。
    pub(super) fn mark_read(&mut self, script: &str, line: usize) -> bool {
        self.read_lines
            .entry(script.to_string())
            .or_default()
            .insert(line)
    }

    /// 已读跳过遇未读是否应停止：已读判定开启 且 skip 配置为"不跳过未读"。
    pub(super) fn unread_stops_skip(&self) -> bool {
        self.already_read_enabled && !self.skip_unread
    }

    /// 已读记录序列化为 `脚本 → 已读行列表`（持久化用）。
    pub(super) fn read_lines_export(&self) -> HashMap<String, Vec<usize>> {
        self.read_lines
            .iter()
            .map(|(k, v)| {
                let mut lines: Vec<usize> = v.iter().copied().collect();
                lines.sort_unstable();
                (k.clone(), lines)
            })
            .collect()
    }

    /// 从持久化数据恢复已读记录（合并，不清空现有）。
    pub(super) fn read_lines_import(&mut self, data: HashMap<String, Vec<usize>>) {
        for (script, lines) in data {
            self.read_lines.entry(script).or_default().extend(lines);
        }
    }

    pub(super) fn reset_auto_wait(&mut self) {
        self.auto_wait_elapsed_ms = 0;
    }

    pub(super) fn reset_wait_flags(&mut self) {
        self.auto_wait_elapsed_ms = 0;
        self.skip_wait_revealed = false;
        self.skip_hold_frames = 0;
    }

    fn reset_modes_for_load(&mut self) {
        self.skip_active = false;
        self.automode_active = false;
        self.was_skipping = false;
        self.control_skip_active = false;
        // 隐藏态是运行时瞬态：读档恢复的场景不再包含被隐藏的可见性覆盖。
        self.hide_active = false;
        self.hidden_layers.clear();
        self.pending_http = None;
        self.reset_wait_flags();
    }

    pub(super) fn mark_skip_wait_revealed(&mut self) {
        self.skip_wait_revealed = true;
    }

    pub(super) fn skip_wait_revealed(&self) -> bool {
        self.skip_wait_revealed
    }

    pub(super) fn should_auto_advance(
        &mut self,
        delta_ms: u64,
        text_ready: bool,
        wait_ms: u64,
    ) -> bool {
        if !self.automode_active() {
            self.auto_wait_elapsed_ms = 0;
            return false;
        }
        if !text_ready {
            self.auto_wait_elapsed_ms = 0;
            return false;
        }
        self.auto_wait_elapsed_ms = self.auto_wait_elapsed_ms.saturating_add(delta_ms);
        self.auto_wait_elapsed_ms >= wait_ms
    }
}

impl CoreRuntime {
    pub(super) fn set_skip_mode(&mut self, enabled: bool) {
        if enabled && !self.control.skip_allowed {
            return;
        }
        let next = enabled;
        if next && self.control.automode_active {
            self.set_automode_mode(false);
        }
        if self.control.skip_active == next {
            self.audio.set_skipping(self.control.skip_active());
            return;
        }
        self.control.skip_active = next;
        self.control.reset_auto_wait();
        // 用合成后的跳过态（含 controlskip）同步音频，避免 Ctrl 按住期间
        // 关闭 commandskip 把音频的跳过标记误清掉。
        self.audio.set_skipping(self.control.skip_active());
        self.sync_control_status_variables();
        if !next {
            self.control.was_skipping = true;
        }
        if !next {
            // 退出 skip 时立即把当前文字全部揭示，否则
            // 新推入的文本会卡在 advance_reveal 的初始进度（仅 1-2 字）。
            self.reveal_text_now();
        }
        self.enqueue_control_handler(if next {
            "commandskipin"
        } else {
            "commandskipout"
        });
    }

    pub(super) fn set_automode_mode(&mut self, enabled: bool) {
        if enabled && !self.control.automode_allowed {
            return;
        }
        let next = enabled;
        if next && self.control.skip_active {
            self.set_skip_mode(false);
        }
        if self.control.automode_active == next {
            return;
        }
        self.control.automode_active = next;
        self.control.reset_auto_wait();
        self.sync_control_status_variables();
        self.enqueue_control_handler(if next { "automodein" } else { "automodeout" });
    }

    /// [skip] 配置：allow/unread 缺省（None）时继承之前的设置，只覆盖显式给出的值。
    pub(super) fn apply_skip_config(&mut self, allow: Option<bool>, skip_unread: Option<bool>) {
        if let Some(allow) = allow {
            self.control.skip_allowed = allow;
            if !allow {
                self.set_skip_mode(false);
            }
        }
        if let Some(skip_unread) = skip_unread {
            self.control.skip_unread = skip_unread;
        }
    }

    pub(super) fn apply_automode_config(
        &mut self,
        allow: bool,
        layer: Option<String>,
        stop_by_click: Option<bool>,
        stop_by_stop: Option<bool>,
        sync_se: Option<Vec<String>>,
    ) {
        self.control.automode_allowed = allow;
        self.control.automode_layer = layer;
        // None=保留之前设置（文档语义）。
        if let Some(v) = stop_by_click {
            self.control.automode_stop_by_click = v;
        }
        if let Some(v) = stop_by_stop {
            self.control.automode_stop_by_stop = v;
        }
        if let Some(se) = sync_se {
            self.control.automode_sync_se = se;
        }
        if !allow {
            self.set_automode_mode(false);
        }
    }

    /// [alreadyread mode]：0=不做「未读停跳」判定；非 0=做（默认）。
    ///
    /// 注意此开关**只**门控「已读跳过遇未读是否停跳」这一判定行为；已读记录的
    /// **写入**与 `s.status.alreadyread` 的**暴露**始终进行（见
    /// [`track_read_and_stop_skip_on_unread`](Self::track_read_and_stop_skip_on_unread)），
    /// 以便自绘「既読」标记的游戏在 mode=0 下仍能读到可靠的已读状态。
    pub(super) fn apply_alreadyread(&mut self, mode: i32) {
        self.control.already_read_enabled = mode != 0;
    }

    /// 已读跟踪 + 未读停跳 + `s.status.alreadyread` 暴露。
    ///
    /// 已读键 = (脚本文件, 该段之后的等待行号)，按**脚本执行位置**隔离——这天然
    /// 免疫 chgmsg 多消息层：每段文本(主层/tips/子层)都落在各自唯一的脚本行，
    /// 判定的永远是「当前执行行」是否已读，与它进的是哪个消息层无关。故不再依赖
    /// 「一屏至多一段 ScenarioText」这一（被 chgmsg 推翻的）前提。
    ///
    /// 语义分层（与 alreadyread.md 一致，两件事解耦）：
    /// - **记录已读 + 暴露 `s.status.alreadyread`**：始终进行。真实 Artemis 里
    ///   带 Lua 的游戏并不自己从零记已读，而是读引擎暴露的 `s.status.alreadyread`
    ///   来画「既読」标记 / 做跳过决策，故此值必须可靠反映「当前行此前是否已读」。
    /// - **「未读停跳」判定**：仅在 `[alreadyread mode!=0]`（默认）时生效。mode=0
    ///   关掉判定后，即便已读跳过遇未读也不停跳（alreadyread.md 明示的唯一具体效果）。
    ///
    /// 每帧调用一次。
    pub(super) fn track_read_and_stop_skip_on_unread(&mut self) {
        // 消费本帧"是否展示了剧情文本"的标志。
        let shown = std::mem::take(&mut self.scenario_text_shown);
        if !shown {
            return;
        }
        // 只在文本后确实建立了等待（停止/点击/计时）时判定这一段剧情。
        if self.wait_reason.is_none() {
            return;
        }
        let Some(script) = self.interpreter.current_script().map(str::to_string) else {
            return;
        };
        let line = self.interpreter.current_line();

        // 本行在「此前的访问/会话」是否已读 —— 必须在本次标记之前取值。
        let was_read = self.control.is_read(&script, line);
        // 暴露给脚本：s.status.alreadyread（当前执行行此前是否已读，1/0）。
        self.interpreter.set_variable(
            "s.status.alreadyread",
            Value::Int(if was_read { 1 } else { 0 }),
        );

        // 已读跳过遇未读剧情：仅在启用判定(mode!=0)时停跳（[skip unread=0] 即不跳未读）。
        if self.control.already_read_enabled
            && self.skip_active()
            && self.control.unread_stops_skip()
            && !was_read
        {
            self.set_skip_mode(false);
        }
        // 标记本段剧情已读（始终维护，使 s.status.alreadyread 跨访问准确）；
        // 有新增则置脏，供 syssave 落 aread.dat。
        if self.control.mark_read(&script, line) {
            self.read_dirty = true;
        }
    }

    /// 自动模式下左键单击是否应停止自动模式。
    pub(super) fn automode_stops_on_click(&self) -> bool {
        self.control.automode_active() && self.control.automode_stop_by_click
    }

    /// 自动模式下 [stop] 是否应停止自动模式。
    pub(super) fn automode_stops_on_stop(&self) -> bool {
        self.control.automode_active() && self.control.automode_stop_by_stop
    }

    pub(super) fn disable_auto_skip(&mut self) {
        self.set_automode_mode(false);
        self.set_skip_mode(false);
    }

    /// Loading a slot always leaves transient playback controls disabled.
    /// These flags belong to the live runtime, not to the serialized script
    /// state, and carrying them through a load makes the restored script race
    /// through waits before its own UI cleanup can run.
    pub(super) fn reset_control_modes_for_load(&mut self) {
        self.control.reset_modes_for_load();
        self.audio.set_skipping(false);
    }

    pub(super) fn apply_exec_command(&mut self, command: &str, mode: Option<i32>) {
        match command {
            "automode" => {
                let enabled = mode.unwrap_or(1) != 0;
                self.set_automode_mode(enabled);
            }
            "skip" => {
                let enabled = mode
                    .map(|value| value != 0)
                    .unwrap_or(!self.control.skip_active);
                self.set_skip_mode(enabled);
            }
            // 与物理右键相同的完整 rclick 链（rclick 脚本优先，回退 push key=2）
            "rclick" => {
                if !self.trigger_rclick() {
                    self.enqueue_exec_input("push", "2");
                }
            }
            // 引擎自有的隐藏模式切换（隐藏消息层 + window 列表，派发 hidein/hideout）
            "hide" => {
                self.toggle_hide_mode();
            }
            // 窗口操作（仅 Windows）：核心不管理窗口，经 ui_command 转发宿主。
            "fullscreen" | "minimize" => {
                crate::ffi::emit_ui_command("exec", serde_json::json!({ "command": command }));
            }
            _ => {
                if let Some((event_name, key)) = exec_input_route(command) {
                    self.enqueue_exec_input(event_name, key);
                } else {
                    crate::core_warn!("[exec] 未实现的命令被忽略: {command}");
                }
            }
        }
    }

    /// 派发 exec 命令对应的全局输入处理器（与真实输入走同一条 enqueue 链）。
    fn enqueue_exec_input(&mut self, event_name: &str, key: &str) {
        let Some(handler) = self.compositor.get_input_handler(event_name, key) else {
            crate::core_debug!("[exec] 未注册 {event_name} key={key:?} 处理器，忽略");
            return;
        };
        // rclick 复用 push key=2 的右键链，参数格式与 process_pointer_handlers
        // 派发物理按键时一致；hidein/backlogin 与 enqueue_control_handler 一致。
        let runtime_params: Vec<(&str, &str)> = if key.is_empty() {
            vec![("type", event_name)]
        } else {
            vec![("key", key), ("type", "click")]
        };
        enqueue_handler_tags(
            &self.interpreter,
            handler.handler.as_deref(),
            handler.file.as_deref(),
            handler.label.as_deref(),
            handler.call,
            &handler.params,
            &runtime_params,
        );
    }

    pub(super) fn reset_control_wait_flags(&mut self) {
        self.control.reset_wait_flags();
    }

    pub(super) fn skip_active(&self) -> bool {
        self.control.skip_active()
    }

    pub(super) fn was_skipping(&self) -> bool {
        self.control.was_skipping
    }

    pub(super) fn clear_was_skipping(&mut self) {
        self.control.was_skipping = false;
    }

    pub(super) fn should_auto_advance(&mut self, delta_ms: u64) -> bool {
        let text_ready = self.is_text_reveal_complete();
        // syncse：等指定 SE/语音播完（空列表退化为等任意语音播完）。
        let voice_ready = self.automode_sync_ready();
        let wait_ms = self
            .interpreter
            .get_variable("s.automodewait")
            .and_then(|value| value.as_int())
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or(AUTO_ADVANCE_DELAY_MS);
        self.control
            .should_auto_advance(delta_ms, text_ready && voice_ready, wait_ms)
    }

    pub(super) fn should_hold_for_skip_reveal(&mut self) -> bool {
        if self.control.skip_wait_revealed() {
            if self.control.skip_hold_frames < 3 {
                self.control.skip_hold_frames += 1;
                return true;
            }
            return false;
        }
        self.reveal_text_now();
        self.control.mark_skip_wait_revealed();
        self.control.skip_hold_frames = 1;
        true
    }

    fn enqueue_control_handler(&mut self, event_name: &str) {
        let Some(handler) = self.compositor.get_input_handler(event_name, "") else {
            return;
        };
        enqueue_handler_tags(
            &self.interpreter,
            handler.handler.as_deref(),
            handler.file.as_deref(),
            handler.label.as_deref(),
            handler.call,
            &handler.params,
            &[("type", event_name)],
        );
    }

    pub(super) fn sync_control_status_variables(&mut self) {
        self.interpreter.set_variable(
            "s.status.commandskip",
            Value::Int(if self.control.skip_active() { 1 } else { 0 }),
        );
        self.interpreter.set_variable(
            "s.status.automode",
            Value::Int(if self.control.automode_active() { 1 } else { 0 }),
        );
        self.interpreter.set_variable(
            "s.status.controlskip",
            Value::Int(if self.control.control_skip_active() {
                1
            } else {
                0
            }),
        );
    }

    // ── controlskip：按住强制跳过 ─────────────────────────────────────

    /// 强制跳过态切换：进入/退出时派发 controlskipin/out 并同步
    /// s.status.controlskip。强制跳过无视 [skip allow=0]（这正是"强制"）。
    pub(super) fn set_control_skip(&mut self, active: bool) {
        if self.control.control_skip_active == active {
            return;
        }
        self.control.control_skip_active = active;
        self.control.reset_auto_wait();
        self.audio.set_skipping(self.control.skip_active());
        self.sync_control_status_variables();
        if !active {
            self.control.was_skipping = true;
            // 与 commandskip 退出一致：立即揭示当前文本，避免卡在初始进度。
            self.reveal_text_now();
        }
        self.enqueue_control_handler(if active {
            "controlskipin"
        } else {
            "controlskipout"
        });
    }

    /// 每帧根据按住的键更新强制跳过态（keyconfig role 14，默认 Ctrl=17）。
    pub(super) fn update_control_skip_from_keys(&mut self, keys_down: &HashSet<u32>) {
        let held = self
            .control
            .keymap
            .get(&ROLE_CONTROL_SKIP)
            .is_some_and(|keys| keys.iter().any(|key| keys_down.contains(key)));
        self.set_control_skip(held);
    }

    // ── hide：临时隐藏消息窗 ─────────────────────────────────────────

    /// [hide] 配置：allow 启用/禁用；window 缺省（None）继承之前设置。
    pub(super) fn apply_hide_config(&mut self, allow: bool, window: Option<&[String]>) {
        self.control.hide_allowed = allow;
        if let Some(window) = window {
            self.control.hide_window = window.to_vec();
        }
        if !allow && self.control.hide_active() {
            self.exit_hide_mode();
        }
    }

    pub(super) fn hide_active(&self) -> bool {
        self.control.hide_active()
    }

    pub(super) fn toggle_hide_mode(&mut self) {
        if self.control.hide_active() {
            self.exit_hide_mode();
        } else {
            self.enter_hide_mode();
        }
    }

    /// 进入隐藏模式：隐藏活动消息层 + window 列表图层（记录原可见性），
    /// 派发 hidein 处理器。
    pub(super) fn enter_hide_mode(&mut self) {
        if !self.control.hide_allowed || self.control.hide_active {
            return;
        }
        let mut targets = self.control.hide_window.clone();
        if let Some(message_layer) = &self.control.active_message_layer
            && !targets.contains(message_layer)
        {
            targets.push(message_layer.clone());
        }

        let mut saved = Vec::new();
        for id in targets {
            let Some(layer) = self.compositor.scene().get(&id) else {
                continue;
            };
            saved.push((id.clone(), layer.props.is_visible()));
            self.set_layer_visible(&id, false);
        }
        self.control.hidden_layers = saved;
        self.control.hide_active = true;
        self.sync_layer_info_all();
        self.enqueue_control_handler("hidein");
    }

    /// 退出隐藏模式：恢复进入时记录的图层可见性，派发 hideout 处理器。
    pub(super) fn exit_hide_mode(&mut self) {
        if !self.control.hide_active {
            return;
        }
        let saved = std::mem::take(&mut self.control.hidden_layers);
        for (id, was_visible) in saved {
            if was_visible && self.compositor.scene().get(&id).is_some() {
                self.set_layer_visible(&id, true);
            }
        }
        self.control.hide_active = false;
        self.sync_layer_info_all();
        self.enqueue_control_handler("hideout");
    }

    fn set_layer_visible(&mut self, id: &str, visible: bool) {
        self.compositor.apply_event(&asb_interpreter::Event::Layer(
            asb_interpreter::event::LayerEvent::SetProperty {
                id: id.to_string(),
                property: "visible".to_string(),
                value: if visible { "1" } else { "0" }.to_string(),
            },
        ));
    }

    // ── rclick：右键单击脚本 ─────────────────────────────────────────

    /// [rclick] 配置：allow 启用/禁用；file 缺省（None）继承之前设置。
    pub(super) fn apply_rclick_config(&mut self, allow: bool, file: Option<&str>) {
        self.control.rclick_allowed = allow;
        if let Some(file) = file.filter(|f| !f.is_empty()) {
            self.control.rclick_file = Some(file.to_string());
        }
    }

    /// 引擎右键链：隐藏态先恢复 → rclick 脚本（call 进入 / 再次右键跳 leave）。
    ///
    /// 返回是否被消费；未消费（rclick 未启用）时调用方按原有 push key=2
    /// 处理链继续派发。
    pub(super) fn trigger_rclick(&mut self) -> bool {
        // 隐藏模式下右键先恢复消息窗，不再进右键菜单。
        if self.control.hide_active() {
            self.exit_hide_mode();
            return true;
        }

        if !self.control.rclick_allowed {
            return false;
        }
        let file = self
            .control
            .rclick_file
            .clone()
            .unwrap_or_else(|| DEFAULT_RCLICK_SCRIPT.to_string());
        // 已在右键脚本内：跳到 leave 标签做收尾（其 return 回到右键前位置）。
        // 用当前脚本名近似判定"在 rclick 脚本内"；嵌套 call 到其他文件时
        // 判定失真，属已知近似。
        if self.interpreter.current_script() == Some(file.as_str()) {
            enqueue_handler_tags(
                &self.interpreter,
                None,
                Some(&file),
                Some(RCLICK_LEAVE_LABEL),
                false,
                &HashMap::new(),
                &[],
            );
        } else {
            // 以子例程方式进入右键脚本（call 压返回帧，脚本 return 后回原位）。
            enqueue_handler_tags(
                &self.interpreter,
                None,
                Some(&file),
                None,
                true,
                &HashMap::new(),
                &[],
            );
        }
        true
    }

    // ── avoid：紧急回避 ─────────────────────────────────────────────

    /// [avoid] 配置：存储覆盖图路径与 windowbutton，等 keyconfig role 15 触发。
    /// 不再在配置时立即显示覆盖（那是触发键的职责）。
    pub(super) fn apply_avoid_config(&mut self, file: Option<&str>, windowbutton: i32) {
        self.control.avoid_file = file.filter(|f| !f.is_empty()).map(str::to_string);
        self.control.avoid_windowbutton = windowbutton;
    }

    /// 进入紧急回避：全屏覆盖（宿主实现）+ 音频静音。未配置 [avoid] 时忽略。
    pub(super) fn enter_avoid(&mut self) {
        if self.control.avoid_active || self.control.avoid_file.is_none() {
            return;
        }
        self.control.avoid_active = true;
        self.audio.set_master_volume(0.0);
        crate::ffi::emit_ui_command(
            "avoid",
            serde_json::json!({
                "action": "show",
                "file": self.control.avoid_file,
                "windowbutton": self.control.avoid_windowbutton,
            }),
        );
    }

    /// 退出紧急回避：撤覆盖 + 恢复音量。
    pub(super) fn exit_avoid(&mut self) {
        if !self.control.avoid_active {
            return;
        }
        self.control.avoid_active = false;
        self.audio.set_master_volume(1.0);
        crate::ffi::emit_ui_command("avoid", serde_json::json!({ "action": "hide" }));
    }

    fn toggle_avoid(&mut self) {
        if self.control.avoid_active {
            self.exit_avoid();
        } else {
            self.enter_avoid();
        }
    }

    // ── autosave ───────────────────────────────────────────────────

    /// [autosave] 配置：0=禁用；1=退出/切后台时；2=每次输入等待时。
    pub(super) fn apply_autosave_config(&mut self, allow: i32) {
        self.control.autosave_allow = allow;
    }

    // ── keyconfig ──────────────────────────────────────────────────

    /// [keyconfig] 应用：role → keys 键 ID 列表覆盖默认分配。
    pub(super) fn apply_keyconfig(&mut self, params: &HashMap<String, String>) {
        match parse_keyconfig(params) {
            Some((role, keys)) => {
                self.control.keymap.insert(role, keys);
            }
            None => {
                crate::core_warn!("[keyconfig] 参数不合法，忽略: {params:?}");
            }
        }
    }

    /// 键盘按下边沿的 role 派发。返回该键是否触发"前进"（role 0）。
    pub(super) fn handle_role_key_edge(&mut self, key: u32) -> bool {
        let roles = self.control.roles_for_key(key);
        let mut advance = false;
        for role in roles {
            match role {
                ROLE_ADVANCE => advance = true,
                // 同一键同时配了开始/结束（如缺省 Space/Shift）即为切换语义，
                // 由 in 分支统一处理，out 分支只在单独配置结束键时生效。
                ROLE_HIDE_IN => self.toggle_hide_mode(),
                ROLE_HIDE_OUT => {
                    if !self.key_is_toggle(key, ROLE_HIDE_IN) {
                        self.exit_hide_mode();
                    }
                }
                ROLE_BACKLOG_IN => self.enqueue_exec_input("backlogin", ""),
                ROLE_BACKLOG_OUT => self.enqueue_exec_input("backlogout", ""),
                ROLE_AUTOMODE_IN => self.set_automode_mode(true),
                ROLE_AUTOMODE_OUT | ROLE_AUTOMODE_OUT_NOCLICK => {
                    if !self.key_is_toggle(key, ROLE_AUTOMODE_IN) {
                        self.set_automode_mode(false);
                    }
                }
                ROLE_SKIP_IN => {
                    if self.key_is_toggle(key, ROLE_SKIP_OUT) {
                        let next = !self.control.skip_active;
                        self.set_skip_mode(next);
                    } else {
                        self.set_skip_mode(true);
                    }
                }
                ROLE_SKIP_OUT => {
                    if !self.key_is_toggle(key, ROLE_SKIP_IN) {
                        self.set_skip_mode(false);
                    }
                }
                // 紧急回避（keyconfig role 15/16）：开始=覆盖 + 静音，结束=恢复。
                // 同键既配 in 又配 out 时按切换语义处理。
                ROLE_AVOID_IN => {
                    if self.key_is_toggle(key, ROLE_AVOID_OUT) {
                        self.toggle_avoid();
                    } else {
                        self.enter_avoid();
                    }
                }
                ROLE_AVOID_OUT => {
                    if !self.key_is_toggle(key, ROLE_AVOID_IN) {
                        self.exit_avoid();
                    }
                }
                // role 14（强制跳过）按"按住"处理，见 update_control_skip_from_keys。
                _ => {}
            }
        }
        advance
    }

    /// 某键是否同时被分配给了另一个 role（开始/结束同键 → 切换语义）。
    fn key_is_toggle(&self, key: u32, other_role: i32) -> bool {
        self.control
            .keymap
            .get(&other_role)
            .is_some_and(|keys| keys.contains(&key))
    }
}

/// 解析 [keyconfig] 的原始参数：role=角色编号、keys=键 ID 数组（逗号/空格分隔）。
fn parse_keyconfig(params: &HashMap<String, String>) -> Option<(i32, Vec<u32>)> {
    let role = params.get("role")?.trim().parse::<i32>().ok()?;
    let keys: Vec<u32> = params
        .get("keys")
        .map(|raw| {
            raw.split(|c: char| c == ',' || c.is_whitespace())
                .filter(|s| !s.is_empty())
                .filter_map(|s| s.trim().parse::<u32>().ok())
                .collect()
        })
        .unwrap_or_default();
    Some((role, keys))
}

/// exec 命令到全局输入处理链的映射：`(事件名, key)`。
///
/// - backlog：触发 `setonbacklogin` 的处理器；
/// - rclick / hide 走引擎自有状态机（trigger_rclick / toggle_hide_mode），
///   不再经此表。
fn exec_input_route(command: &str) -> Option<(&'static str, &'static str)> {
    match command {
        "backlog" => Some(("backlogin", "")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ROLE_ADVANCE, ROLE_AVOID_IN, ROLE_AVOID_OUT, ROLE_CONTROL_SKIP, ROLE_HIDE_IN, ROLE_SKIP_IN,
        RuntimeControlState, exec_input_route, parse_keyconfig,
    };
    use std::collections::HashMap;

    #[test]
    fn load_reset_disables_transient_advance_modes() {
        let mut control = RuntimeControlState {
            skip_active: true,
            automode_active: true,
            auto_wait_elapsed_ms: 500,
            skip_wait_revealed: true,
            skip_hold_frames: 3,
            was_skipping: true,
            control_skip_active: true,
            hide_active: true,
            hidden_layers: vec![("mw".into(), true)],
            ..RuntimeControlState::default()
        };

        control.reset_modes_for_load();

        assert!(!control.skip_active());
        assert!(!control.automode_active());
        assert_eq!(control.auto_wait_elapsed_ms, 0);
        assert!(!control.skip_wait_revealed);
        assert_eq!(control.skip_hold_frames, 0);
        assert!(!control.was_skipping);
        assert!(!control.control_skip_active());
        assert!(!control.hide_active());
        assert!(control.hidden_layers.is_empty());
    }

    #[test]
    fn exec_commands_route_to_the_matching_input_handlers() {
        // backlog 走全局事件；rclick/hide 改由引擎状态机直接处理，不再入表。
        assert_eq!(exec_input_route("backlog"), Some(("backlogin", "")));
        assert_eq!(exec_input_route("rclick"), None);
        assert_eq!(exec_input_route("hide"), None);
        // automode/skip/fullscreen/minimize 不在输入链映射内（各有专门分支）。
        assert_eq!(exec_input_route("automode"), None);
        assert_eq!(exec_input_route("fullscreen"), None);
        assert_eq!(exec_input_route("unknown"), None);
    }

    #[test]
    fn control_skip_forces_skip_even_when_commandskip_disallowed() {
        // 强制跳过（Ctrl 按住）应无视 [skip allow=0] 的门控。
        let control = RuntimeControlState {
            skip_allowed: false,
            skip_active: false,
            control_skip_active: true,
            ..RuntimeControlState::default()
        };
        assert!(control.skip_active());
        assert!(control.control_skip_active());
    }

    #[test]
    fn keyconfig_parses_role_and_number_array_keys() {
        // keys 是 NUMBER ARRAY（逗号分隔，容忍空格）
        let params = HashMap::from([
            ("role".to_string(), "12".to_string()),
            ("keys".to_string(), "16, 83 13".to_string()),
        ]);
        assert_eq!(parse_keyconfig(&params), Some((12, vec![16, 83, 13])));

        // keys 缺省 → 清空该 role 的分配
        let params = HashMap::from([("role".to_string(), "0".to_string())]);
        assert_eq!(parse_keyconfig(&params), Some((0, vec![])));

        // role 非法 → None
        let params = HashMap::from([("role".to_string(), "x".to_string())]);
        assert_eq!(parse_keyconfig(&params), None);
    }

    #[test]
    fn default_keymap_matches_key_assign_spec() {
        // docs/spec/key_assign.md：Enter 前进、Space 隐藏、Shift 跳过、Ctrl 强制跳过
        let control = RuntimeControlState::default();
        assert_eq!(control.roles_for_key(13), vec![ROLE_ADVANCE]);
        assert!(control.roles_for_key(32).contains(&ROLE_HIDE_IN));
        assert!(control.roles_for_key(16).contains(&ROLE_SKIP_IN));
        assert_eq!(control.roles_for_key(17), vec![ROLE_CONTROL_SKIP]);
        // 未分配的键无 role
        assert!(control.roles_for_key(999).is_empty());
    }

    #[test]
    fn automode_stop_flags_default_on_and_gate_by_active() {
        // stopbyclick/stopbystop 默认开；仅在自动模式激活时门控生效。
        let mut control = RuntimeControlState::default();
        assert!(control.automode_stop_by_click);
        assert!(control.automode_stop_by_stop);
        // 未激活自动模式：不停止。
        assert!(!control.automode_active());
        // 激活后（allow+active）门控为真。
        control.automode_active = true;
        assert!(control.automode_active());
        assert!(control.automode_stop_by_click && control.automode_active());
    }

    #[test]
    fn already_read_tracking_and_unread_stop_gate() {
        let mut control = RuntimeControlState::default();
        // 默认：已读判定开、skip_unread=true（可跳未读）→ 未读不停跳。
        assert!(!control.unread_stops_skip());
        // [skip unread=0]（不跳未读）+ 已读判定开 → 未读应停跳。
        control.skip_unread = false;
        assert!(control.unread_stops_skip());
        // [alreadyread mode=0] 关掉已读判定 → 不停跳（Lua 游戏走这条）。
        control.already_read_enabled = false;
        assert!(!control.unread_stops_skip());

        // 标记/查询已读，按脚本隔离。
        assert!(!control.is_read("scene01.asb", 100));
        assert!(control.mark_read("scene01.asb", 100)); // 新标记返回 true
        assert!(!control.mark_read("scene01.asb", 100)); // 重复返回 false
        assert!(control.is_read("scene01.asb", 100));
        assert!(!control.is_read("tips.asb", 100)); // 不同脚本互不影响

        // 导出/导入往返。
        control.mark_read("scene01.asb", 101);
        let export = control.read_lines_export();
        assert_eq!(export.get("scene01.asb"), Some(&vec![100, 101]));
        let mut fresh = RuntimeControlState::default();
        fresh.read_lines_import(export);
        assert!(fresh.is_read("scene01.asb", 100) && fresh.is_read("scene01.asb", 101));
    }

    #[test]
    fn keyconfig_assigns_avoid_roles() {
        // docs/tag/system/keyconfig.md：role 15=紧急回避开始、16=结束。
        // 缺省无键位（游戏用 keyconfig 分配），配置后应能解析。
        assert_eq!(ROLE_AVOID_IN, 15);
        assert_eq!(ROLE_AVOID_OUT, 16);
        let (role_in, keys_in) = parse_keyconfig(&HashMap::from([
            ("role".into(), "15".into()),
            ("keys".into(), "88".into()),
        ]))
        .expect("role 15 应可解析");
        assert_eq!(role_in, ROLE_AVOID_IN);
        assert_eq!(keys_in, vec![88]);

        let mut control = RuntimeControlState::default();
        control.keymap.insert(role_in, keys_in);
        assert!(control.roles_for_key(88).contains(&ROLE_AVOID_IN));
    }
}
