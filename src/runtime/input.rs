use super::{CoreRuntime, InlineEventFrame};
use crate::compositor::Compositor;
use asb_interpreter::event::WaitReason;
use std::collections::{HashMap, HashSet};

impl CoreRuntime {
    pub fn feed_mouse(&self, x: i32, y: i32) {
        let mut s = self.input.lock().unwrap();
        s.mouse_x = x;
        s.mouse_y = y;
    }

    pub fn feed_click(&self) {
        let mut s = self.input.lock().unwrap();
        s.clicked = true;
        s.keys_down_edge.insert(1);
    }

    pub fn feed_mouse_button(&self, button: u32, pressed: bool) {
        let mut s = self.input.lock().unwrap();
        if pressed {
            if s.mouse_buttons_down.insert(button) {
                s.mouse_buttons_down_edge.insert(button);
                if s.keys_down.insert(button) {
                    s.keys_down_edge.insert(button);
                }
            }
        } else {
            if s.mouse_buttons_down.remove(&button) {
                s.mouse_buttons_up_edge.insert(button);
            }
            if s.keys_down.remove(&button) {
                s.keys_up_edge.insert(button);
            }
        }
    }

    /// 宿主触摸事件入口：`phase` 0=down/1=move/2=up。快照层负责多点上限截断、
    /// flick 阈值判定等。
    pub fn feed_touch(&self, id: u32, phase: u8, x: i32, y: i32) {
        let mut s = self.input.lock().unwrap();
        s.feed_touch(id, phase, x, y);
    }

    pub fn feed_key_down(&self, vk: u32) {
        let mut s = self.input.lock().unwrap();
        if s.keys_down.insert(vk) {
            s.keys_down_edge.insert(vk);
        }
    }

    pub fn feed_key_up(&self, vk: u32) {
        let mut s = self.input.lock().unwrap();
        if s.keys_down.remove(&vk) {
            s.keys_up_edge.insert(vk);
        }
    }

