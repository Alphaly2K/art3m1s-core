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
        let _ = s.keys_down.insert(1);
        s.keys_down_edge.insert(1);
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
        let (clicked, mouse_x, mouse_y) = {
            let mut s = self.input.lock().unwrap();
            let v = s.clicked;
            s.clicked = false;
            (v, s.mouse_x as f32, s.mouse_y as f32)
        };

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

        if clicked {
            if let Some(ref id) = new_hover {
                enqueue_layer_handler(
                    &self.interpreter,
                    &self.compositor,
                    id,
                    "click",
                    &[("click", "1")],
                );
                enqueue_input_handler(
                    &self.interpreter,
                    &self.compositor,
                    "push",
                    "1",
                    &[("key", "1"), ("type", "click")],
                );
            }
        }

        clicked
    }

    pub(super) fn clear_input_edges(&self) {
        self.input.lock().unwrap().clear_edges();
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

fn enqueue_layer_handler(
    interpreter: &asb_interpreter::Interpreter,
    compositor: &Compositor,
    layer_id: &str,
    event_type: &str,
    runtime_params: &[(&str, &str)],
) {
    let Some(layer) = compositor.scene().get(layer_id) else {
        return;
    };
    let Some(h) = layer.event_handlers.get(event_type) else {
        return;
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
}

fn enqueue_input_handler(
    interpreter: &asb_interpreter::Interpreter,
    compositor: &Compositor,
    event_name: &str,
    key: &str,
    runtime_params: &[(&str, &str)],
) {
    let Some(h) = compositor.get_input_handler(event_name, key) else {
        return;
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
}
