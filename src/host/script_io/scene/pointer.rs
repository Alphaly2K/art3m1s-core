use super::*;
use crate::host::input::{OVERRIDE_IS_DOWN_EDGE, OVERRIDE_IS_PUSH, OVERRIDE_IS_UP_EDGE};
use crate::script_handler::{event_dispatch_layers, handler_tags, layer_filter_params};

#[derive(Default)]
pub(super) struct PointerDispatch {
    due: bool,
    position: Option<(i32, i32)>,
    hovered: Vec<String>,
    pending: VecDeque<InputDispatch>,
    current: Option<InputDispatch>,
    pub(super) tags: Vec<(String, HashMap<String, String>)>,
}

struct InputDispatch {
    name: String,
    filter: HashMap<String, String>,
    tags: Vec<(String, HashMap<String, String>)>,
    claim: Option<u32>,
}

impl InputDispatch {
    fn global(
        handler: &crate::compositor::reduce::InputHandler,
        event: &str,
        claim: Option<u32>,
        params: &[(&str, &str)],
    ) -> Self {
        Self {
            name: format!("seton{event}"),
            filter: handler.filter_params.clone(),
            tags: handler_tags(
                handler.handler.as_deref(),
                handler.file.as_deref(),
                handler.label.as_deref(),
                handler.call,
                &handler.params,
                params,
            ),
            claim,
        }
    }
}

impl SceneIo {
    pub(in crate::host::script_io) fn advance_input(&mut self, now_ns: u64) {
        self.with_input(|input| input.advance(now_ns));
        self.pointer.due = true;
    }

    pub(in crate::host::script_io) fn pointer_ready(&self) -> bool {
        self.pointer.due || self.pointer.current.is_some() || !self.pointer.pending.is_empty()
    }

    pub(in crate::host::script_io) fn next_pointer_filter(&mut self) -> Option<EventFilterWork> {
        if std::mem::take(&mut self.pointer.due) {
            let mut scene = self.scene.lock().unwrap();
            scene.input.freeze();
            self.pointer.sample(&mut scene);
        }
        let mut dispatch = self.pointer.pending.pop_front()?;
        let name = std::mem::take(&mut dispatch.name);
        let params = std::mem::take(&mut dispatch.filter);
        self.pointer.current = Some(dispatch);
        Some(EventFilterWork::Queued(name, params))
    }

    pub(in crate::host::script_io) fn queue_mode_handler(&mut self, event: &str) {
        let scene = self.scene.lock().unwrap();
        if let Some(handler) = scene.compositor.get_input_handler(event, "") {
            self.pointer.pending.push_back(InputDispatch::global(
                handler,
                event,
                None,
                &[("type", event)],
            ));
        }
    }

    pub(in crate::host::script_io) fn finish_pointer_filter(
        &mut self,
        decision: EventFilterDecision,
    ) -> bool {
        let Some(mut dispatch) = self.pointer.current.take() else {
            return false;
        };
        let accepted = match decision {
            EventFilterDecision::Rejected => false,
            EventFilterDecision::Handled => true,
            EventFilterDecision::DispatchDefault => {
                let queued = !dispatch.tags.is_empty();
                self.pointer.tags.append(&mut dispatch.tags);
                queued
            }
        };
        if accepted && let Some(code) = dispatch.claim {
            self.with_input(|input| input.claim(code));
        }
        true
    }
}

impl PointerDispatch {
    fn sample(&mut self, scene: &mut SceneProjection) {
        self.sample_pointer(scene);
        for (code, bits) in scene.input.effective_keys() {
            let key = code.to_string();
            if let Some(handler) = scene.compositor.get_input_handler("push", &key)
                && (bits & OVERRIDE_IS_DOWN_EDGE != 0
                    || (bits & OVERRIDE_IS_PUSH != 0
                        && handler
                            .params
                            .get("keyrepeat")
                            .is_some_and(|repeat| repeat == "1")))
            {
                let event_type = if matches!(code, 1 | 2 | 4) {
                    "click"
                } else {
                    "key"
                };
                self.pending.push_back(InputDispatch::global(
                    handler,
                    "push",
                    Some(code),
                    &[("key", &key), ("type", event_type)],
                ));
            }
        }
    }

    fn sample_pointer(&mut self, scene: &mut SceneProjection) {
        let position = scene.input.pointer_present.then_some(scene.input.pointer);
        let left = scene.input.frame_bits(1);
        let click = left & OVERRIDE_IS_DOWN_EDGE != 0;
        if position == self.position && !click && left & OVERRIDE_IS_UP_EDGE == 0 {
            return;
        }
        self.position = position;
        let hits = position.map_or_else(Vec::new, |(x, y)| scene.hit_layers(x, y));
        let hovered = event_dispatch_layers(&scene.compositor, &hits, "rollover");
        let mut left: Vec<_> = self
            .hovered
            .iter()
            .filter(|id| !hovered.contains(id))
            .cloned()
            .collect();
        left.sort_unstable();
        for id in left {
            self.push(scene, &id, "rollout");
        }
        for id in &hovered {
            if !self.hovered.contains(id) {
                self.push(scene, id, "rollover");
            }
        }
        self.hovered = hovered;
        if click {
            for id in event_dispatch_layers(&scene.compositor, &hits, "click") {
                self.push(scene, &id, "click");
            }
        }
    }

    fn push(&mut self, scene: &SceneProjection, id: &str, event: &str) {
        let Some(handler) = scene
            .compositor
            .scene()
            .get(id)
            .and_then(|layer| layer.event_handlers.get(event))
            .filter(|handler| handler.enabled)
        else {
            return;
        };
        let click = event == "click";
        self.pending.push_back(InputDispatch {
            name: "lyevent".into(),
            filter: layer_filter_params(handler, id, event).into_owned(),
            tags: handler_tags(
                handler.handler.as_deref(),
                handler.file.as_deref(),
                handler.label.as_deref(),
                handler.call,
                &handler.params,
                if click { &[("click", "1")] } else { &[] },
            ),
            claim: click.then_some(1),
        });
    }
}