    pub(super) fn process_pointer_handlers(&mut self) -> bool {
        self.refresh_inline_event_frame();
        let (
            legacy_clicked,
            mouse_x,
            mouse_y,
            mouse_buttons,
            mouse_down_edges,
            mouse_up_edges,
            key_down_edges,
            keys_down,
        ) = {
            let s = self.input.lock().unwrap();
            let clicked = s.clicked;
            let mut key_down_edges: Vec<u32> = s.keys_down_edge.iter().copied().collect();
            key_down_edges.sort_unstable();
            (
                clicked,
                s.mouse_x as f32,
                s.mouse_y as f32,
                s.mouse_buttons_down.clone(),
                s.mouse_buttons_down_edge.clone(),
                s.mouse_buttons_up_edge.clone(),
                key_down_edges,
                s.keys_down.clone(),
            )
        };
        let left_down_edge = legacy_clicked || mouse_down_edges.contains(&1);
        let left_up_edge = mouse_up_edges.contains(&1);
        let left_down = mouse_buttons.contains(&1);
        let mut needs_inline_event_frame = false;

        // controlskip：按住 keyconfig role 14 的键（缺省 Ctrl=17）期间强制跳过。
        self.update_control_skip_from_keys(&keys_down);

        // hide 模式：左键单击先恢复消息窗，本帧不再进入常规点击/前进链。
        // （右键恢复走下方 trigger_rclick 的隐藏分支。）
        if self.hide_active() && left_down_edge {
            self.exit_hide_mode();
            return false;
        }

        // 文本内联链接（[link]）：鼠标移动刷新 hover 强调；点击命中链接则以其
        // file/label 触发 jump 并吞掉该次点击（不再推进剧情）。
        self.update_link_hover(mouse_x, mouse_y);
        if left_down_edge && self.handle_link_click(mouse_x, mouse_y) {
            return false;
        }

        let hit_layers = self
            .compositor
            .hit_test_all(mouse_x, mouse_y, &mut self.texture_provider);
        let top_hover = hit_layers.first().cloned();
        let hover_dispatch = event_dispatch_layers(&self.compositor, &hit_layers, "rollover");
        let new_hovered: HashSet<String> = hover_dispatch.iter().cloned().collect();
        if new_hovered != self.hovered_layers {
            let mut old_only: Vec<String> = self
                .hovered_layers
                .difference(&new_hovered)
                .cloned()
                .collect();
            old_only.sort();
            for old in old_only {
                let dispatch = enqueue_layer_handler(
                    &self.interpreter,
                    &self.compositor,
                    &old,
                    "rollout",
                    &[],
                );
                needs_inline_event_frame |= dispatch.needs_return_frame;
            }

            for new in hover_dispatch {
                if !self.hovered_layers.contains(&new) {
                    let dispatch = enqueue_layer_handler(
                        &self.interpreter,
                        &self.compositor,
                        &new,
                        "rollover",
                        &[],
                    );
                    needs_inline_event_frame |= dispatch.needs_return_frame;
                }
            }
            self.hovered_layers = new_hovered;
        }

        let mut handled_by_layer = false;
        let mut handled_by_left_push = false;
        let mut handled_by_drag = false;
        let click_dispatch = if left_down_edge {
            event_dispatch_layers(&self.compositor, &hit_layers, "click")
        } else {
            Vec::new()
        };
        if left_down_edge {
            if let Some(ref id) = top_hover {
                let dispatch = self.start_pointer_drag(id, mouse_x, mouse_y);
                handled_by_drag = dispatch.handled;
                needs_inline_event_frame |= dispatch.needs_return_frame;
            }
            if !handled_by_drag {
                for id in &click_dispatch {
                    let dispatch = enqueue_layer_handler(
                        &self.interpreter,
                        &self.compositor,
                        id,
                        "click",
                        &[("click", "1")],
                    );
                    handled_by_layer |= dispatch.handled;
                    needs_inline_event_frame |= dispatch.needs_return_frame;
                }
            }
        }

        if left_down {
            let dispatch = self.continue_pointer_drag(mouse_x, mouse_y);
            handled_by_drag |= dispatch.handled;
            needs_inline_event_frame |= dispatch.needs_return_frame;
        }
        if left_up_edge {
            let dispatch = self.finish_pointer_drag();
            handled_by_drag |= dispatch.handled;
            needs_inline_event_frame |= dispatch.needs_return_frame;
        }

        let mut role_advance = false;
        for key in key_down_edges {
            // 右键/ESC：先走引擎的 rclick 链（隐藏恢复 / rclick 脚本）；
            // 被消费时不再派发同键的 push 处理器。
            if is_rclick_trigger_key(key) && self.trigger_rclick() {
                continue;
            }
            let key_string = key.to_string();
            let event_type = if is_mouse_button(key) { "click" } else { "key" };
            let dispatch = enqueue_input_handler(
                &self.interpreter,
                &self.compositor,
                "push",
                &key_string,
                &[("key", &key_string), ("type", event_type)],
            );
            needs_inline_event_frame |= dispatch.needs_return_frame;
            if key == 1 && dispatch.handled {
                handled_by_left_push = true;
            }
            // keyconfig 的 role 分配（前进/隐藏/日志/自动/跳过等）。
            role_advance |= self.handle_role_key_edge(key);
        }

        if needs_inline_event_frame {
            self.begin_inline_event_frame();
        }

        let push_absorbs_default_click =
            global_push_absorbs_default_click(self.wait_reason.as_ref(), handled_by_left_push);
        let clicked = (left_down_edge
            && !handled_by_layer
            && !push_absorbs_default_click
            && !handled_by_drag)
            // keyconfig role 0（前进，缺省 Enter）与单击等效
            || role_advance;
        if left_down_edge {
            crate::core_debug!(
                "[input] left-down wait={:?} top={:?} click_layers={:?} layer={} push={} drag={} advance={}",
                self.wait_reason,
                top_hover,
                click_dispatch,
                handled_by_layer,
                handled_by_left_push,
                handled_by_drag,
                clicked
            );
        }
        clicked
    }

    pub(super) fn clear_input_edges(&self) {
        self.input.lock().unwrap().clear_edges();
    }

    pub(super) fn script_decide_edge(&self) -> bool {
        self.input.lock().unwrap().scripted_down_edge()
    }

