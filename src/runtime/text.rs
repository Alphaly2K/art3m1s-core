use super::CoreRuntime;
use crate::render_pipeline::draw::DrawCommand;
use crate::text::render::{ScetweenConfig, TextRenderer, TextSpanToken};
use asb_interpreter::Event;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

fn text_span_ready(state: &crate::text::render::FontState, span: &TextSpanToken) -> Option<bool> {
    let layer = state.layers.get(&span.layer_id)?;
    (layer.generation == span.generation).then_some(!layer.reveal_pending)
}

#[derive(Debug, Clone)]
pub(super) struct PendingTextTranslation {
    span: Option<TextSpanToken>,
    translated: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PendingScenarioText {
    source: String,
    ruby: Option<String>,
    span: Option<TextSpanToken>,
}

/// backlog / message-tags 的进程级镜像，供解释器 `var system=get_backlog_size /
/// get_backlog_tags / get_message_tags` 的宿主查询钩子读取。
///
/// 钩子是进程级注册点（var 标签路径拿不到 runtime 实例，且 text_renderer 非
/// Send+Sync 不能直接跨线程借入），因此这里维护一份可克隆的快照：runtime 每帧从
/// text_renderer 抽取 backlog/消息层的再现标签序列刷进来，钩子只读它并按伪数组
/// 约定（name.0..N + name.size）落值。allfont=0/1 两套预先算好，查询时按需取用。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BacklogSnapshot {
    /// 每页的再现标签：`.0`=allfont=0 的序列、`.1`=allfont=1 的序列。
    /// 页码即下标（0=最旧页），长度即 get_backlog_size 的结果。
    pub pages: Vec<(Vec<String>, Vec<String>)>,
    /// 各消息层当前显示文本的再现标签：id → (allfont=0, allfont=1)。
    pub message_layers: HashMap<String, (Vec<String>, Vec<String>)>,
}

// 这三个访问器是给解释器宿主查询钩子（var system=get_backlog_* / get_message_tags）
// 消费的读取入口；钩子接线在 ../asb-interpreter 与 events.rs（超出本任务白名单，见
// skipped）。在此之前非测试构建里它们无调用方，故允许 dead_code 以免噪声。
#[allow(dead_code)]
impl BacklogSnapshot {
    /// get_backlog_size：已存页数。
    pub fn backlog_size(&self) -> usize {
        self.pages.len()
    }

    /// get_backlog_tags：第 `page` 页的再现标签（越界返回 None）。
    pub fn backlog_tags(&self, page: usize, allfont: bool) -> Option<Vec<String>> {
        self.pages
            .get(page)
            .map(|(no, yes)| if allfont { yes.clone() } else { no.clone() })
    }

    /// get_message_tags：消息层 `id` 的再现标签（层不存在返回 None）。
    pub fn message_tags(&self, id: &str, allfont: bool) -> Option<Vec<String>> {
        self.message_layers
            .get(id)
            .map(|(no, yes)| if allfont { yes.clone() } else { no.clone() })
    }
}

// HashMap::new() 非 const，无法直接放进 `static Mutex<_>`（Vec::new() 可以），
// 故用 LazyLock 首次访问时构造缺省快照。
static BACKLOG_SNAPSHOT: LazyLock<Mutex<BacklogSnapshot>> =
    LazyLock::new(|| Mutex::new(BacklogSnapshot::default()));

/// 当前消息层文本度量的进程级镜像：`(整体宽度, 总高度, 最后一行宽度)`。
/// 供 var system=get_message_layer_width/height/line_width 的宿主查询钩子读取，
/// 由 runtime 每帧从 text_renderer 刷新（同 backlog 快照，text_renderer 非
/// Send+Sync 不能直接借入进程级钩子）。
static TEXT_METRICS: Mutex<(f32, f32, f32)> = Mutex::new((0.0, 0.0, 0.0));

/// 读取当前文本度量快照。消费方是解释器宿主查询钩子。
pub(crate) fn text_metrics_snapshot() -> (f32, f32, f32) {
    *TEXT_METRICS.lock().unwrap()
}

/// 读取当前 backlog 快照（get_backlog_* / get_message_tags 钩子入口）。
///
/// 消费方是解释器宿主查询钩子（接线见 skipped），故非测试构建里暂无调用方。
#[allow(dead_code)]
pub(crate) fn backlog_snapshot() -> BacklogSnapshot {
    BACKLOG_SNAPSHOT.lock().unwrap().clone()
}

