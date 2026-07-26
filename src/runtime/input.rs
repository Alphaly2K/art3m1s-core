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
            )
        };
        let left_down_edge = legacy_clicked || mouse_down_edges.contains(&1);
        let left_up_edge = mouse_up_edges.contains(&1);
        let left_down = mouse_buttons.contains(&1);
        let mut needs_inline_event_frame = false;

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

        for key in key_down_edges {
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
        }

        if needs_inline_event_frame {
            self.begin_inline_event_frame();
        }

        let push_absorbs_default_click =
            global_push_absorbs_default_click(self.wait_reason.as_ref(), handled_by_left_push);
        let clicked =
            left_down_edge && !handled_by_layer && !push_absorbs_default_click && !handled_by_drag;
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
            committed: false,
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
        let Some((left, top)) = self.compositor.layer_offset(layer_id) else {
            return HandlerDispatch::default();
        };
        self.pointer_drag.layer_id = Some(layer_id.to_string());
        self.pointer_drag.start_mouse_x = mouse_x;
        self.pointer_drag.start_mouse_y = mouse_y;
        self.pointer_drag.start_left = left;
        self.pointer_drag.start_top = top;
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

fn global_push_absorbs_default_click(
    wait_reason: Option<&WaitReason>,
    handled_by_left_push: bool,
) -> bool {
    handled_by_left_push && !matches!(wait_reason, Some(WaitReason::Timed { input: 1, .. }))
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
        InlineEventFrame, detach_inline_event_marker, event_dispatch_layers,
        global_push_absorbs_default_click, inline_event_marker_is_active,
    };
    use crate::compositor::Compositor;
    use asb_interpreter::CallFrame;
    use asb_interpreter::event::{Event, WaitReason};
    use std::collections::HashMap;

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
            committed: false,
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
    fn detaching_committed_marker_preserves_nested_control_flow_frames() {
        let caller = CallFrame {
            script: "story.asb".into(),
            return_line: 12,
        };
        let frame = InlineEventFrame {
            script: "system/ui.asb".into(),
            line: 80,
            stack: vec![caller.clone()],
            committed: true,
        };
        let nested_call = CallFrame {
            script: "system/script.asb".into(),
            return_line: 21,
        };
        let mut stack = vec![
            caller.clone(),
            CallFrame {
                script: frame.script.clone(),
                return_line: frame.line,
            },
            nested_call.clone(),
        ];

        assert!(detach_inline_event_marker(&frame, &mut stack));
        assert_eq!(stack.len(), 2);
        assert_eq!(stack[0].script, caller.script);
        assert_eq!(stack[1].script, nested_call.script);
        assert_eq!(stack[1].return_line, nested_call.return_line);
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
    handler: Option<&str>,
    file: Option<&str>,
    label: Option<&str>,
    call: bool,
    params: &HashMap<String, String>,
    runtime_params: &[(&str, &str)],
) -> HandlerDispatch {
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
    dispatch_handler(
        interpreter,
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
    dispatch_handler(
        interpreter,
        h.handler.as_deref(),
        h.file.as_deref(),
        h.label.as_deref(),
        h.call,
        &h.params,
        runtime_params,
    )
}