    fn begin_inline_event_frame(&mut self) {
        self.refresh_inline_event_frame();
        if self.active_inline_event_frame.is_some() {
            return;
        }
        let Some(script) = self.interpreter.current_script().map(str::to_string) else {
            return;
        };
        let line = self.interpreter.current_line();
        let stack = self.interpreter.call_stack();
        let mut event_stack = stack.clone();
        event_stack.push(asb_interpreter::CallFrame {
            script: script.clone(),
            return_line: line,
        });
        if let Err(error) = self
            .interpreter
            .restore_position(&script, line, event_stack)
        {
            crate::core_error!("建立事件返回帧失败: {error:?}");
            return;
        }
        self.active_inline_event_frame = Some(InlineEventFrame {
            script,
            line,
            stack,
            claimed_by_jump: false,
        });
    }

    pub(super) fn refresh_inline_event_frame(&mut self) {
        let Some(frame) = &self.active_inline_event_frame else {
            return;
        };
        let stack = self.interpreter.call_stack();
        if !inline_event_marker_is_active(frame, &stack) {
            self.active_inline_event_frame = None;
        }
    }

    fn start_pointer_drag(
        &mut self,
        layer_id: &str,
        mouse_x: f32,
        mouse_y: f32,
    ) -> HandlerDispatch {
        if !self.compositor.is_layer_draggable(layer_id)
            || !has_drag_handler(&self.compositor, layer_id)
        {
            return HandlerDispatch::default();
        }
        self.begin_pointer_drag(layer_id, mouse_x, mouse_y)
    }

    /// `[lydrag]`：把图层强制设为拖动状态。
    ///
    /// 典型用途是滑块：点击滑轨后先用 lyprop 把旋钮移到鼠标下，再用 lydrag
    /// 立即开始拖动。与常规拖动入口不同，这里跳过 `is_layer_draggable` /
    /// 拖动处理器检查，也不要求鼠标悬停在该图层上；之后每帧由
    /// `continue_pointer_drag` 照常接管。
    pub(super) fn force_pointer_drag(&mut self, layer_id: &str) {
        let (mouse_x, mouse_y) = {
            let s = self.input.lock().unwrap();
            (s.mouse_x as f32, s.mouse_y as f32)
        };
        let dispatch = self.begin_pointer_drag(layer_id, mouse_x, mouse_y);
        if !dispatch.handled {
            crate::core_warn!("[lydrag] 图层不存在，忽略: {layer_id}");
            return;
        }
        if dispatch.needs_return_frame {
            self.begin_inline_event_frame();
        }
    }

    /// 常规/强制拖动共用的启动逻辑：记录起点并派发 dragin 处理器。
    fn begin_pointer_drag(
        &mut self,
        layer_id: &str,
        mouse_x: f32,
        mouse_y: f32,
    ) -> HandlerDispatch {
        let Some(state) = forced_drag_state(
            layer_id,
            mouse_x,
            mouse_y,
            self.compositor.layer_offset(layer_id),
        ) else {
            return HandlerDispatch::default();
        };
        self.pointer_drag = state;
        let dispatch = enqueue_layer_handler(
            &self.interpreter,
            &self.compositor,
            layer_id,
            "dragin",
            &[("drag", "1"), ("id", layer_id)],
        );
        HandlerDispatch {
            handled: true,
            needs_return_frame: dispatch.needs_return_frame,
        }
    }

    fn continue_pointer_drag(&mut self, mouse_x: f32, mouse_y: f32) -> HandlerDispatch {
        let Some(layer_id) = self.pointer_drag.layer_id.clone() else {
            return HandlerDispatch::default();
        };
        let dx = mouse_x - self.pointer_drag.start_mouse_x;
        let dy = mouse_y - self.pointer_drag.start_mouse_y;
        self.compositor.drag_layer_to(
            &layer_id,
            self.pointer_drag.start_left,
            self.pointer_drag.start_top,
            dx,
            dy,
        );
        self.sync_layer_info(&layer_id);
        let dispatch = enqueue_layer_handler(
            &self.interpreter,
            &self.compositor,
            &layer_id,
            "drag",
            &[("drag", "1"), ("id", &layer_id)],
        );
        HandlerDispatch {
            handled: true,
            needs_return_frame: dispatch.needs_return_frame,
        }
    }