/// 从 FontState 抽取 backlog / 消息层再现标签，构造快照。
///
/// 拆成自由函数便于用 GlyphTextRenderer 直接单测，无需 GL runtime。
fn build_backlog_snapshot(state: &crate::text::render::FontState) -> BacklogSnapshot {
    let mut snapshot = BacklogSnapshot::default();
    // backlog 各页两套（allfont=0/1）再现标签，页码即下标（0=最旧页）。
    for page in 0..state.get_backlog_size() {
        let no = state.get_backlog_tags(page, false).unwrap_or_default();
        let yes = state.get_backlog_tags(page, true).unwrap_or_default();
        snapshot.pages.push((no, yes));
    }
    // 各消息层当前显示文本两套再现标签。
    for id in state.layers.keys() {
        let no = state.get_message_tags(id, false).unwrap_or_default();
        let yes = state.get_message_tags(id, true).unwrap_or_default();
        snapshot.message_layers.insert(id.clone(), (no, yes));
    }
    snapshot
}

/// 把当前活动消息层登记为合成器默认消息层，并建立「消息层 ID → 场景图层 ID」映射，
/// 使 [lyprop id="~xxx"] / id="~" 能解析到对应场景图层。
///
/// 消息层与场景图层此处同名（MessageLayerSwitch 分支已 ensure_layer 出同名场景层），
/// 故绑定为 id→id；将来若解耦可在此改写映射目标。`active` 为 None（消息层被弹空）
/// 时清默认消息层。拆成自由函数便于用 Compositor 直接单测。
fn apply_message_layer_binding(
    compositor: &mut crate::compositor::Compositor,
    active: Option<(String, bool)>,
) {
    match active {
        Some((message_id, layered)) => {
            let scene_id = message_layer_scene_id(&message_id, layered);
            compositor.ensure_layer(&scene_id);
            compositor.set_message_layer_binding(&message_id, &scene_id);
            compositor.set_default_message_layer(Some(message_id));
        }
        // 后续 [lyprop id="~"] 找不到默认层时按合成器约定忽略该操作。
        None => compositor.set_default_message_layer(None),
    }
}

