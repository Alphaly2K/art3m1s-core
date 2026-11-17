use super::*;
use crate::host::input::{OVERRIDE_IS_DOWN_EDGE, OVERRIDE_IS_PUSH, OVERRIDE_IS_UP_EDGE};
use crate::script_handler::{event_dispatch_layers, handler_tags, layer_filter_params};

#[derive(Default)]
pub(super) struct PointerDispatch {
    due: bool,
    position: Option<(i32, i32)>,
    hovered: Vec<String>,
    drag: Option<Drag>,
    pending: VecDeque<InputDispatch>,
    current: Option<InputDispatch>,
    pub(super) tags: Vec<(String, HashMap<String, String>)>,
}

struct Drag {
    id: String,
    pointer: (i32, i32),
    origin: (f32, f32),
}

struct InputDispatch {
    name: String,
    filter: HashMap<String, String>,
    tags: Vec<(String, HashMap<String, String>)>,
    claim: Option<u32>,
    fallback: Vec<InputDispatch>,
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
            fallback: Vec::new(),
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

    pub(in crate::host::script_io) fn freeze_new_input(&self) -> bool {
        if self.pointer.due {
            self.with_input(|input| input.freeze());
        }
        self.pointer.due
    }

    pub(in crate::host::script_io) fn next_pointer_filter(&mut self) -> Option<EventFilterWork> {
        if std::mem::take(&mut self.pointer.due) {
            self.pointer.sample(&mut self.scene.lock().unwrap());
        }
        let mut dispatch = self.pointer.pending.pop_front()?;
        let name = std::mem::take(&mut dispatch.name);
        let params = std::mem::take(&mut dispatch.filter);
        self.pointer.current = Some(dispatch);
        Some(EventFilterWork::Queued(name, params))
    }

    pub(in crate::host::script_io) fn queue_mode_handler(&mut self, event: &str) {
        self.queue_global_handler(event, "", &[("type", event)]);
    }

    pub(in crate::host::script_io) fn queue_global_handler(
        &mut self,
        event: &str,
        key: &str,
        params: &[(&str, &str)],
    ) {
        let scene = self.scene.lock().unwrap();
        if let Some(handler) = scene.compositor.get_input_handler(event, key) {
            self.pointer
                .pending
                .push_back(InputDispatch::global(handler, event, None, params));
        }
    }

    pub(in crate::host::script_io) fn queue_menu(&mut self, file: &str, leaving: bool) {
        self.pointer.tags.extend(handler_tags(
            None,
            Some(file),
            leaving.then_some("leave"),
            !leaving,
            &HashMap::new(),
            &[],
        ));
    }

    pub(in crate::host::script_io) fn force_drag(&mut self, id: &str) {
        self.pointer.begin_drag(&mut self.scene.lock().unwrap(), id);
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
        if !accepted {
            for fallback in dispatch.fallback.into_iter().rev() {
                self.pointer.pending.push_front(fallback);
            }
        }
        true
    }
}