    /// 每帧按鼠标位置刷新文本链接的 hover 强调。返回 hover 是否发生变化
    /// （runtime 目前每帧都重建文本命令，返回值仅供未来节流用）。
    fn update_link_hover(&mut self, mouse_x: f32, mouse_y: f32) -> bool {
        match self.text_renderer.as_mut() {
            Some(renderer) => renderer.update_link_hover(mouse_x, mouse_y),
            None => false,
        }
    }

    /// 点击命中文本链接时以其 file/label 触发 jump，并返回 true 表示已吞掉本次
    /// 点击（不再传给剧情推进）。未命中返回 false。
    ///
    /// 命中区必须落在**有效可见**的消息层上：mw 被隐藏（如右键关闭消息窗）后，
    /// 其文本缓冲与链接区间仍在 renderer 里，但那片区域不该再响应链接点击，
    /// 否则点在已隐藏的旧文本区会误触发跳转，导致剧情推进卡死。
    fn handle_link_click(&mut self, mouse_x: f32, mouse_y: f32) -> bool {
        let Some(renderer) = self.text_renderer.as_ref() else {
            return false;
        };
        let area = renderer
            .link_hit_areas()
            .into_iter()
            .filter(|a| a.contains(mouse_x, mouse_y))
            .find(|a| {
                link_area_has_jump_target(a)
                    && self.compositor.is_message_layer_visible(&a.layer_id)
            });
        let Some(area) = area else {
            return false;
        };
        // 纯跳转（非 call）：入队 "jump" 标签，交由 advance_script 在当前
        // 等待/停止态下排水执行——命中链接后剧情跳转到目标并从那里继续，
        // 不返回原位置，故无需内联返回帧（与图层 click 的 file/label 跳转一致）。
        enqueue_handler_tags(
            &self.interpreter,
            None,
            area.file.as_deref(),
            area.label.as_deref(),
            false,
            &HashMap::new(),
            &[],
        );
        true
    }

    fn finish_pointer_drag(&mut self) -> HandlerDispatch {
        let Some(layer_id) = self.pointer_drag.layer_id.take() else {
            return HandlerDispatch::default();
        };
        let dispatch = enqueue_layer_handler(
            &self.interpreter,
            &self.compositor,
            &layer_id,
            "dragout",
            &[("drag", "0"), ("id", &layer_id)],
        );
        HandlerDispatch {
            handled: true,
            needs_return_frame: dispatch.needs_return_frame,
        }
    }
}

pub(super) fn inline_event_marker_is_active(
    frame: &InlineEventFrame,
    stack: &[asb_interpreter::CallFrame],
) -> bool {
    stack
        .get(frame.stack.len())
        .is_some_and(|marker| marker.script == frame.script && marker.return_line == frame.line)
}

pub(super) fn detach_inline_event_marker(
    frame: &InlineEventFrame,
    stack: &mut Vec<asb_interpreter::CallFrame>,
) -> bool {
    if !inline_event_marker_is_active(frame, stack) {
        return false;
    }
    stack.remove(frame.stack.len());
    true
}

#[derive(Clone, Copy, Debug, Default)]
struct HandlerDispatch {
    handled: bool,
    needs_return_frame: bool,
}

fn is_mouse_button(key: u32) -> bool {
    matches!(key, 1..=3)
}

/// 触发右键链（rclick 脚本 / 隐藏恢复）的按键：鼠标右键(2)、ESC(27)。
/// docs/spec/key_assign.md：右键、ESC → 调用右键脚本 rclick.iet。
fn is_rclick_trigger_key(key: u32) -> bool {
    matches!(key, 2 | 27)
}

fn global_push_absorbs_default_click(
    wait_reason: Option<&WaitReason>,
    handled_by_left_push: bool,
) -> bool {
    handled_by_left_push && !matches!(wait_reason, Some(WaitReason::Timed { input: 1, .. }))
}

