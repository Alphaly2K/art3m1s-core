//! 事件归约：把解释器的 [`Event`] 流应用到合成器状态上。
//!
//! [`Compositor`] 持有一棵 [`Scene`] 和一个合成器时钟。它消费解释器在 `run` 过程
//! 中通过回调发出的 `Event`，把与画面相关的变体（图层增删改、缓动、转场）落到
//! 场景树上；与画面无关的变体（音频、存档、文本…）忽略，留给引擎别的子系统。
//!
//! 时间推进与渲染分离：解释器只管"发生了什么"，宿主把合成器状态交给顶层
//! [`crate::render_pipeline::RenderPipeline`] 进入后续渲染管线。

use crate::compositor::anim::{self, AnimeState, TweenHandler};
use crate::compositor::events::{CompositorEvent, IntoCompositorEvent};
use crate::compositor::lyedit::{self, LayerEditQueue, LayerEditRequest};
use crate::compositor::scene::{LayerEventHandler, Scene};
use crate::render_pipeline::draw::TextureProvider;
use crate::render_pipeline::transition::{self, TransitionState};
use asb_interpreter::event::LayerEvent;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

mod hit_test;

/// 已注册的输入事件处理器。
///
/// 由 Lua 脚本通过 `e:tag{"setonpush", key=..., handler="calllua", function="..."}`
/// 之类的 seton* 标签注册。引擎在检测到相应输入时把它交还解释器执行，自身不解释
/// handler/function 的含义——与 [`LayerEventHandler`] 同构。
#[derive(Debug, Clone, Default)]
pub struct InputHandler {
    /// 命中时先就地执行的标签名（如 `"calllua"`）。
    pub handler: Option<String>,
    /// 跳转/调用目标脚本文件。
    pub file: Option<String>,
    /// 跳转/调用目标标签。
    pub label: Option<String>,
    /// call=1 时压调用栈（对应 call 标签），否则等同 jump。
    pub call: bool,
    /// 标签里除已知字段外的所有参数（function、key、adv、ui、btn 等），
    /// 触发时原样塞进 handler 标签的参数表。
    pub params: HashMap<String, String>,
    /// 注册事件时的完整标签参数，供 `e:setEventFilter` 原样检查。
    pub filter_params: HashMap<String, String>,
}

fn complete_event_filter_params(
    extra_params: &HashMap<String, String>,
    fields: &[(&str, Option<&str>)],
    flags: &[(&str, bool)],
) -> HashMap<String, String> {
    let mut params = extra_params.clone();
    for (name, value) in fields {
        if let Some(value) = value
            && !value.is_empty()
        {
            params.insert((*name).to_string(), (*value).to_string());
        }
    }
    for (name, enabled) in flags {
        if *enabled {
            params.insert((*name).to_string(), "1".to_string());
        }
    }
    params
}

/// `[tweenset]` 组内暂存的一条 lytween 请求（owned，等 `[/tweenset]` 统一启动）。
#[derive(Debug, Clone, Default)]
struct PendingSetTween {
    id: String,
    param: String,
    from: Option<String>,
    to: Option<String>,
    ease: Option<String>,
    time: Option<u64>,
    delay: Option<u64>,
    loop_count: Option<i32>,
    yoyo: Option<i32>,
    loop_delay: Option<u64>,
    sync: bool,
    delete: bool,
    handler_file: Option<String>,
    handler_label: Option<String>,
    handler_handler: Option<String>,
}

/// 后端无关的合成器：场景树 + 时钟 + 事件归约。
pub struct Compositor {
    pub(crate) scene: Scene,
    /// 合成器时钟（毫秒），缓动与转场都基于它。
    pub(crate) clock_ms: u64,
    /// 舞台到物理像素的缩放因子（HiDPI）。
    pub(super) stage_scale: f32,
    /// 输入事件处理器注册表，按 (event_name, key) 索引。
    pub(super) input_handlers: HashMap<(String, String), InputHandler>,
    /// 自上次 `poll_tween_events` 以来产生待处理的缓动完成事件。
    pub(super) pending_tween_events: Vec<TweenHandler>,
    /// `[trans]` 转场状态（交叉淡化等）。
    pub(crate) trans_state: RefCell<Option<TransitionState>>,
    /// `[anime]` 帧动画状态，按图层 ID 索引。
    pub(super) anime_states: HashMap<String, AnimeState>,
    /// `[tweenset]` 收集态：Some 表示正在收集组内 lytween。
    tween_set_pending: Option<Vec<PendingSetTween>>,
    /// Tween 集编号分配器（1 起）。
    next_tween_set_id: u64,
    /// `[lyedit]` 排队与结果状态。渲染管线在帧构建前借 provider 处理，
    /// 因此用 RefCell 允许经 `&Compositor` 内部可变。
    pub(crate) layer_edits: RefCell<LayerEditQueue>,
    /// `~消息层ID` → 场景图层 ID 的绑定表（`[lyprop id="~xxx"]` 解析用）。
    message_layer_bindings: HashMap<String, String>,
    /// 因逻辑父图层被删除而失效的消息层。
    ///
    /// `/chgmsg` 弹栈可能恢复旧 ID，但不能借此复活已随父层删除的文字；
    /// 只有显式 `chgmsg` 再次切到该层时才清除此集合中的记录。
    deleted_message_layers: HashSet<String>,
    /// 默认消息图层的消息层 ID（`[lyprop id="~"]` 解析用）。
    default_message_layer: Option<String>,
    /// 当前显示中的点击等待图标图层 ID（`[glyph]`）。
    active_wait_icon: Option<String>,
}

impl std::fmt::Debug for Compositor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compositor")
            .field("scene", &self.scene)
            .field("clock_ms", &self.clock_ms)
            .field("stage_scale", &self.stage_scale)
            .field("input_handlers", &self.input_handlers)
            .field("pending_tween_events", &self.pending_tween_events)
            .finish()
    }
}

impl Default for Compositor {
    fn default() -> Self {
        Self {
            scene: Scene::new(),
            clock_ms: 0,
            stage_scale: 1.0,
            input_handlers: HashMap::new(),
            pending_tween_events: Vec::new(),
            trans_state: RefCell::new(None),
            anime_states: HashMap::new(),
            tween_set_pending: None,
            next_tween_set_id: 1,
            layer_edits: RefCell::new(LayerEditQueue::default()),
            message_layer_bindings: HashMap::new(),
            deleted_message_layers: HashSet::new(),
            default_message_layer: None,
            active_wait_icon: None,
        }
    }
}

impl Compositor {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Compositor {
    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    pub fn scene_snapshot(&self) -> Scene {
        self.scene.clone()
    }

    pub fn ensure_layer(&mut self, id: &str) {
        self.scene.ensure(id);
    }

    pub fn set_layer_file(&mut self, id: &str, file: Option<String>) {
        self.scene.set_file(id, file);
    }

    pub fn clear_layer_file_if_matches(&mut self, id: &str, expected: &str) {
        self.scene.clear_file_if_matches(id, expected);
    }

    pub fn restore_scene(&mut self, scene: Scene) {
        self.scene.replace_with(scene);
        self.pending_tween_events.clear();
    }