impl PointerDispatch {
    fn sample(&mut self, scene: &mut SceneProjection) {
        let claimed = [1, 2, 27].map(|code| scene.input.is_claimed(code));
        let link_clicked = self.sample_pointer(scene);
        for (code, bits) in scene.input.effective_keys() {
            if (code == 1 && (link_clicked || claimed[0]))
                || (code == 2 && claimed[1])
                || (code == 27 && claimed[2])
            {
                continue;
            }
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

    fn sample_pointer(&mut self, scene: &mut SceneProjection) -> bool {
        let position = scene.input.pointer_present.then_some(scene.input.pointer);
        let left = scene.input.frame_bits(1);
        let click = left & OVERRIDE_IS_DOWN_EDGE != 0 && !scene.input.is_claimed(1);
        let moved = position != self.position;
        let release = scene.input.focus_lost || scene.input.is_physical_release(1);
        let link = if moved || click || scene.dirty {
            scene.link_under_pointer(position)
        } else {
            None
        };
        if click
            && self.drag.is_none()
            && let Some(link) = link
        {
            let tags = handler_tags(
                None,
                link.file.as_deref(),
                link.label.as_deref(),
                false,
                &HashMap::new(),
                &[],
            );
            if !tags.is_empty() {
                scene.input.claim(1);
                self.tags.extend(tags);
                return true;
            }
        }
        if !moved && !click && left & OVERRIDE_IS_UP_EDGE == 0 && !release {
            return false;
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
            self.pending
                .extend(Self::layer(scene, &id, "rollout", None, &[]));
        }
        for id in &hovered {
            if !self.hovered.contains(id) {
                self.pending
                    .extend(Self::layer(scene, id, "rollover", None, &[]));
            }
        }
        self.hovered = hovered;
        if click && self.drag.is_none() {
            let draggable = hits
                .first()
                .filter(|id| scene.compositor.is_layer_draggable(id));
            let direct_drag = draggable.is_some_and(|id| {
                scene
                    .compositor
                    .scene()
                    .get(id)
                    .and_then(|layer| layer.event_handlers.get("drag"))
                    .is_some_and(|handler| handler.enabled)
                    && self.begin_drag(scene, id)
            });
            if !direct_drag {
                let clicks = event_dispatch_layers(&scene.compositor, &hits, "click")
                    .into_iter()
                    .filter_map(|id| Self::layer(scene, &id, "click", Some(1), &[("click", "1")]));
                if let Some(mut drag_in) = draggable.and_then(|id| {
                    Self::layer(scene, id, "dragin", Some(1), &[("drag", "1"), ("id", id)])
                }) {
                    drag_in.fallback.extend(clicks);
                    self.pending.push_back(drag_in);
                } else {
                    self.pending.extend(clicks);
                }
            }
        }
        if self.drag.is_some() && (click || release) {
            scene.input.claim(1);
        }
        if let Some(drag) = &self.drag
            && moved
            && scene.input.is_physical_down(1)
            && !release
            && let Some((x, y)) = position
        {
            if scene
                .compositor
                .drag_layer_to(
                    &drag.id,
                    drag.origin.0,
                    drag.origin.1,
                    x as f32 - drag.pointer.0 as f32,
                    y as f32 - drag.pointer.1 as f32,
                )
                .is_some()
            {
                scene.dirty = true;
                self.pending.extend(Self::layer(
                    scene,
                    &drag.id,
                    "drag",
                    None,
                    &[("drag", "1"), ("id", &drag.id)],
                ));
            } else {
                self.drag = None;
            }
        }
        if release && let Some(drag) = self.drag.take() {
            self.pending.extend(Self::layer(
                scene,
                &drag.id,
                "dragout",
                None,
                &[("drag", "0"), ("id", &drag.id)],
            ));
        }
        false
    }

    fn begin_drag(&mut self, scene: &mut SceneProjection, id: &str) -> bool {
        let Some(origin) = scene.compositor.layer_offset(id) else {
            return false;
        };
        self.drag = Some(Drag {
            id: id.into(),
            pointer: scene.input.pointer,
            origin,
        });
        scene.input.claim(1);
        self.pending.extend(Self::layer(
            scene,
            id,
            "dragin",
            None,
            &[("drag", "1"), ("id", id)],
        ));
        true
    }

    fn layer(
        scene: &SceneProjection,
        id: &str,
        event: &str,
        claim: Option<u32>,
        params: &[(&str, &str)],
    ) -> Option<InputDispatch> {
        let handler = scene
            .compositor
            .scene()
            .get(id)
            .and_then(|layer| layer.event_handlers.get(event))
            .filter(|handler| handler.enabled)?;
        Some(InputDispatch {
            name: "lyevent".into(),
            filter: layer_filter_params(handler, id, event).into_owned(),
            tags: handler_tags(
                handler.handler.as_deref(),
                handler.file.as_deref(),
                handler.label.as_deref(),
                handler.call,
                &handler.params,
                params,
            ),
            claim,
            fallback: Vec::new(),
        })
    }
}