/// 计算拖动起始状态：只要求图层存在（能取到 offset），不做 draggable /
/// 处理器 / 鼠标悬停检查——这是 `[lydrag]` 强制拖动语义的核心。
fn forced_drag_state(
    layer_id: &str,
    mouse_x: f32,
    mouse_y: f32,
    layer_offset: Option<(f32, f32)>,
) -> Option<super::PointerDragState> {
    let (left, top) = layer_offset?;
    Some(super::PointerDragState {
        layer_id: Some(layer_id.to_string()),
        start_mouse_x: mouse_x,
        start_mouse_y: mouse_y,
        start_left: left,
        start_top: top,
    })
}

/// 文本链接命中区域是否有可跳转目标（file 或 label 至少其一非空）。
/// 二者皆空的链接视为无目标，点击不吞、照常推进剧情。
fn link_area_has_jump_target(area: &crate::text::render::LinkHitArea) -> bool {
    area.file.is_some() || area.label.is_some()
}

fn has_drag_handler(compositor: &Compositor, layer_id: &str) -> bool {
    compositor
        .scene()
        .get(layer_id)
        .map(|layer| {
            ["drag", "dragin", "dragout"].iter().any(|event_type| {
                layer
                    .event_handlers
                    .get(*event_type)
                    .is_some_and(|handler| handler.enabled)
            })
        })
        .unwrap_or(false)
}

/// Artemis only dispatches an overlapping pointer event to the top hit layer
/// and lower handlers that explicitly opt into `penetration`. Penetrating
/// handlers run from bottom to top.
fn event_dispatch_layers(
    compositor: &Compositor,
    hit_layers: &[String],
    event_type: &str,
) -> Vec<String> {
    let mut layers = hit_layers
        .iter()
        .enumerate()
        .filter_map(|(index, id)| {
            let handler = compositor.scene().get(id)?.event_handlers.get(event_type)?;
            (handler.enabled && (index == 0 || handler.penetration)).then(|| id.clone())
        })
        .collect::<Vec<_>>();
    layers.reverse();
    layers
}

#[cfg(test)]
mod tests {
    use super::{
        InlineEventFrame, dispatch_handler, event_dispatch_layers, forced_drag_state,
        global_push_absorbs_default_click, inline_event_marker_is_active,
        link_area_has_jump_target,
    };
    use crate::compositor::Compositor;
    use crate::text::render::LinkHitArea;
    use asb_interpreter::event::{Event, WaitReason};
    use asb_interpreter::{CallFrame, Interpreter, InterpreterConfig};
    use std::collections::HashMap;

    #[test]
    fn event_filter_fake_results_never_enqueue_the_original_handler() {
        let interpreter = Interpreter::new(InterpreterConfig::default());
        let filter_params = HashMap::from([
            ("id".to_string(), "button".to_string()),
            ("type".to_string(), "click".to_string()),
        ]);

        interpreter
            .lua()
            .load(
                r#"
                __engine:setEventFilter(function(e, name, param)
                    seen_name = name
                    seen_id = param.id
                    return verdict
                end)
                "#,
            )
            .exec()
            .unwrap();

        interpreter.lua().globals().set("verdict", 1).unwrap();
        let success = dispatch_handler(
            &interpreter,
            "lyevent",
            &filter_params,
            Some("calllua"),
            None,
            None,
            false,
            &HashMap::new(),
            &[],
        );
        assert!(success.handled);
        assert!(
            interpreter
                .engine_context()
                .lock()
                .unwrap()
                .tag_queue
                .is_empty()
        );

        interpreter.lua().globals().set("verdict", 2).unwrap();
        let failure = dispatch_handler(
            &interpreter,
            "lyevent",
            &filter_params,
            Some("calllua"),
            None,
            None,
            false,
            &HashMap::new(),
            &[],
        );
        assert!(!failure.handled);
        assert!(
            interpreter
                .engine_context()
                .lock()
                .unwrap()
                .tag_queue
                .is_empty()
        );
        assert_eq!(
            interpreter
                .lua()
                .globals()
                .get::<String>("seen_name")
                .unwrap(),
            "lyevent"
        );
        assert_eq!(
            interpreter
                .lua()
                .globals()
                .get::<String>("seen_id")
                .unwrap(),
            "button"
        );
    }