fn message_layer_scene_id(message_id: &str, layered: bool) -> String {
    if layered {
        return message_id.to_string();
    }
    let mut encoded = String::with_capacity(
        crate::compositor::scene::MESSAGE_LAYER_OVERLAY_PREFIX.len() + message_id.len() * 2,
    );
    encoded.push_str(crate::compositor::scene::MESSAGE_LAYER_OVERLAY_PREFIX);
    for byte in message_id.as_bytes() {
        use std::fmt::Write;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

impl CoreRuntime {
    pub(super) fn set_text_renderer(&mut self, renderer: Box<dyn TextRenderer>) {
        self.text_renderer = Some(renderer);
    }

    /// 从 text_renderer 抽取 backlog / 消息层再现标签，刷进进程级快照，供解释器
    /// `var system=get_backlog_size / get_backlog_tags / get_message_tags` 的宿主
    /// 查询钩子读取。每帧（render 前）调用一次即可保证查询读到最新值。
    ///
    /// 注意：解释器侧的钩子字段与 execute_var_system 接线尚未落地（在
    /// ../asb-interpreter，超出本任务白名单），改动点见任务 skipped。快照本身
    /// 已可用，钩子接上后即刻生效。
    pub(super) fn sync_backlog_snapshot(&self) {
        let Some(renderer) = self.text_renderer.as_ref() else {
            return;
        };
        *BACKLOG_SNAPSHOT.lock().unwrap() = build_backlog_snapshot(renderer.font_state());
        // 顺带刷新文本度量（get_message_layer_width/height/line_width）。
        *TEXT_METRICS.lock().unwrap() = renderer
            .active_layer_text_metrics()
            .unwrap_or((0.0, 0.0, 0.0));
    }

    pub(super) fn advance_text(&mut self, delta_ms: u64) {
        let skip_active = self.skip_active();
        let was_skipping = self.was_skipping();
        let mut reveal_complete = false;
        if let Some(renderer) = self.text_renderer.as_mut() {
            renderer.advance_reveal(delta_ms);
            if skip_active {
                renderer.reveal_all();
            } else if was_skipping {
                renderer.reveal_all();
                reveal_complete = renderer.is_reveal_complete();
            }
        }
        if was_skipping && reveal_complete {
            self.clear_was_skipping();
        }
    }

    pub(super) fn reveal_text_now(&mut self) {
        if let Some(renderer) = self.text_renderer.as_mut() {
            renderer.reveal_all();
        }
    }

    pub(super) fn is_text_reveal_complete(&self) -> bool {
        self.text_renderer
            .as_ref()
            .map(|renderer| renderer.is_reveal_complete())
            .unwrap_or(true)
    }

    pub(super) fn build_text_commands(&mut self) -> HashMap<String, Vec<DrawCommand>> {
        let (commands, layered) = {
            let Some(renderer) = self.text_renderer.as_mut() else {
                return HashMap::new();
            };
            // 不再在这里调 advance_reveal(0)——那会把 reveal_index 重置为 1。
            // advance_reveal 只在 advance_text 里每帧调一次。
            let commands = renderer.build_text_commands(&mut self.texture_provider);
            let layered = commands
                .keys()
                .map(|id| {
                    let is_layered = renderer
                        .font_state()
                        .layers
                        .get(id)
                        .is_some_and(|layer| layer.layered);
                    (id.clone(), is_layered)
                })
                .collect::<HashMap<_, _>>();
            (commands, layered)
        };

        let mut remapped = HashMap::<String, Vec<DrawCommand>>::new();
        for (message_id, layer_commands) in commands {
            let scene_id = message_layer_scene_id(
                &message_id,
                layered.get(&message_id).copied().unwrap_or(false),
            );
            remapped.entry(scene_id).or_default().extend(layer_commands);
        }
        remapped
    }

    pub(super) fn apply_text_event(&mut self, event: &Event) -> Option<PendingScenarioText> {
        if let Event::FontSettings(settings) | Event::FontDefault(settings) = event
            && let Some(face) = settings.get("face").filter(|face| !face.is_empty())
        {
            self.load_script_font(face);
        }

        // 剧本文本在光栅化前先过注入链（汉化补丁等），需在借用 renderer 前算好。
        let mut background_request = None;
        let injected = match event {
            Event::ScenarioText { content, .. } => {
                let host_text = match crate::ffi::request_text_injection(content) {
                    crate::ffi::TextInjectResult::Unchanged => content.clone(),
                    crate::ffi::TextInjectResult::Replaced(text) => text,
                    crate::ffi::TextInjectResult::Pending => {
                        let ruby = self.text_renderer.as_ref().and_then(|renderer| {
                            let state = renderer.font_state();
                            let active = state
                                .active_layer
                                .as_deref()
                                .unwrap_or(crate::text::glyph::DEFAULT_MESSAGE_LAYER);
                            state
                                .layers
                                .get(active)
                                .and_then(|layer| layer.open_ruby.as_ref())
                                .map(|(_, text)| text.clone())
                        });
                        background_request = Some(PendingScenarioText {
                            source: content.clone(),
                            ruby,
                            span: None,
                        });
                        content.clone()
                    }
                };
                Some(self.text_inject.run(&host_text))
            }
            _ => None,
        };

        let mut tracked_span = None;
        let restored_face = {
            let Some(renderer) = self.text_renderer.as_mut() else {
                return None;
            };
            match event {
                Event::ScenarioText { content, inline } => {
                    let content = injected.as_deref().unwrap_or(content);
                    if background_request.is_some() {
                        tracked_span = renderer.push_text_tracked(content, *inline);
                    } else {
                        renderer.push_text(content, *inline);
                    }
                }
                Event::FontSettings(settings) => renderer.apply_font_settings(settings),
                Event::FontInit => renderer.font_init(),
                Event::FontClose => renderer.font_pop(),
                Event::FontDefault(settings) => renderer.font_default(settings),
                Event::MessageLayerSwitch { id, stack, layered } => {
                    renderer.switch_message_layer(id.as_deref(), *stack);
                    renderer.font_state_mut().active_layer_mut().layered = *layered == Some(1);
                    // 消息层切换后的 lyprop `~` 绑定在 renderer 借用结束后统一处理
                    // （见函数末尾 sync_message_layer_binding）。
                }
                Event::MessageLayerPop => renderer.pop_message_layer(),
                // [rt omitblankline=]：换行前按标签值更新"末行为空则不换行"，
                // 再执行换行（配置在 layout 上，push_line_break 会读取）。
                Event::LineBreak { omitblankline } => {
                    renderer
                        .font_state_mut()
                        .set_rt_omit_blank_line(*omitblankline);
                    renderer.push_line_break();
                }
                Event::PageBreak { backlog } => renderer.push_page_break(*backlog),
                Event::GlyphConfig(config) => renderer.set_glyph_config(config),
                // [indent]：对话缩进的字符对/识别范围/嵌套（空 pair 即禁用缩进）。
                Event::IndentConfig { pair, range, nest } => {
                    renderer
                        .font_state_mut()
                        .set_indent(Some(pair.as_str()), *range, Some(*nest));
                }
                // [prohibit]：自定义行首/行尾禁则字符集，覆盖内置默认表。
                Event::ProhibitConfig { head, foot } => {
                    renderer
                        .font_state_mut()
                        .set_prohibit(Some(head.as_str()), Some(foot.as_str()));
                }
                // [wordparts]：视为单词组成部分的字符集（避免英文单词被拦腰换行）。
                Event::WordpartsConfig { parts } => {
                    renderer.font_state_mut().set_wordparts(parts);
                }
                Event::TextAnimation(params) => {
                    renderer.set_scetween(ScetweenConfig::from_params(params));
                }
                Event::SceneIn => renderer.show_text(),
                Event::SceneOut => renderer.hide_text(),
                // ── ruby / link ──
                Event::RubyStart { text } => renderer.ruby_start(text),
                Event::RubyEnd => renderer.ruby_end(),
                // 解释器已补齐 shadowcolor/outlinecolor 字段，直接透传。
                Event::LinkStart {
                    file,
                    label,
                    link_type,
                    color,
                    shadowcolor,
                    outlinecolor,
                } => renderer.link_start(
                    file.as_deref(),
                    label.as_deref(),
                    *link_type,
                    color.as_deref(),
                    shadowcolor.as_deref(),
                    outlinecolor.as_deref(),
                ),
                Event::LinkEnd => renderer.link_end(),
                Event::LinkEnable => renderer.set_links_enabled(true),
                Event::LinkDisable => renderer.set_links_enabled(false),
                // ── backlog ──
                // [backlog]：解释器已补齐 messagelayer/includefont/hide/layer/clear
                // 字段，在 BacklogSettings 上逐一落值（None=继承先前设置）。
                Event::BacklogConfig {
                    allow,
                    messagelayer,
                    includefont,
                    hide,
                    layer,
                    clear,
                } => {
                    let backlog = &mut renderer.font_state_mut().backlog;
                    backlog.settings.allow = *allow;
                    if let Some(ml) = messagelayer {
                        backlog.settings.message_layer = ml.clone();
                    }
                    if let Some(inc) = includefont {
                        backlog.settings.include_font = *inc;
                    }
                    if let Some(h) = hide {
                        backlog.settings.hide = h.clone();
                    }
                    // layer=None 表示禁用自动显示（文档：缺省则禁用），直接覆盖。
                    backlog.settings.layer = layer.clone();
                    if *clear {
                        backlog.clear();
                    }
                }
                // [writebacklog]：mode=1 换页存历史（rp 的 backlog 参数可逐次覆盖）
                Event::WriteBacklogConfig { mode } => {
                    renderer.font_state_mut().backlog.set_write_mode(*mode);
                }
                _ => {}
            }
            match event {
                Event::FontInit
                | Event::FontClose
                | Event::FontDefault(_)
                | Event::FontSettings(_)
                | Event::MessageLayerSwitch { .. }
                | Event::MessageLayerPop => renderer.active_font_face().map(str::to_string),
                _ => None,
            }
        };
        if let Some(face) = restored_face {
            self.load_script_font(&face);
        }

        // ── lyprop `~` 消息层绑定接线 ────────────────────────────────
        // 文本子系统创建/切换消息层时，把「消息层 ID → 场景图层 ID」登记进合成器，
        // 使 [lyprop id="~xxx"] / id="~" 能解析到对应场景图层。这里放在 renderer
        // 借用结束之后：切换后活动消息层由 renderer 决定，需回读它拿真实 ID。
        match event {
            Event::MessageLayerSwitch { .. } | Event::MessageLayerPop => {
                self.sync_message_layer_binding();
            }
            _ => {}
        }
        if let Some(request) = background_request.as_mut() {
            request.span = tracked_span;
        }
        background_request
    }

    pub(super) fn begin_text_translation(&mut self, pending: PendingScenarioText) {
        self.text_translation_serial = self.text_translation_serial.wrapping_add(1);
        let serial = self.text_translation_serial;
        self.pending_text_translations.insert(
            serial,
            PendingTextTranslation {
                span: pending.span,
                translated: None,
            },
        );
        crate::ffi::emit_ui_command(
            "text_translate",
            serde_json::json!({
                "serial": serial,
                "text": pending.source,
                "ruby": pending.ruby,
                "blocking": false,
            }),
        );
    }

    pub fn submit_text_translation(&mut self, serial: u64, translated: Option<&str>) -> bool {
        let Some(pending) = self.pending_text_translations.get_mut(&serial) else {
            crate::core_debug!("[translation] 忽略过期结果 serial={serial}");
            return false;
        };
        let Some(translated) = translated else {
            self.pending_text_translations.remove(&serial);
            return true;
        };
        pending.translated = Some(translated.to_string());
        true
    }

    /// 网络结果只在目标层逐字显示结束后落入字形缓冲，避免替换长度变化使
    /// reveal_index 跳跃。页面已切换的结果直接丢弃视觉更新，宿主缓存仍保留。
    pub(super) fn apply_ready_text_translations(&mut self) {
        let Some(renderer) = self.text_renderer.as_ref() else {
            self.pending_text_translations.clear();
            return;
        };
        let state = renderer.font_state();
        let mut expired = Vec::new();
        let mut ready = Vec::new();
        for (&serial, pending) in &self.pending_text_translations {
            if pending.translated.is_none() {
                continue;
            }
            let Some(span) = pending.span.as_ref() else {
                expired.push(serial);
                continue;
            };
            match text_span_ready(state, span) {
                Some(true) => ready.push(serial),
                Some(false) => {}
                None => expired.push(serial),
            }
        }
        for serial in expired {
            self.pending_text_translations.remove(&serial);
        }
        for serial in ready {
            self.apply_ready_text_translation(serial);
        }
    }

    fn apply_ready_text_translation(&mut self, serial: u64) {
        let Some(pending) = self.pending_text_translations.remove(&serial) else {
            return;
        };
        let (Some(text), Some(span), Some(renderer)) = (
            pending.translated,
            pending.span,
            self.text_renderer.as_mut(),
        ) else {
            return;
        };
        let old_end = span.end;
        let layer_id = span.layer_id.clone();
        let generation = span.generation;
        let Some(delta) = renderer.replace_text_span(&span, &self.text_inject.run(&text)) else {
            crate::core_debug!("[translation] 页面已变化，译文仅保留在宿主缓存 serial={serial}");
            return;
        };
        if delta != 0 {
            for pending in self.pending_text_translations.values_mut() {
                let Some(other) = pending.span.as_mut() else {
                    continue;
                };
                if other.layer_id == layer_id
                    && other.generation == generation
                    && other.start >= old_end
                {
                    other.start = other.start.saturating_add_signed(delta);
                    other.end = other.end.saturating_add_signed(delta);
                }
            }
        }
    }

    pub(super) fn clear_pending_text_translation(&mut self) {
        self.pending_text_translations.clear();
    }

    /// 把当前活动消息层登记为合成器的默认消息层，并建立「消息层 ID → 场景图层
    /// ID」映射。分层消息层绑定同名图像层；独立消息层绑定到内部顶层节点。
    pub(super) fn sync_message_layer_binding(&mut self) {
        let Some(renderer) = self.text_renderer.as_ref() else {
            return;
        };
        let state = renderer.font_state();
        let active = state.active_layer.as_ref().map(|id| {
            let layered = state.layers.get(id).is_some_and(|layer| layer.layered);
            (id.clone(), layered)
        });
        apply_message_layer_binding(&mut self.compositor, active);
    }

    // ── glyph 点击等待图标接线 ───────────────────────────────────────
    //
    // 进入行末/页末点击等待时把等待图标图层移动到最后一个字符旁并显示；
    // 退出等待时隐藏。位置由文本子系统的 click_wait_icon_placement 计算，
    // 显隐由合成器的 show/hide_click_wait_icon 落到场景。

    /// 进入点击等待时显示等待图标。
    ///
    /// `page_end`=false 为行末等待（用 glyph 的 layer + left/top），true 为页末
    /// 等待（用 rplayer + rpleft/rptop）。未配置图标图层或当前层无文本时不显示。
    pub(super) fn enter_click_wait_icon(&mut self, page_end: bool) {
        let placement = self
            .text_renderer
            .as_ref()
            .and_then(|renderer| renderer.click_wait_icon_placement(page_end));
        if let Some(p) = placement {
            self.compositor
                .show_click_wait_icon(&p.layer_id, p.left, p.top, p.homing);
        }
    }

    /// 退出点击等待时隐藏等待图标。
    pub(super) fn exit_click_wait_icon(&mut self) {
        self.compositor.hide_click_wait_icon();
    }

    fn load_script_font(&mut self, face: &str) {
        if self.loaded_font_face.as_deref() == Some(face) {
            return;
        }
        let Some(renderer) = self.text_renderer.as_mut() else {
            return;
        };
        let mut errors = Vec::new();
        for candidate in std::iter::once(face.to_string()).chain(font_fallback_candidates(face)) {
            match crate::load_font_ffi(&candidate).and_then(|bytes| renderer.set_font_bytes(bytes))
            {
                Ok(()) => {
                    if candidate == face {
                        crate::core_info!("[text] 已加载脚本字体: {face}");
                    } else {
                        crate::core_info!("[text] 字体回退: {face} -> {candidate}");
                    }
                    // 记录脚本请求的逻辑字体名，避免每次 [font] 都重复探测缺失实体。
                    self.loaded_font_face = Some(face.to_string());
                    return;
                }
                Err(error) => errors.push(format!("{candidate}: {error}")),
            }
        }
        crate::core_warn!("[text] 脚本字体加载失败 {face}: {}", errors.join("; "));
    }
}

pub(super) fn font_fallback_candidates(face: &str) -> Vec<String> {
    let (stem, extension) = face.rsplit_once('.').unwrap_or((face, ""));
    let lower = stem.to_ascii_lowercase();
    for separator in ['-', '_'] {
        let suffix = format!("{separator}medium");
        if lower.ends_with(&suffix) {
            let family = &stem[..stem.len() - suffix.len()];
            let extension = if extension.is_empty() {
                String::new()
            } else {
                format!(".{extension}")
            };
            return ["regular", "bold"]
                .into_iter()
                .map(|weight| format!("{family}{separator}{weight}{extension}"))
                .collect();
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::{
        BACKLOG_SNAPSHOT, BacklogSnapshot, backlog_snapshot, build_backlog_snapshot,
        font_fallback_candidates, text_span_ready,
    };
    use crate::text::GlyphTextRenderer;
    use crate::text::render::{FontState, TextRenderer, TextSpanToken};
    use std::collections::HashMap;

    #[test]
    fn medium_font_fallbacks_stay_in_the_same_family() {
        assert_eq!(
            font_fallback_candidates("font/sourcehansans-medium.otf"),
            vec![
                "font/sourcehansans-regular.otf",
                "font/sourcehansans-bold.otf"
            ]
        );
        assert_eq!(
            font_fallback_candidates("font/ui_medium.ttf"),
            vec!["font/ui_regular.ttf", "font/ui_bold.ttf"]
        );
        assert!(font_fallback_candidates("font/story.otf").is_empty());
    }

    #[test]
    fn async_translation_waits_for_reveal_and_expires_after_page_change() {
        let mut state = FontState::new();
        let layer = state.active_layer_mut();
        layer.reveal_pending = true;
        let span = TextSpanToken {
            layer_id: layer.id.clone(),
            generation: layer.generation,
            start: 0,
            end: 0,
            page_tag_index: 0,
            font_size: 40.0,
            font_face: None,
        };

        assert_eq!(text_span_ready(&state, &span), Some(false));
        state.active_layer_mut().reveal_pending = false;
        assert_eq!(text_span_ready(&state, &span), Some(true));
        state.active_layer_mut().clear_page();
        assert_eq!(text_span_ready(&state, &span), None);
    }

    #[test]
    fn snapshot_accessors_follow_pseudo_array_conventions() {
        let mut snap = BacklogSnapshot::default();
        snap.pages.push((
            vec!["[print data=\"页0\"]".to_string()],
            vec![
                "[font size=\"40\"]".to_string(),
                "[print data=\"页0\"]".to_string(),
            ],
        ));
        snap.message_layers.insert(
            "adv01".to_string(),
            (
                vec!["[print data=\"当前\"]".to_string()],
                vec![
                    "[font size=\"40\"]".to_string(),
                    "[print data=\"当前\"]".to_string(),
                ],
            ),
        );

        // get_backlog_size
        assert_eq!(snap.backlog_size(), 1);
        // get_backlog_tags：allfont=0/1 两套、越界 None
        assert_eq!(
            snap.backlog_tags(0, false).unwrap(),
            vec!["[print data=\"页0\"]"]
        );
        assert_eq!(snap.backlog_tags(0, true).unwrap().len(), 2);
        assert!(snap.backlog_tags(1, false).is_none());
        // get_message_tags：按 id 查、不存在 None
        assert_eq!(
            snap.message_tags("adv01", false).unwrap(),
            vec!["[print data=\"当前\"]"]
        );
        assert_eq!(snap.message_tags("adv01", true).unwrap().len(), 2);
        assert!(snap.message_tags("missing", false).is_none());
    }

    #[test]
    fn build_snapshot_extracts_backlog_pages_and_message_tags() {
        let mut r = GlyphTextRenderer::new();
        // 存两页历史（writebacklog mode=1 后连续换页）
        r.font_state_mut().backlog.set_write_mode(true);
        r.push_text("第一页", false);
        r.push_page_break(None);
        r.push_text("第二页", false);
        r.push_page_break(None);
        // 当前消息层再留一页未换页的文本，供 get_message_tags 抽取
        r.push_text("当前行", false);

        let snap = build_backlog_snapshot(r.font_state());

        // backlog：两页，页码即下标，与 get_backlog_tags 一致
        assert_eq!(snap.backlog_size(), 2);
        assert_eq!(
            snap.backlog_tags(0, false).unwrap(),
            vec!["[print data=\"第一页\"]"]
        );
        assert_eq!(
            snap.backlog_tags(1, false).unwrap(),
            vec!["[print data=\"第二页\"]"]
        );
        // 默认消息层当前文本再现标签
        let msg = snap
            .message_tags(crate::text::glyph::DEFAULT_MESSAGE_LAYER, false)
            .unwrap();
        assert_eq!(msg, vec!["[print data=\"当前行\"]"]);
    }

    #[test]
    fn build_snapshot_allfont_prepends_page_font() {
        let mut r = GlyphTextRenderer::new();
        r.font_default(&HashMap::from([("size".to_string(), "40".to_string())]));
        r.push_text("あ", false);

        let snap = build_backlog_snapshot(r.font_state());
        let with_font = snap
            .message_tags(crate::text::glyph::DEFAULT_MESSAGE_LAYER, true)
            .unwrap();
        // allfont=1 时以页首字体的 [font …] 开头
        assert_eq!(with_font[0], "[font size=\"40\"]");
        // allfont=0 时不含字体标签
        let no_font = snap
            .message_tags(crate::text::glyph::DEFAULT_MESSAGE_LAYER, false)
            .unwrap();
        assert_eq!(no_font, vec!["[print data=\"あ\"]"]);
    }

    #[test]
    fn snapshot_static_round_trips() {
        // 直接写进程级快照再读回，验证 backlog_snapshot() 访问路径（宿主钩子读取入口）。
        let mut snap = BacklogSnapshot::default();
        snap.pages
            .push((vec!["[print data=\"x\"]".to_string()], Vec::new()));
        *BACKLOG_SNAPSHOT.lock().unwrap() = snap.clone();
        assert_eq!(backlog_snapshot(), snap);
        // 复位，避免污染其它测试（进程级静态共享）
        *BACKLOG_SNAPSHOT.lock().unwrap() = BacklogSnapshot::default();
    }

    // ── 任务 #3：lyprop `~` 消息层绑定 ──

    #[test]
    fn message_layer_binding_registers_active_layer_and_resolves_tilde() {
        use super::apply_message_layer_binding;
        use crate::compositor::Compositor;
        use asb_interpreter::Event;
        use asb_interpreter::event::LayerEvent;

        let mut c = Compositor::new();
        // 场景图层与消息层同名（apply_text_event 的 ensure_layer 语义）
        c.apply_event(&Event::Layer(LayerEvent::Create {
            id: "mw".into(),
            file: "mw_bg".into(),
        }));

        // 切到消息层 mw 后接线绑定（等价 sync_message_layer_binding 读到 active="mw"）
        apply_message_layer_binding(&mut c, Some(("mw".to_string(), true)));

        // `~mw` 应解析到场景图层 mw
        c.apply_event(&Event::Layer(LayerEvent::SetProperty {
            id: "~mw".into(),
            property: "alpha".into(),
            value: "100".into(),
        }));
        assert_eq!(c.scene().get("mw").unwrap().props.alpha, Some(100));
        // `~`（默认消息层）也应指向 mw
        c.apply_event(&Event::Layer(LayerEvent::SetProperty {
            id: "~".into(),
            property: "left".into(),
            value: "42".into(),
        }));
        assert_eq!(c.scene().get("mw").unwrap().props.left, Some(42.0));
    }

    #[test]
    fn independent_message_layer_uses_overlay_scene_node() {
        use super::{apply_message_layer_binding, message_layer_scene_id};
        use crate::compositor::Compositor;
        use asb_interpreter::Event;
        use asb_interpreter::event::LayerEvent;

        let mut c = Compositor::new();
        apply_message_layer_binding(&mut c, Some(("1.80.mw.adv".to_string(), false)));
        let overlay = message_layer_scene_id("1.80.mw.adv", false);

        assert!(c.scene().get(&overlay).is_some());
        assert!(c.scene().get("1.80").is_none());

        c.apply_event(&Event::Layer(LayerEvent::SetProperty {
            id: "~".into(),
            property: "visible".into(),
            value: "0".into(),
        }));
        assert_eq!(c.scene().get(&overlay).unwrap().props.visible, Some(false));
    }

    #[test]
    fn message_layer_binding_none_clears_default() {
        use super::apply_message_layer_binding;
        use crate::compositor::Compositor;
        use asb_interpreter::Event;
        use asb_interpreter::event::LayerEvent;

        let mut c = Compositor::new();
        c.apply_event(&Event::Layer(LayerEvent::Create {
            id: "mw".into(),
            file: "mw_bg".into(),
        }));
        apply_message_layer_binding(&mut c, Some(("mw".to_string(), true)));
        // 弹空活动消息层：清默认消息层，`~` 此后无目标（合成器忽略该操作）
        apply_message_layer_binding(&mut c, None);
        c.apply_event(&Event::Layer(LayerEvent::SetProperty {
            id: "~".into(),
            property: "left".into(),
            value: "99".into(),
        }));
        // 默认消息层已清空，left 不应被改动
        assert_ne!(c.scene().get("mw").unwrap().props.left, Some(99.0));
    }

    #[test]
    fn switch_message_layer_exposes_active_id_for_binding() {
        // 验证接线依赖的数据流：switch 后 font_state().active_layer 即目标消息层 ID。
        let mut r = GlyphTextRenderer::new();
        r.switch_message_layer(Some("mw"), true);
        assert_eq!(r.font_state().active_layer.as_deref(), Some("mw"));
    }

    // ── 任务 #2：glyph 点击等待图标 ──

    #[test]
    fn click_wait_placement_feeds_compositor_show_and_hide() {
        use crate::compositor::Compositor;
        use crate::render_pipeline::draw::TextureId;
        use crate::text::render::GlyphInfo;
        use asb_interpreter::Event;
        use asb_interpreter::event::LayerEvent;

        // 等宽字形（宽/步进 10），无字体时 push_text 不产字形，故直接注入缓冲。
        fn glyph(c: char) -> GlyphInfo {
            GlyphInfo {
                character: c.to_string(),
                texture_id: TextureId(0),
                atlas_x: 0.0,
                atlas_y: 0.0,
                atlas_w: 0.0,
                atlas_h: 0.0,
                offset_x: 0.0,
                offset_y: 0.0,
                width: 10.0,
                height: 0.0,
                advance_x: 10.0,
            }
        }

        let mut r = GlyphTextRenderer::new();
        // 配置行末图标图层为 "90"，无偏移、homing=1
        r.set_glyph_config(&HashMap::from([
            ("layer".to_string(), "90".to_string()),
            ("homing".to_string(), "1".to_string()),
        ]));
        {
            let layer = r.font_state_mut().active_layer_mut();
            layer.left = 100.0;
            layer.top = 200.0;
            layer.text_buffer = vec![glyph('あ')];
        }

        // 行末等待（page_end=false）应得到摆放信息
        let placement = r
            .click_wait_icon_placement(false)
            .expect("配置了 layer 且有文本，应返回摆放信息");
        assert_eq!(placement.layer_id, "90");
        assert!(placement.homing);

        // 把摆放信息喂给合成器（等价 enter_click_wait_icon）
        let mut c = Compositor::new();
        c.apply_event(&Event::Layer(LayerEvent::Create {
            id: "90".into(),
            file: "icon".into(),
        }));
        c.show_click_wait_icon(
            &placement.layer_id,
            placement.left,
            placement.top,
            placement.homing,
        );
        assert_eq!(c.active_wait_icon(), Some("90"));
        assert_eq!(c.scene().get("90").unwrap().props.visible, Some(true));

        // 退出等待隐藏（等价 exit_click_wait_icon）
        c.hide_click_wait_icon();
        assert_eq!(c.active_wait_icon(), None);
        assert_eq!(c.scene().get("90").unwrap().props.visible, Some(false));
    }

    #[test]
    fn click_wait_placement_none_without_glyph_layer() {
        // [glyph] 未配置图标图层：即使有文本也不应返回摆放信息（不显示图标）。
        let mut r = GlyphTextRenderer::new();
        r.push_text("あ", false);
        assert!(r.click_wait_icon_placement(false).is_none());
    }
}
