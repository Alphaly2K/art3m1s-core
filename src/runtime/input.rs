use super::CoreRuntime;
use crate::compositor::Compositor;
use std::collections::HashMap;

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

        let new_hover = self
            .compositor
            .hit_test(mouse_x, mouse_y, &mut self.texture_provider);
        if new_hover != self.hovered_layer {
            if let Some(ref old) = self.hovered_layer {
                enqueue_layer_handler(&self.interpreter, &self.compositor, old, "rollout", &[]);
            }
            if let Some(new) = &new_hover {
                enqueue_layer_handler(&self.interpreter, &self.compositor, new, "rollover", &[]);
            }
            self.hovered_layer = new_hover.clone();
        }

        let mut handled_by_layer = false;
        let mut handled_by_left_push = false;
        let mut handled_by_drag = false;
        if left_down_edge {
            if let Some(ref id) = new_hover {
                handled_by_drag = self.start_pointer_drag(id, mouse_x, mouse_y);
            }
            if let Some(ref id) = new_hover {
                if !handled_by_drag {
                    handled_by_layer = enqueue_layer_handler(
                        &self.interpreter,
                        &self.compositor,
                        id,
                        "click",
                        &[("click", "1")],
                    );
                }
            }
        }

        if left_down {
            handled_by_drag |= self.continue_pointer_drag(mouse_x, mouse_y);
        }
        if left_up_edge {
            handled_by_drag |= self.finish_pointer_drag();
        }

        for key in key_down_edges {
            let key_string = key.to_string();
            let event_type = if is_mouse_button(key) { "click" } else { "key" };
            let handled = enqueue_input_handler(
                &self.interpreter,
                &self.compositor,
                "push",
                &key_string,
                &[("key", &key_string), ("type", event_type)],
            );
            if key == 1 && handled {
                handled_by_left_push = true;
            }
        }

        left_down_edge && !handled_by_layer && !handled_by_left_push && !handled_by_drag
    }

    pub(super) fn clear_input_edges(&self) {
        self.input.lock().unwrap().clear_edges();
    }

    pub(super) fn script_decide_edge(&self) -> bool {
        self.input.lock().unwrap().keys_down_edge.contains(&124)
    }

    fn start_pointer_drag(&mut self, layer_id: &str, mouse_x: f32, mouse_y: f32) -> bool {
        if !self.compositor.is_layer_draggable(layer_id)
            || !has_drag_handler(&self.compositor, layer_id)
        {
            return false;
        }
        let Some((left, top)) = self.compositor.layer_offset(layer_id) else {
            return false;
        };
        self.pointer_drag.layer_id = Some(layer_id.to_string());
        self.pointer_drag.start_mouse_x = mouse_x;
        self.pointer_drag.start_mouse_y = mouse_y;
        self.pointer_drag.start_left = left;
        self.pointer_drag.start_top = top;
        let _ = enqueue_layer_handler(
            &self.interpreter,
            &self.compositor,
            layer_id,
            "dragin",
            &[("drag", "1")],
        );
        true
    }

    fn continue_pointer_drag(&mut self, mouse_x: f32, mouse_y: f32) -> bool {
        let Some(layer_id) = self.pointer_drag.layer_id.clone() else {
            return false;
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
        let _ = enqueue_layer_handler(
            &self.interpreter,
            &self.compositor,
            &layer_id,
            "drag",
            &[("drag", "1")],
        );
        true
    }

    fn finish_pointer_drag(&mut self) -> bool {
        let Some(layer_id) = self.pointer_drag.layer_id.take() else {
            return false;
        };
        let _ = enqueue_layer_handler(
            &self.interpreter,
            &self.compositor,
            &layer_id,
            "dragout",
            &[("drag", "0")],
        );
        true
    }
}

fn is_mouse_button(key: u32) -> bool {
    matches!(key, 1..=3)
}

fn has_drag_handler(compositor: &Compositor, layer_id: &str) -> bool {
    compositor
        .scene()
        .get(layer_id)
        .map(|layer| {
            layer.event_handlers.contains_key("drag")
                || layer.event_handlers.contains_key("dragin")
                || layer.event_handlers.contains_key("dragout")
        })
        .unwrap_or(false)
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

fn enqueue_layer_handler(
    interpreter: &asb_interpreter::Interpreter,
    compositor: &Compositor,
    layer_id: &str,
    event_type: &str,
    runtime_params: &[(&str, &str)],
) -> bool {
    let Some(layer) = compositor.scene().get(layer_id) else {
        return false;
    };
    let Some(h) = layer.event_handlers.get(event_type) else {
        return false;
    };
    enqueue_handler_tags(
        interpreter,
        h.handler.as_deref(),
        h.file.as_deref(),
        h.label.as_deref(),
        h.call,
        &h.params,
        runtime_params,
    );
    true
}

fn enqueue_input_handler(
    interpreter: &asb_interpreter::Interpreter,
    compositor: &Compositor,
    event_name: &str,
    key: &str,
    runtime_params: &[(&str, &str)],
) -> bool {
    let Some(h) = compositor.get_input_handler(event_name, key) else {
        return false;
    };
    enqueue_handler_tags(
        interpreter,
        h.handler.as_deref(),
        h.file.as_deref(),
        h.label.as_deref(),
        h.call,
        &h.params,
        runtime_params,
    );
    true
}