    #[test]
    fn input_enabled_timed_wait_keeps_default_click_despite_global_push() {
        let input_wait = WaitReason::Timed {
            milliseconds: 2500,
            input: 1,
        };
        let non_input_wait = WaitReason::Timed {
            milliseconds: 2500,
            input: 0,
        };

        assert!(!global_push_absorbs_default_click(Some(&input_wait), true));
        assert!(global_push_absorbs_default_click(
            Some(&non_input_wait),
            true
        ));
        assert!(global_push_absorbs_default_click(None, true));
        assert!(!global_push_absorbs_default_click(None, false));
    }

    #[test]
    fn overlapping_events_only_include_penetrating_lower_layers_bottom_to_top() {
        let mut compositor = Compositor::new();
        for (id, penetration) in [("lower", true), ("middle", false), ("top", false)] {
            compositor.apply_event(&Event::LayerEventHandler {
                id: id.into(),
                event_type: "click".into(),
                mode: "init".into(),
                file: None,
                label: None,
                call: false,
                handler: Some("calllua".into()),
                penetration,
                extra_params: HashMap::new(),
            });
        }

        let hits = vec!["top".into(), "middle".into(), "lower".into()];
        assert_eq!(
            event_dispatch_layers(&compositor, &hits, "click"),
            vec!["lower".to_string(), "top".to_string()]
        );
    }

    #[test]
    fn nested_ui_events_reuse_the_existing_inline_return_frame() {
        let caller = CallFrame {
            script: "story.asb".into(),
            return_line: 12,
        };
        let frame = InlineEventFrame {
            script: "system/ui.asb".into(),
            line: 80,
            stack: vec![caller.clone()],
            claimed_by_jump: false,
        };
        let marker = CallFrame {
            script: frame.script.clone(),
            return_line: frame.line,
        };
        let nested_call = CallFrame {
            script: "system/script.asb".into(),
            return_line: 21,
        };

        assert!(inline_event_marker_is_active(
            &frame,
            &[caller.clone(), marker.clone(), nested_call]
        ));
        assert!(inline_event_marker_is_active(
            &frame,
            &[caller.clone(), marker]
        ));
        assert!(!inline_event_marker_is_active(&frame, &[caller]));
    }

    #[test]
    fn forced_drag_only_requires_the_layer_to_exist() {
        // [lydrag]：即便图层未标记 draggable、鼠标不在图层上，也应进入拖动态；
        // 起点取当前鼠标坐标与图层 offset。
        let state = forced_drag_state("knob", 320.0, 240.0, Some((80.0, 10.0))).unwrap();
        assert_eq!(state.layer_id.as_deref(), Some("knob"));
        assert_eq!(state.start_mouse_x, 320.0);
        assert_eq!(state.start_mouse_y, 240.0);
        assert_eq!(state.start_left, 80.0);
        assert_eq!(state.start_top, 10.0);

        // 图层不存在（取不到 offset）时不得进入拖动态。
        assert!(forced_drag_state("missing", 0.0, 0.0, None).is_none());
    }

    #[test]
    fn link_area_jump_target_requires_file_or_label() {
        let base = LinkHitArea {
            layer_id: "msg".into(),
            link_index: 0,
            left: 0.0,
            top: 0.0,
            width: 10.0,
            height: 10.0,
            file: None,
            label: None,
        };
        // file/label 皆空 → 无目标，点击不吞。
        assert!(!link_area_has_jump_target(&base));
        // 只有 file。
        assert!(link_area_has_jump_target(&LinkHitArea {
            file: Some("scene2.asb".into()),
            ..base.clone()
        }));
        // 只有 label。
        assert!(link_area_has_jump_target(&LinkHitArea {
            label: Some("branch_a".into()),
            ..base.clone()
        }));
        // 两者都有。
        assert!(link_area_has_jump_target(&LinkHitArea {
            file: Some("scene2.asb".into()),
            label: Some("branch_a".into()),
            ..base
        }));
    }
}