    /// 读档是全局状态切换边界：旧画面和旧 UI 输入处理器不能穿透到新存档。
    /// 随后若存档携带 scene，调用方再用 `restore_scene` 覆盖。
    pub fn reset_for_load(&mut self) {
        self.scene.replace_with(Scene::new());
        self.input_handlers.clear();
        self.pending_tween_events.clear();
        *self.trans_state.borrow_mut() = None;
        self.anime_states.clear();
        self.tween_set_pending = None;
        self.layer_edits.borrow_mut().clear();
        self.message_layer_bindings.clear();
        self.deleted_message_layers.clear();
        self.default_message_layer = None;
        self.active_wait_icon = None;
    }

    pub fn clock_ms(&self) -> u64 {
        self.clock_ms
    }

    /// 设置舞台缩放因子（HiDPI scale）。宿主在窗口初始化/缩放变化时调用。
    pub fn set_stage_scale(&mut self, scale: f32) {
        self.stage_scale = scale;
    }

    pub fn stage_scale(&self) -> f32 {
        self.stage_scale
    }

    /// 查询指定事件/键组合的已注册处理器。宿主在检测到输入后调用。
    pub fn get_input_handler(&self, event_name: &str, key: &str) -> Option<&InputHandler> {
        self.input_handlers
            .get(&(event_name.to_string(), key.to_string()))
    }

    pub fn layer_offset(&self, id: &str) -> Option<(f32, f32)> {
        self.scene.get(id).map(|layer| layer.props.offset())
    }

    pub fn is_layer_draggable(&self, id: &str) -> bool {
        self.scene
            .get(id)
            .and_then(|layer| layer.props.custom.get("draggable"))
            .map(|value| !matches!(value.trim(), "" | "0" | "off" | "false"))
            .unwrap_or(false)
    }

    pub fn drag_layer_to(
        &mut self,
        id: &str,
        origin_left: f32,
        origin_top: f32,
        delta_x: f32,
        delta_y: f32,
    ) -> Option<(f32, f32)> {
        let layer = self.scene.get(id)?;
        let (min_x, min_y, max_x, max_y) = layer
            .props
            .custom
            .get("dragarea")
            .and_then(|value| parse_drag_area(value))
            .unwrap_or((f32::MIN, f32::MIN, f32::MAX, f32::MAX));

        let left = (origin_left + delta_x).clamp(min_x, max_x);
        let top = (origin_top + delta_y).clamp(min_y, max_y);

        let mut raw = HashMap::new();
        raw.insert("left".to_string(), trim_float(left));
        raw.insert("top".to_string(), trim_float(top));
        self.scene.set_props(id, &raw);
        Some((left, top))
    }

    /// 推进合成器时钟。宿主每帧用累计的真实时间调用一次。
    pub fn advance(&mut self, delta_ms: u64) {
        self.clock_ms = self.clock_ms.saturating_add(delta_ms);

        transition::clear_finished(&self.trans_state, self.clock_ms);
        anim::gc_finished_tweens(
            &mut self.scene,
            self.clock_ms,
            &mut self.pending_tween_events,
        );
        anim::update_anime_frames(&mut self.scene, &mut self.anime_states, self.clock_ms);
    }
    ///
    /// 宿主在每帧 `advance` 之后调用，将返回到的 [`TweenHandler`] 交回解释器
    /// 执行（对于 `sync` 缓动则构造 Wait 事件暂停脚本）。
    pub fn poll_tween_events(&mut self) -> Vec<TweenHandler> {
        std::mem::take(&mut self.pending_tween_events)
    }

    // ── [lyedit] 像素加工 ──────────────────────────────────────

    /// 处理排队中的 `[lyedit]` 请求（渲染管线在帧构建前调用，需要 provider）。
    pub fn process_layer_edits(&self, provider: &mut dyn TextureProvider) {
        let mut queue = self.layer_edits.borrow_mut();
        lyedit::process_pending(&self.scene, &mut queue, provider);
    }

    /// 当前有效的"图层 ID → lyedit 加工后纹理名"重定向表（帧构建用）。
    pub(crate) fn layer_edit_overrides(&self) -> HashMap<String, String> {
        self.layer_edits.borrow().valid_overrides(&self.scene)
    }

    // ── [trans] input 跳过 ──────────────────────────────────────

    /// 用户输入请求跳过进行中的转场（`[trans]` input 参数语义）。
    /// `in_skip_mode` = 引擎当前是否处于跳过状态（input=2 用）。
    /// 返回是否真的清除了转场。
    pub fn skip_transition_by_input(&self, in_skip_mode: bool) -> bool {
        transition::skip_by_input(&self.trans_state, in_skip_mode)
    }

    // ── [lyc] 单色/蒙版图层 ─────────────────────────────────────

    /// 把图层设为 `lyc` 单色模式。`color` 为 `RRGGBB` 或 `AARRGGBB`。
    /// 宽高经 lyprop 兼容的 width/height 属性设置。解析失败返回 `false`。
    pub fn set_layer_solid_color(&mut self, id: &str, color: &str) -> bool {
        match crate::compositor::props::parse_hex_color(color) {
            Some([a, r, g, b]) => {
                self.scene.set_solid_color(id, Some([r, g, b, a]));
                true
            }
            None => false,
        }
    }

    /// 设置/清除图层的 `lyc` 蒙版图路径。
    pub fn set_layer_mask(&mut self, id: &str, mask: Option<&str>) {
        self.scene.set_mask(id, mask.map(str::to_string));
    }

    // ── [lyprop] `~` 消息图层前缀的绑定 ─────────────────────────

    /// 登记消息层 ID → 场景图层 ID 的映射（文本子系统在创建消息层时调用）。
    pub fn set_message_layer_binding(&mut self, message_id: &str, scene_layer_id: &str) {
        self.message_layer_bindings
            .insert(message_id.to_string(), scene_layer_id.to_string());
    }

    /// 显式 `chgmsg` 重新选择消息层时，使其脱离父层删除产生的失效状态。
    pub fn revive_message_layer(&mut self, message_id: &str) {
        self.deleted_message_layers.remove(message_id);
    }

    /// 设置默认消息图层的消息层 ID（`[lyprop id="~"]` 解析用）。
    pub fn set_default_message_layer(&mut self, message_id: Option<String>) {
        self.default_message_layer = message_id;
    }

    /// 解析图层事件里的特殊 ID 形式：
    /// - `~xxx` → 消息层绑定表映射到场景图层 ID（未登记时按 `xxx` 直查场景）；
    /// - `~` → 默认消息图层（未设置时返回 `None`，调用方忽略本次操作）；
    /// - 其余原样返回。`!`（根图层）由调用方在此之前单独分派。
    fn resolve_layer_target(&self, raw: &str) -> Option<String> {
        let Some(rest) = raw.strip_prefix('~') else {
            return Some(raw.to_string());
        };
        let message_id = if rest.is_empty() {
            match &self.default_message_layer {
                Some(id) => id.clone(),
                None => {
                    crate::core_warn!("[lyprop] id=\"~\" 但未设置默认消息图层，忽略");
                    return None;
                }
            }
        } else {
            rest.to_string()
        };
        Some(
            self.message_layer_bindings
                .get(&message_id)
                .cloned()
                .unwrap_or(message_id),
        )
    }