pub(super) fn enqueue_handler_tags(
    interpreter: &asb_interpreter::Interpreter,
    handler_tag: Option<&str>,
    file: Option<&str>,
    label: Option<&str>,
    call: bool,
    params: &HashMap<String, String>,
    runtime_params: &[(&str, &str)],
) {
    let ctx = interpreter.engine_context();
    let mut queue = ctx.lock().unwrap();
    if let Some(tag) = handler_tag {
        let mut p = params.clone();
        for (k, v) in runtime_params {
            p.insert(k.to_string(), v.to_string());
        }
        queue.tag_queue.push((tag.to_string(), p));
    }
    if file.is_some() || label.is_some() {
        let mut p = HashMap::new();
        if let Some(f) = file {
            p.insert("file".to_string(), f.to_string());
        }
        if let Some(l) = label {
            p.insert("label".to_string(), l.to_string());
        }
        queue
            .tag_queue
            .push((if call { "call" } else { "jump" }.to_string(), p));
    }
}

/// 把一个已命中的处理器排队并给出派发结论。
///
/// `needs_return_frame`：只有"纯 handler（无 file/label 跳转）"需要伪造
/// 返回帧——这是层事件与全局输入事件共用的派发策略，改动请保持两侧一致。
fn dispatch_handler(
    interpreter: &asb_interpreter::Interpreter,
    filter_name: &str,
    filter_params: &HashMap<String, String>,
    handler: Option<&str>,
    file: Option<&str>,
    label: Option<&str>,
    call: bool,
    params: &HashMap<String, String>,
    runtime_params: &[(&str, &str)],
) -> HandlerDispatch {
    // 过滤器观察的是注册事件的标签名及原始参数，而非触发时的 click/key。
    // 1 = 假装成功，2 = 假装失败；两者都不能把原处理器排入队列。
    match interpreter.run_event_filter(filter_name, filter_params) {
        Some(1) => {
            return HandlerDispatch {
                handled: true,
                needs_return_frame: false,
            };
        }
        Some(2) => return HandlerDispatch::default(),
        _ => {}
    }
    enqueue_handler_tags(
        interpreter,
        handler,
        file,
        label,
        call,
        params,
        runtime_params,
    );
    HandlerDispatch {
        handled: true,
        needs_return_frame: handler.is_some() && file.is_none() && label.is_none(),
    }
}

fn enqueue_layer_handler(
    interpreter: &asb_interpreter::Interpreter,
    compositor: &Compositor,
    layer_id: &str,
    event_type: &str,
    runtime_params: &[(&str, &str)],
) -> HandlerDispatch {
    let Some(layer) = compositor.scene().get(layer_id) else {
        return HandlerDispatch::default();
    };
    let Some(h) = layer.event_handlers.get(event_type) else {
        return HandlerDispatch::default();
    };
    if !h.enabled {
        return HandlerDispatch::default();
    }
    let fallback_filter_params;
    let filter_params = if h.filter_params.is_empty() {
        fallback_filter_params = {
            let mut params = h.params.clone();
            params.insert("id".to_string(), layer_id.to_string());
            params.insert("type".to_string(), event_type.to_string());
            if let Some(value) = &h.file {
                params.insert("file".to_string(), value.clone());
            }
            if let Some(value) = &h.label {
                params.insert("label".to_string(), value.clone());
            }
            if let Some(value) = &h.handler {
                params.insert("handler".to_string(), value.clone());
            }
            if h.call {
                params.insert("call".to_string(), "1".to_string());
            }
            if h.penetration {
                params.insert("penetration".to_string(), "1".to_string());
            }
            params
        };
        &fallback_filter_params
    } else {
        &h.filter_params
    };
    dispatch_handler(
        interpreter,
        "lyevent",
        filter_params,
        h.handler.as_deref(),
        h.file.as_deref(),
        h.label.as_deref(),
        h.call,
        &h.params,
        runtime_params,
    )
}

fn enqueue_input_handler(
    interpreter: &asb_interpreter::Interpreter,
    compositor: &Compositor,
    event_name: &str,
    key: &str,
    runtime_params: &[(&str, &str)],
) -> HandlerDispatch {
    let Some(h) = compositor.get_input_handler(event_name, key) else {
        return HandlerDispatch::default();
    };
    let filter_name = format!("seton{event_name}");
    dispatch_handler(
        interpreter,
        &filter_name,
        &h.filter_params,
        h.handler.as_deref(),
        h.file.as_deref(),
        h.label.as_deref(),
        h.call,
        &h.params,
        runtime_params,
    )
}