    /// 消息层当前是否有效可见（用于 link 命中检测）。
    ///
    /// `message_id` 是文本子系统的消息层 ID（如 `adv01` 或 `1.80.mw.adv_adv`）。
    /// 先经绑定表解析到场景图层 ID，再查其祖先感知可见性——mw 隐藏（自身或父层
    /// visible=0）后返回 false，使已隐藏文本区的链接命中失效。
    /// 未登记绑定时按 message_id 直查场景。
    pub fn is_message_layer_visible(&self, message_id: &str) -> bool {
        if self.deleted_message_layers.contains(message_id) {
            return false;
        }
        let scene_id = self
            .message_layer_bindings
            .get(message_id)
            .map(String::as_str)
            .unwrap_or(message_id);
        self.scene.is_effectively_visible(scene_id)
            && (scene_id == message_id || self.scene.existing_path_is_visible(message_id))
    }

    /// 删除逻辑图层子树时，同步移除挂在内部顶层节点上的独立消息层。
    ///
    /// 独立消息层为了保证绘制顺序不直接作为逻辑父层的场景子节点，因此普通的
    /// `Scene::delete` 无法级联到它们。绑定的逻辑 ID 仍遵循点分层级，需在这里
    /// 按逻辑子树清理，否则 `Delete 1` 后旧剧情文字会残留在后续转场中。
    fn remove_message_layers_in_subtree(&mut self, deleted_id: &str) {
        let removed: Vec<(String, String)> = self
            .message_layer_bindings
            .iter()
            .filter(|(message_id, scene_id)| {
                layer_id_is_same_or_descendant(message_id, deleted_id)
                    || layer_id_is_same_or_descendant(scene_id, deleted_id)
            })
            .map(|(message_id, scene_id)| (message_id.clone(), scene_id.clone()))
            .collect();

        for (message_id, scene_id) in &removed {
            self.message_layer_bindings.remove(message_id);
            self.deleted_message_layers.insert(message_id.clone());
            if !self
                .message_layer_bindings
                .values()
                .any(|remaining| remaining == scene_id)
            {
                self.scene.delete(scene_id);
            }
        }

        if self.default_message_layer.as_deref().is_some_and(|id| {
            layer_id_is_same_or_descendant(id, deleted_id)
                || removed.iter().any(|(message_id, _)| message_id == id)
        }) {
            self.default_message_layer = None;
        }
    }

    // ── [glyph] 点击等待图标 ────────────────────────────────────

    /// 进入点击等待时显示等待图标图层。
    ///
    /// `left`/`top` 为最后一个字符位置加 glyph 偏移后的目标坐标（由文本子系统的
    /// `click_wait_icon_placement` 计算）；`homing=false` 时只切可见性、不动坐标。
    pub fn show_click_wait_icon(&mut self, layer_id: &str, left: f32, top: f32, homing: bool) {
        if let Some(prev) = self.active_wait_icon.take()
            && prev != layer_id
        {
            let raw = HashMap::from([("visible".to_string(), "0".to_string())]);
            self.scene.set_props(&prev, &raw);
        }
        let mut raw = HashMap::from([("visible".to_string(), "1".to_string())]);
        if homing {
            raw.insert("left".to_string(), left.to_string());
            raw.insert("top".to_string(), top.to_string());
        }
        self.scene.set_props(layer_id, &raw);
        self.active_wait_icon = Some(layer_id.to_string());
    }

    /// 离开点击等待（用户点击继续/换页完成）时隐藏等待图标图层。
    pub fn hide_click_wait_icon(&mut self) {
        if let Some(id) = self.active_wait_icon.take() {
            let raw = HashMap::from([("visible".to_string(), "0".to_string())]);
            self.scene.set_props(&id, &raw);
        }
    }

    /// 当前显示中的等待图标图层 ID（测试/查询用）。
    pub fn active_wait_icon(&self) -> Option<&str> {
        self.active_wait_icon.as_deref()
    }

    /// 把一个视觉/交互事件应用到场景上。
    pub fn apply_event<'a>(&mut self, event: impl IntoCompositorEvent<'a>) {
        let Some(event) = event.into_compositor_event() else {
            return;
        };
        self.apply_compositor_event(event);
    }

    fn apply_compositor_event(&mut self, event: CompositorEvent<'_>) {
        match event {
            CompositorEvent::Layer(layer_event) => self.apply_layer_event(layer_event),
            CompositorEvent::LayerRename { id, to } => {
                self.scene.rename(id, to);
            }
            CompositorEvent::LayerTween {
                id,
                param,
                from,
                to,
                ease,
                time,
                delay,
                loop_count,
                yoyo,
                loop_delay,
                sync,
                delete,
                handler_file,
                handler_label,
                handler_handler,
            } => {
                // [tweenset] 收集中：先入队，等 [/tweenset] 统一按顺序启动。
                if let Some(pending) = self.tween_set_pending.as_mut() {
                    pending.push(PendingSetTween {
                        id: id.to_string(),
                        param: param.to_string(),
                        from: from.map(str::to_string),
                        to: to.map(str::to_string),
                        ease: ease.map(str::to_string),
                        time,
                        delay,
                        loop_count,
                        yoyo,
                        loop_delay,
                        sync,
                        delete,
                        handler_file: handler_file.map(str::to_string),
                        handler_label: handler_label.map(str::to_string),
                        handler_handler: handler_handler.map(str::to_string),
                    });
                    return;
                }
                anim::apply_tween(
                    &mut self.scene,
                    self.clock_ms,
                    anim::TweenRequest {
                        id,
                        param,
                        from,
                        to,
                        ease,
                        time,
                        delay,
                        loop_count,
                        yoyo,
                        loop_delay,
                        sync,
                        delete,
                        handler_file,
                        handler_label,
                        handler_handler,
                        set_id: None,
                    },
                )
            }
            CompositorEvent::LayerTweenDelete { id } => {
                // 强制完成：把该图层所有缓动直接落到终值并清空。
                // （同组 tweenset 的其余缓动在 finish_tweens 内级联删除。）
                anim::finish_tweens(&mut self.scene, id);
            }
            // ── [tweenset] ... [/tweenset] ──
            CompositorEvent::TweenSetStart => {
                self.tween_set_pending = Some(Vec::new());
            }
            CompositorEvent::TweenSetEnd => {
                let Some(pending) = self.tween_set_pending.take() else {
                    return;
                };
                if pending.is_empty() {
                    return;
                }
                let set_id = self.next_tween_set_id;
                self.next_tween_set_id += 1;
                // 顺序执行：每条的启动时刻 = 前面所有条目 (delay + time) 的累计。
                let mut offset_ms: u64 = 0;
                for item in &pending {
                    let start_delay = offset_ms + item.delay.unwrap_or(0);
                    anim::apply_tween(
                        &mut self.scene,
                        self.clock_ms,
                        anim::TweenRequest {
                            id: &item.id,
                            param: &item.param,
                            from: item.from.as_deref(),
                            to: item.to.as_deref(),
                            ease: item.ease.as_deref(),
                            time: item.time,
                            delay: Some(start_delay),
                            loop_count: item.loop_count,
                            yoyo: item.yoyo,
                            loop_delay: item.loop_delay,
                            sync: item.sync,
                            delete: item.delete,
                            handler_file: item.handler_file.as_deref(),
                            handler_label: item.handler_label.as_deref(),
                            handler_handler: item.handler_handler.as_deref(),
                            set_id: Some(set_id),
                        },
                    );
                    offset_ms = start_delay + item.time.unwrap_or(0);
                }
            }
            // ── [lyedit] 像素加工：排队，帧构建前由渲染管线处理 ──
            CompositorEvent::LayerEdit {
                id,
                mode,
                color,
                file,
                left,
                top,
            } => {
                self.layer_edits
                    .borrow_mut()
                    .pending
                    .push(LayerEditRequest {
                        id: id.to_string(),
                        mode: mode.to_string(),
                        color: color.map(str::to_string),
                        file: file.map(str::to_string),
                        left: left.unwrap_or(0),
                        top: top.unwrap_or(0),
                    });
            }
            CompositorEvent::LayerEventHandler {
                id,
                event_type,
                mode,
                file,
                label,
                call,
                handler,
                penetration,
                extra_params,
            } => {
                // disable/enable 只切换已有处理器，不能丢掉 init 时注册的
                // key/function 等参数。HENPRI 会在 enable 时省略 key，依赖引擎
                // 恢复原处理器。
                self.scene.ensure(id);
                if let Some(layer) = self.scene.get_mut(id) {
                    match mode {
                        "reset" => {
                            layer.event_handlers.remove(event_type);
                        }
                        "disable" => {
                            if let Some(existing) = layer.event_handlers.get_mut(event_type) {
                                existing.enabled = false;
                            }
                        }
                        "enable" => {
                            if let Some(existing) = layer.event_handlers.get_mut(event_type) {
                                existing.enabled = true;
                            } else {
                                let filter_params = complete_event_filter_params(
                                    extra_params,
                                    &[
                                        ("id", Some(id)),
                                        ("type", Some(event_type)),
                                        ("mode", Some(mode)),
                                        ("file", file),
                                        ("label", label),
                                        ("handler", handler),
                                    ],
                                    &[("call", call), ("penetration", penetration)],
                                );
                                layer.event_handlers.insert(
                                    event_type.to_string(),
                                    LayerEventHandler {
                                        enabled: true,
                                        handler: handler.map(str::to_string),
                                        file: file.map(str::to_string),
                                        label: label.map(str::to_string),
                                        call,
                                        penetration,
                                        params: extra_params.clone(),
                                        filter_params,
                                    },
                                );
                            }
                        }
                        _ => {
                            let filter_params = complete_event_filter_params(
                                extra_params,
                                &[
                                    ("id", Some(id)),
                                    ("type", Some(event_type)),
                                    ("mode", Some(mode)),
                                    ("file", file),
                                    ("label", label),
                                    ("handler", handler),
                                ],
                                &[("call", call), ("penetration", penetration)],
                            );
                            layer.event_handlers.insert(
                                event_type.to_string(),
                                LayerEventHandler {
                                    enabled: true,
                                    handler: handler.map(str::to_string),
                                    file: file.map(str::to_string),
                                    label: label.map(str::to_string),
                                    call,
                                    penetration,
                                    params: extra_params.clone(),
                                    filter_params,
                                },
                            );
                        }
                    }
                }
            }
            // 输入事件处理器注册（setonpush 等 seton* 标签）。
            CompositorEvent::SetInputHandler {
                event_name,
                file,
                label,
                call,
                handler,
                extra_params,
            } => {
                // key 字段标识处理器响应的按键/输入（"1" = 鼠标左键）。
                // 引擎按 (event_name, key) 索引，不解释 handler/function 的语义。
                let key = extra_params.get("key").cloned().unwrap_or_default();
                let filter_params = complete_event_filter_params(
                    extra_params,
                    &[("file", file), ("label", label), ("handler", handler)],
                    &[("call", call)],
                );
                self.input_handlers.insert(
                    (event_name.to_string(), key),
                    InputHandler {
                        handler: handler.map(str::to_string),
                        file: file.map(str::to_string),
                        label: label.map(str::to_string),
                        call,
                        params: extra_params.clone(),
                        filter_params,
                    },
                );
            }
            CompositorEvent::DelInputHandler { event_name, key } => {
                if let Some(key) = key {
                    self.input_handlers
                        .remove(&(event_name.to_string(), key.to_string()));
                } else {
                    self.input_handlers
                        .retain(|(name, _), _| name != event_name);
                }
            }
            // ── 帧动画 ──
            CompositorEvent::Anime {
                id,
                mode,
                file,
                mask,
                time,
                loop_count,
                props,
            } => anim::apply_anime_event(
                &mut self.scene,
                &mut self.anime_states,
                self.clock_ms,
                anim::AnimeRequest {
                    id,
                    mode,
                    file,
                    mask,
                    time,
                    loop_count,
                    props,
                },
            ),
            // ── 转场 ──
            CompositorEvent::Trans {
                trans_type,
                time,
                rule,
                vague,
                input,
            } => {
                transition::start(
                    &self.trans_state,
                    self.clock_ms,
                    transition::TransitionRequest {
                        trans_type,
                        time,
                        rule,
                        vague,
                        input,
                    },
                );
            }
            // ── Flip 即刻提交 ──
            CompositorEvent::Flip => {
                transition::clear(&self.trans_state);
            }
        }
    }

    fn apply_layer_event(&mut self, event: &LayerEvent) {
        match event {
            LayerEvent::Create { id, file } => {
                self.scene.create(id, Some(file.clone()));
            }
            LayerEvent::Create2 { id, file, alpha } => {
                self.scene.create(id, Some(file.clone()));
                if let Some(alpha) = alpha {
                    let mut raw = HashMap::new();
                    raw.insert("alpha".to_string(), alpha.to_string());
                    self.scene.set_props(id, &raw);
                }
            }
            LayerEvent::Delete { id } => {
                let Some(id) = self.resolve_layer_target(id) else {
                    return;
                };
                // tweenset 级联：先收集待删子树里涉及的 Tween 集，删除子树后
                // 把同组散落在其他图层上的缓动一并清掉（tweenset.md）。
                let set_ids: HashSet<u64> = self
                    .scene
                    .subtree_ids(&id)
                    .iter()
                    .filter_map(|node_id| self.scene.get(node_id))
                    .flat_map(|layer| layer.tweens.iter().filter_map(|t| t.set_id))
                    .collect();
                self.scene.delete(&id);
                self.remove_message_layers_in_subtree(&id);
                anim::remove_tween_sets(&mut self.scene, &set_ids);
                // 图层没了，对应的 lyedit 加工态也随之作废。
                self.layer_edits.borrow_mut().states.remove(&id);
            }
            LayerEvent::SetProperty {
                id,
                property,
                value,
            } => {
                let mut raw = HashMap::new();
                raw.insert(property.clone(), value.clone());
                self.set_layer_props_routed(id, &raw);
            }
            LayerEvent::SetProperties { id, properties } => {
                self.set_layer_props_routed(id, properties);
            }
        }
    }

    /// 属性设置的统一入口：分派 `!`（根图层）与 `~`（消息图层）特殊 ID，
    /// 并拦截 `color`/`mask` 两个 lyc 专属键落到图层的单色/蒙版字段。
    fn set_layer_props_routed(&mut self, raw_id: &str, raw: &HashMap<String, String>) {
        // `!`：包含所有图层的根图层。
        if raw_id == "!" {
            self.scene.set_root_props(raw);
            return;
        }
        let Some(id) = self.resolve_layer_target(raw_id) else {
            return;
        };
        // `color`：lyc 单色图层模式（RRGGBB / AARRGGBB）。lyprop 无 color 参数，
        // 该键只可能来自 lyc 链路。
        if let Some(color) = raw.get("color")
            && let Some([a, r, g, b]) = crate::compositor::props::parse_hex_color(color)
        {
            self.scene.set_solid_color(&id, Some([r, g, b, a]));
        }
        // `mask`：lyc 蒙版图。同时保留在 custom 里（shader 蒙版路径也读它）。
        // 注意 lyshader 效果也用 "mask" 键传蒙版**图层引用**——本次或此前设置过
        // shader 的图层不做 lyc 蒙版镜像，避免把图层引用当图片路径合成。
        if let Some(mask) = raw.get("mask") {
            let shader_active = raw.get("shader").is_some_and(|s| !s.is_empty())
                || self.scene.get(&id).is_some_and(|layer| {
                    layer.props.shader.as_deref().is_some_and(|s| !s.is_empty())
                });
            if !shader_active {
                self.scene
                    .set_mask(&id, Some(mask.clone()).filter(|m| !m.is_empty()));
            }
        }
        self.scene.set_props(&id, raw);
    }
}

fn parse_drag_area(value: &str) -> Option<(f32, f32, f32, f32)> {
    let mut parts = value.split(',').map(|part| part.trim().parse::<f32>().ok());
    Some((
        parts.next()??,
        parts.next()??,
        parts.next()??,
        parts.next()??,
    ))
}

fn trim_float(value: f32) -> String {
    if value.fract().abs() < f32::EPSILON {
        format!("{}", value as i32)
    } else {
        value.to_string()
    }
}

fn layer_id_is_same_or_descendant(candidate: &str, root: &str) -> bool {
    candidate == root
        || candidate
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::mock::MockProvider;
    use crate::render_pipeline::draw::{TextureId, TextureInfo, TextureProvider};
    use asb_interpreter::Event;
    use asb_interpreter::event::LayerEvent;

    struct AlphaProvider {
        alpha: u8,
    }

    impl TextureProvider for AlphaProvider {
        fn resolve(&mut self, _name: &str) -> Option<(TextureId, TextureInfo)> {
            Some((
                TextureId(1),
                TextureInfo {
                    width: 100,
                    height: 100,
                },
            ))
        }

        fn upload_rgba(
            &mut self,
            _name: &str,
            _width: u32,
            _height: u32,
            _data: &[u8],
        ) -> Option<(TextureId, TextureInfo)> {
            None
        }

        fn pixel_alpha(&self, _texture: TextureId, _x: u32, _y: u32) -> Option<u8> {
            Some(self.alpha)
        }
    }

    fn create(id: &str, file: &str) -> Event {
        Event::Layer(LayerEvent::Create {
            id: id.into(),
            file: file.into(),
        })
    }

    #[test]
    fn create_and_delete_via_events() {
        let mut c = Compositor::new();
        c.apply_event(&create("1", "bg"));
        c.apply_event(&create("1.0", "fg"));
        assert_eq!(c.scene().len(), 2);

        c.apply_event(&Event::Layer(LayerEvent::Delete { id: "1".into() }));
        assert!(c.scene().is_empty());
    }

    #[test]
    fn create2_applies_alpha() {
        let mut c = Compositor::new();
        c.apply_event(&Event::Layer(LayerEvent::Create2 {
            id: "1".into(),
            file: "bg".into(),
            alpha: Some(128),
        }));
        assert_eq!(c.scene().get("1").unwrap().props.alpha, Some(128));
    }

    #[test]
    fn set_properties_event_merges() {
        let mut c = Compositor::new();
        c.apply_event(&create("1", "bg"));
        let mut props = HashMap::new();
        props.insert("left".to_string(), "50".to_string());
        props.insert("alpha".to_string(), "200".to_string());
        c.apply_event(&Event::Layer(LayerEvent::SetProperties {
            id: "1".into(),
            properties: props,
        }));
        let p = &c.scene().get("1").unwrap().props;
        assert_eq!(p.left, Some(50.0));
        assert_eq!(p.alpha, Some(200));
    }

    #[test]
    fn rename_event_moves_layer() {
        let mut c = Compositor::new();
        c.apply_event(&create("1.0", "a"));
        c.apply_event(&Event::LayerRename {
            id: "1.0".into(),
            to: "1.5".into(),
        });
        assert!(c.scene().get("1.0").is_none());
        assert_eq!(c.scene().get("1.5").unwrap().file.as_deref(), Some("a"));
    }

    #[test]
    fn tween_event_drives_value_then_settles() {
        let mut c = Compositor::new();
        c.apply_event(&create("1", "a"));
        c.apply_event(&Event::LayerTween {
            id: "1".into(),
            param: "alpha".into(),
            from: Some("0".into()),
            to: Some("255".into()),
            ease: None,
            time: Some(1000),
            delay: None,
            loop_count: None,
            yoyo: None,
            loop_delay: None,
            sync: false,
            delete: false,
            handler_file: None,
            handler_label: None,
            handler_handler: None,
        });

        // 推进到中点，缓动仍在进行。
        c.advance(500);
        let mut provider = MockProvider::new();
        let frame = crate::render_pipeline::RenderPipeline::new(&c).build(&mut provider);
        assert!((frame.commands[0].opacity - 0.5).abs() < 0.02);

        // 推进到结束，缓动被回收且终值固化到属性。
        c.advance(600);
        assert!(c.scene().get("1").unwrap().tweens.is_empty());
        assert_eq!(c.scene().get("1").unwrap().props.alpha, Some(255));
    }

    #[test]
    fn ignores_unrelated_events() {
        let mut c = Compositor::new();
        c.apply_event(&create("1", "a"));
        // 文本/音频等事件不应改变场景。
        c.apply_event(&Event::Text {
            content: "hello".into(),
        });
        c.apply_event(&Event::StopAllSounds { duration: 0 });
        assert_eq!(c.scene().len(), 1);
    }

    #[test]
    fn reset_for_load_clears_scene_and_input() {
        let mut c = Compositor::new();
        c.apply_event(&create("title", "title_bg"));
        c.apply_event(&Event::SetEventHandler {
            event_name: "push".into(),
            file: None,
            label: None,
            call: false,
            handler: Some("calllua".into()),
            extra_params: HashMap::from([("key".into(), "1".into())]),
        });

        c.reset_for_load();

        assert!(c.scene().is_empty());
        assert!(c.get_input_handler("push", "1").is_none());
    }

    #[test]
    fn lyevent_disable_enable_preserves_registered_handler_params() {
        let mut c = Compositor::new();
        c.apply_event(&create("slot", "slot_button"));
        c.apply_event(&Event::Layer(LayerEvent::SetProperties {
            id: "slot".into(),
            properties: HashMap::from([
                ("width".into(), "100".into()),
                ("height".into(), "100".into()),
            ]),
        }));
        c.apply_event(&Event::LayerEventHandler {
            id: "slot".into(),
            event_type: "rollover".into(),
            mode: "init".into(),
            file: None,
            label: None,
            call: false,
            handler: Some("calllua".into()),
            penetration: false,
            extra_params: HashMap::from([
                ("function".into(), "btn_over".into()),
                ("key".into(), "bt_save10".into()),
            ]),
        });

        c.apply_event(&Event::LayerEventHandler {
            id: "slot".into(),
            event_type: "rollover".into(),
            mode: "disable".into(),
            file: None,
            label: None,
            call: false,
            handler: Some("calllua".into()),
            penetration: false,
            extra_params: HashMap::new(),
        });
        let mut provider = MockProvider::new();
        assert_eq!(c.hit_test(10.0, 10.0, &mut provider), None);

        c.apply_event(&Event::LayerEventHandler {
            id: "slot".into(),
            event_type: "rollover".into(),
            mode: "enable".into(),
            file: None,
            label: None,
            call: false,
            handler: Some("calllua".into()),
            penetration: false,
            extra_params: HashMap::new(),
        });

        let handler = &c.scene().get("slot").unwrap().event_handlers["rollover"];
        assert!(handler.enabled);
        assert_eq!(
            handler.params.get("key").map(String::as_str),
            Some("bt_save10")
        );
        assert_eq!(
            handler.params.get("function").map(String::as_str),
            Some("btn_over")
        );
        assert_eq!(
            handler.filter_params.get("id").map(String::as_str),
            Some("slot")
        );
        assert_eq!(
            handler.filter_params.get("type").map(String::as_str),
            Some("rollover")
        );
        assert_eq!(
            handler.filter_params.get("handler").map(String::as_str),
            Some("calllua")
        );
        assert_eq!(
            handler.filter_params.get("function").map(String::as_str),
            Some("btn_over")
        );
        let mut provider = MockProvider::new();
        assert_eq!(c.hit_test(10.0, 10.0, &mut provider), Some("slot".into()));
    }

    #[test]
    fn hit_test_clickablethreshold_uses_texture_alpha_not_layer_alpha() {
        let mut c = Compositor::new();
        c.apply_event(&create("dock", "dockarea"));
        c.apply_event(&Event::Layer(LayerEvent::SetProperties {
            id: "dock".into(),
            properties: HashMap::from([
                ("left".into(), "10".into()),
                ("top".into(), "20".into()),
                ("width".into(), "100".into()),
                ("height".into(), "100".into()),
                ("alpha".into(), "0".into()),
                ("clickablethreshold".into(), "128".into()),
            ]),
        }));
        c.apply_event(&Event::LayerEventHandler {
            id: "dock".into(),
            event_type: "rollover".into(),
            mode: String::new(),
            file: None,
            label: None,
            call: false,
            handler: Some("calllua".into()),
            penetration: false,
            extra_params: HashMap::new(),
        });

        let mut opaque_provider = AlphaProvider { alpha: 255 };
        assert_eq!(
            c.hit_test(50.0, 50.0, &mut opaque_provider),
            Some("dock".into())
        );

        let mut transparent_provider = AlphaProvider { alpha: 0 };
        assert_eq!(c.hit_test(50.0, 50.0, &mut transparent_provider), None);
    }

    #[test]
    fn hit_test_all_returns_overlapping_hover_layers_top_to_bottom() {
        let mut c = Compositor::new();
        c.apply_event(&create("1.0", "lower"));
        c.apply_event(&create("1.1", "upper"));
        for id in ["1.0", "1.1"] {
            c.apply_event(&Event::Layer(LayerEvent::SetProperties {
                id: id.into(),
                properties: HashMap::from([
                    ("left".into(), "0".into()),
                    ("top".into(), "0".into()),
                    ("width".into(), "100".into()),
                    ("height".into(), "100".into()),
                ]),
            }));
            c.apply_event(&Event::LayerEventHandler {
                id: id.into(),
                event_type: "rollover".into(),
                mode: String::new(),
                file: None,
                label: None,
                call: false,
                handler: Some("calllua".into()),
                penetration: id == "1.0",
                extra_params: HashMap::new(),
            });
        }

        let mut provider = MockProvider::new();
        assert_eq!(
            c.hit_test_all(10.0, 10.0, &mut provider),
            vec!["1.1".to_string(), "1.0".to_string()]
        );

        let mut provider = MockProvider::new();
        assert_eq!(c.hit_test(10.0, 10.0, &mut provider), Some("1.1".into()));
    }

    #[test]
    fn hit_test_respects_parent_transform() {
        let mut c = Compositor::new();
        c.apply_event(&create("1.0", "button"));
        c.apply_event(&Event::Layer(LayerEvent::SetProperties {
            id: "1".into(),
            properties: HashMap::from([("xscale".into(), "200".into())]),
        }));
        c.apply_event(&Event::Layer(LayerEvent::SetProperties {
            id: "1.0".into(),
            properties: HashMap::from([
                ("left".into(), "50".into()),
                ("top".into(), "0".into()),
                ("width".into(), "100".into()),
                ("height".into(), "100".into()),
            ]),
        }));
        c.apply_event(&Event::LayerEventHandler {
            id: "1.0".into(),
            event_type: "rollover".into(),
            mode: String::new(),
            file: None,
            label: None,
            call: false,
            handler: Some("calllua".into()),
            penetration: false,
            extra_params: HashMap::new(),
        });

        let mut provider = MockProvider::new();
        assert_eq!(c.hit_test(180.0, 50.0, &mut provider), Some("1.0".into()));

        let mut provider = MockProvider::new();
        assert_eq!(c.hit_test(75.0, 50.0, &mut provider), None);
    }

    #[test]
    fn drag_layer_updates_offset_with_dragarea() {
        let mut c = Compositor::new();
        c.apply_event(&create("1", "slider"));
        c.apply_event(&Event::Layer(LayerEvent::SetProperties {
            id: "1".into(),
            properties: HashMap::from([
                ("left".into(), "80".into()),
                ("top".into(), "0".into()),
                ("draggable".into(), "1".into()),
                ("dragarea".into(), "0,0,160,0".into()),
            ]),
        }));

        assert!(c.is_layer_draggable("1"));
        assert_eq!(
            c.drag_layer_to("1", 80.0, 0.0, 200.0, 20.0),
            Some((160.0, 0.0))
        );
        assert_eq!(c.layer_offset("1"), Some((160.0, 0.0)));

        assert_eq!(
            c.drag_layer_to("1", 80.0, 0.0, -200.0, -20.0),
            Some((0.0, 0.0))
        );
        assert_eq!(c.layer_offset("1"), Some((0.0, 0.0)));
    }

    #[test]
    fn anime_events_play_n_rounds_then_settle_on_last_frame() {
        let mut c = Compositor::new();
        let anime = |mode: &str, file: Option<&str>, time: Option<u64>, loop_count: Option<i32>| {
            Event::Anime {
                id: "90".into(),
                mode: mode.into(),
                file: file.map(str::to_string),
                mask: None,
                time,
                loop_count,
                props: HashMap::new(),
            }
        };
        // init/add/end 序列：两帧 100ms 间隔、总时长 200ms、播放 1 次。
        c.apply_event(&anime("init", Some("g0"), None, Some(1)));
        c.apply_event(&anime("add", Some("g1"), Some(100), None));
        c.apply_event(&anime("end", None, Some(200), None));

        // 播放中：150ms 处于第二帧。
        c.advance(150);
        assert_eq!(c.scene().get("90").unwrap().file.as_deref(), Some("g1"));
        assert!(c.anime_states.contains_key("90"));

        // 播完 1 次（>=200ms）：停在最后一帧并清理播放状态。
        c.advance(100);
        assert_eq!(c.scene().get("90").unwrap().file.as_deref(), Some("g1"));
        assert!(c.anime_states.is_empty());
    }

    fn set_tween(id: &str, param: &str, from: &str, to: &str, time: u64) -> Event {
        Event::LayerTween {
            id: id.into(),
            param: param.into(),
            from: Some(from.into()),
            to: Some(to.into()),
            ease: None,
            time: Some(time),
            delay: None,
            loop_count: None,
            yoyo: None,
            loop_delay: None,
            sync: false,
            delete: false,
            handler_file: None,
            handler_label: None,
            handler_handler: None,
        }
    }

    #[test]
    fn tweenset_runs_members_sequentially() {
        let mut c = Compositor::new();
        c.apply_event(&create("1", "a"));
        c.apply_event(&Event::TweenSetStart);
        c.apply_event(&set_tween("1", "left", "0", "100", 1000));
        c.apply_event(&set_tween("1", "left", "100", "0", 1000));
        // 收集期间不应立即产生缓动。
        assert!(c.scene().get("1").unwrap().tweens.is_empty());
        c.apply_event(&Event::TweenSetEnd);

        // 组内同参数不做替换：两段都在。
        let tweens = &c.scene().get("1").unwrap().tweens;
        assert_eq!(tweens.len(), 2);
        assert_eq!(tweens[0].start_ms, 0);
        assert_eq!(tweens[1].start_ms, 1000);
        assert_eq!(tweens[0].set_id, tweens[1].set_id);
        assert!(tweens[0].set_id.is_some());

        // 第一段中点：left≈50（第二段未启动，不得用 from=100 覆盖）。
        c.advance(500);
        let mut provider = MockProvider::new();
        let frame = crate::render_pipeline::RenderPipeline::new(&c).build(&mut provider);
        let x = frame.commands[0].transform.translation.x;
        assert!((x - 50.0).abs() < 2.0, "left={x}");

        // 第二段中点（1500ms）：从 100 回落到 ~50。
        c.advance(1000);
        let mut provider = MockProvider::new();
        let frame = crate::render_pipeline::RenderPipeline::new(&c).build(&mut provider);
        let x = frame.commands[0].transform.translation.x;
        assert!((x - 50.0).abs() < 2.0, "left={x}");

        // 全部结束：终值 0。
        c.advance(1000);
        assert_eq!(c.scene().get("1").unwrap().props.left, Some(0.0));
        assert!(c.scene().get("1").unwrap().tweens.is_empty());
    }

    #[test]
    fn tweendel_cascades_whole_tween_set() {
        let mut c = Compositor::new();
        c.apply_event(&create("1", "a"));
        c.apply_event(&create("2", "b"));
        c.apply_event(&Event::TweenSetStart);
        c.apply_event(&set_tween("1", "left", "0", "100", 1000));
        c.apply_event(&set_tween("2", "top", "0", "50", 1000));
        c.apply_event(&Event::TweenSetEnd);
        assert_eq!(c.scene().get("2").unwrap().tweens.len(), 1);

        // lytweendel 完成图层 1 的 tween → 同组图层 2 的 tween 一并删除。
        c.apply_event(&Event::LayerTweenDelete { id: "1".into() });
        assert!(c.scene().get("1").unwrap().tweens.is_empty());
        assert!(c.scene().get("2").unwrap().tweens.is_empty());
        // 图层 1 的缓动落终值；图层 2 的被删除（不落终值）。
        assert_eq!(c.scene().get("1").unwrap().props.left, Some(100.0));
        assert_eq!(c.scene().get("2").unwrap().props.top, None);
    }

    #[test]
    fn deleting_layer_cascades_tween_set() {
        let mut c = Compositor::new();
        c.apply_event(&create("1", "a"));
        c.apply_event(&create("2", "b"));
        c.apply_event(&Event::TweenSetStart);
        c.apply_event(&set_tween("1", "left", "0", "100", 1000));
        c.apply_event(&set_tween("2", "top", "0", "50", 1000));
        c.apply_event(&Event::TweenSetEnd);

        c.apply_event(&Event::Layer(LayerEvent::Delete { id: "1".into() }));
        assert!(c.scene().get("1").is_none());
        assert!(c.scene().get("2").unwrap().tweens.is_empty());
    }

    #[test]
    fn deleting_logical_parent_removes_independent_message_overlay() {
        let mut c = Compositor::new();
        c.apply_event(&create("1", "background"));
        c.ensure_layer("__message_overlay_1_80_adv");
        c.set_message_layer_binding("1.80.mw.adv", "__message_overlay_1_80_adv");
        c.set_default_message_layer(Some("1.80.mw.adv".into()));
        assert!(c.is_message_layer_visible("1.80.mw.adv"));

        c.apply_event(&Event::Layer(LayerEvent::Delete { id: "1".into() }));

        assert!(c.scene().get("__message_overlay_1_80_adv").is_none());
        assert!(!c.is_message_layer_visible("1.80.mw.adv"));
        assert!(c.default_message_layer.is_none());

        // `/chgmsg` 弹栈只会重新登记绑定，不应复活已随父层删除的旧文字。
        c.ensure_layer("__message_overlay_1_80_adv");
        c.set_message_layer_binding("1.80.mw.adv", "__message_overlay_1_80_adv");
        assert!(!c.is_message_layer_visible("1.80.mw.adv"));

        // 脚本显式 chgmsg 到该层时才重新启用。
        c.revive_message_layer("1.80.mw.adv");
        assert!(c.is_message_layer_visible("1.80.mw.adv"));
    }

    #[test]
    fn deleting_logical_parent_keeps_unrelated_message_overlay() {
        let mut c = Compositor::new();
        c.apply_event(&create("1", "background"));
        c.apply_event(&create("2", "menu"));
        c.ensure_layer("__message_overlay_1");
        c.ensure_layer("__message_overlay_2");
        c.set_message_layer_binding("1.80.mw.adv", "__message_overlay_1");
        c.set_message_layer_binding("2.help", "__message_overlay_2");

        c.apply_event(&Event::Layer(LayerEvent::Delete { id: "1".into() }));

        assert!(c.scene().get("__message_overlay_1").is_none());
        assert!(c.scene().get("__message_overlay_2").is_some());
        assert!(c.is_message_layer_visible("2.help"));
    }

    #[test]
    fn lyprop_bang_targets_root_props() {
        let mut c = Compositor::new();
        c.apply_event(&create("1", "a"));
        c.apply_event(&Event::Layer(LayerEvent::SetProperties {
            id: "!".into(),
            properties: HashMap::from([("alpha".into(), "128".into())]),
        }));
        // 不应创建名为 "!" 的普通图层。
        assert!(c.scene().get("!").is_none());
        assert_eq!(c.scene().root_props().alpha, Some(128));
    }

    #[test]
    fn lyprop_tilde_resolves_message_layer_binding() {
        let mut c = Compositor::new();
        c.apply_event(&create("mw", "mw_bg"));
        c.set_message_layer_binding("adv", "mw");
        c.set_default_message_layer(Some("adv".into()));

        // `~adv` → 绑定的场景图层 mw。
        c.apply_event(&Event::Layer(LayerEvent::SetProperty {
            id: "~adv".into(),
            property: "alpha".into(),
            value: "100".into(),
        }));
        assert_eq!(c.scene().get("mw").unwrap().props.alpha, Some(100));
        assert!(c.scene().get("~adv").is_none());

        // `~` → 默认消息图层。
        c.apply_event(&Event::Layer(LayerEvent::SetProperty {
            id: "~".into(),
            property: "left".into(),
            value: "42".into(),
        }));
        assert_eq!(c.scene().get("mw").unwrap().props.left, Some(42.0));

        // 未登记的 `~xxx` 回退为按 xxx 直查场景。
        c.apply_event(&create("nvl", "nvl_bg"));
        c.apply_event(&Event::Layer(LayerEvent::SetProperty {
            id: "~nvl".into(),
            property: "top".into(),
            value: "7".into(),
        }));
        assert_eq!(c.scene().get("nvl").unwrap().props.top, Some(7.0));
    }

    #[test]
    fn color_property_switches_layer_to_solid_mode() {
        let mut c = Compositor::new();
        c.apply_event(&Event::Layer(LayerEvent::Create {
            id: "5".into(),
            file: "".into(),
        }));
        c.apply_event(&Event::Layer(LayerEvent::SetProperties {
            id: "5".into(),
            properties: HashMap::from([
                ("color".into(), "80FF0000".into()),
                ("width".into(), "100".into()),
                ("height".into(), "50".into()),
            ]),
        }));
        let layer = c.scene().get("5").unwrap();
        assert_eq!(layer.solid_color, Some([255, 0, 0, 0x80]));
        assert_eq!(layer.props.width, Some(100.0));
    }

    #[test]
    fn mask_property_sets_layer_mask() {
        let mut c = Compositor::new();
        c.apply_event(&create("1", "fg"));
        c.apply_event(&Event::Layer(LayerEvent::SetProperty {
            id: "1".into(),
            property: "mask".into(),
            value: "fgmask".into(),
        }));
        assert_eq!(c.scene().get("1").unwrap().mask.as_deref(), Some("fgmask"));
    }

    #[test]
    fn lyedit_negative_reuploads_and_overrides_layer_texture() {
        let mut c = Compositor::new();
        c.apply_event(&create("1", "bg"));

        let mut provider = MockProvider::new();
        provider.put_pixels("bg", 1, 1, vec![10, 20, 30, 255]);

        c.apply_event(&Event::LayerEdit {
            id: "1".into(),
            mode: "negative".into(),
            color: None,
            file: None,
            left: None,
            top: None,
        });
        // 排队后由渲染管线处理（build 内部触发）。
        let frame = crate::render_pipeline::RenderPipeline::new(&c).build(&mut provider);
        let name = provider.name_of(frame.commands[0].texture).to_string();
        assert!(name.starts_with("__lyedit_1_"), "name={name}");
        let (_, _, pixels) = provider.pixels_named(&name).unwrap();
        assert_eq!(&pixels[0..4], &[245, 235, 225, 255]);

        // 连续第二次 lyedit 在上一次结果上叠加。
        c.apply_event(&Event::LayerEdit {
            id: "1".into(),
            mode: "negative".into(),
            color: None,
            file: None,
            left: None,
            top: None,
        });
        let frame = crate::render_pipeline::RenderPipeline::new(&c).build(&mut provider);
        let name2 = provider.name_of(frame.commands[0].texture).to_string();
        assert_ne!(name, name2);
        let (_, _, pixels) = provider.pixels_named(&name2).unwrap();
        assert_eq!(&pixels[0..4], &[10, 20, 30, 255]);

        // 图层换图后重定向失效，恢复解析原始 file。
        c.apply_event(&create("1", "bg2"));
        let frame = crate::render_pipeline::RenderPipeline::new(&c).build(&mut provider);
        assert_eq!(provider.name_of(frame.commands[0].texture), "bg2");
    }

    #[test]
    fn glyph_wait_icon_show_and_hide() {
        let mut c = Compositor::new();
        c.apply_event(&create("90", "glyph0"));

        // homing=1：移动并显示。
        c.show_click_wait_icon("90", 320.0, 240.0, true);
        let layer = c.scene().get("90").unwrap();
        assert_eq!(layer.props.visible, Some(true));
        assert_eq!(layer.props.left, Some(320.0));
        assert_eq!(layer.props.top, Some(240.0));
        assert_eq!(c.active_wait_icon(), Some("90"));

        // 隐藏。
        c.hide_click_wait_icon();
        assert_eq!(c.scene().get("90").unwrap().props.visible, Some(false));
        assert_eq!(c.active_wait_icon(), None);

        // homing=0：只切可见性，不动坐标。
        c.apply_event(&Event::Layer(LayerEvent::SetProperties {
            id: "90".into(),
            properties: HashMap::from([("left".into(), "5".into()), ("top".into(), "6".into())]),
        }));
        c.show_click_wait_icon("90", 999.0, 999.0, false);
        let layer = c.scene().get("90").unwrap();
        assert_eq!(layer.props.visible, Some(true));
        assert_eq!(layer.props.left, Some(5.0));

        // 换图标图层时自动隐藏旧图层。
        c.show_click_wait_icon("91", 0.0, 0.0, false);
        assert_eq!(c.scene().get("90").unwrap().props.visible, Some(false));
        assert_eq!(c.scene().get("91").unwrap().props.visible, Some(true));
    }

    #[test]
    fn skip_transition_by_input_clears_active_transition() {
        let mut c = Compositor::new();
        c.apply_event(&Event::Trans {
            trans_type: 1,
            time: Some(1000),
            rule: None,
            vague: None,
            input: 1,
        });
        assert!(c.skip_transition_by_input(false));
        assert!(!c.skip_transition_by_input(false)); // 已清除
    }

    #[test]
    fn tween_default_from_uses_current_value() {
        let mut c = Compositor::new();
        c.apply_event(&create("1", "a"));
        // 当前 left=100，from 省略，应从 100 缓动到 0。
        let mut props = HashMap::new();
        props.insert("left".to_string(), "100".to_string());
        c.apply_event(&Event::Layer(LayerEvent::SetProperties {
            id: "1".into(),
            properties: props,
        }));
        c.apply_event(&Event::LayerTween {
            id: "1".into(),
            param: "left".into(),
            from: None,
            to: Some("0".into()),
            ease: None,
            time: Some(1000),
            delay: None,
            loop_count: None,
            yoyo: None,
            loop_delay: None,
            sync: false,
            delete: false,
            handler_file: None,
            handler_label: None,
            handler_handler: None,
        });
        let t = &c.scene().get("1").unwrap().tweens[0];
        assert_eq!(t.from, 100.0);
        assert_eq!(t.to, 0.0);
    }
}
