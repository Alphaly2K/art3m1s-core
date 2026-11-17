use super::*;
use crate::script_handler::{event_dispatch_layers, handler_tags, layer_filter_params};

#[derive(Default)]
pub(super) struct PointerDispatch {
    due: bool,
    position: Option<(i32, i32)>,
    hovered: Vec<String>,
    pending: VecDeque<LayerDispatch>,
    current: Option<LayerDispatch>,
    pub(super) tags: Vec<(String, HashMap<String, String>)>,
}

struct LayerDispatch {
    filter: HashMap<String, String>,
    tags: Vec<(String, HashMap<String, String>)>,
    click: bool,
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
            self.pointer.sample(&mut self.scene.lock().unwrap());
        }
        let mut dispatch = self.pointer.pending.pop_front()?;
        let params = std::mem::take(&mut dispatch.filter);
        self.pointer.current = Some(dispatch);
        Some(EventFilterWork::Queued("lyevent".into(), params))
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
        if dispatch.click && accepted {
            self.with_input(|input| input.click_claimed = true);
        }
        true
    }
}

impl PointerDispatch {
    fn sample(&mut self, scene: &mut SceneProjection) {
        let position = scene.input.pointer_present.then_some(scene.input.pointer);
        let click = scene.input.is_pressed(1);
        if position == self.position && !click && !scene.input.is_released(1) {
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
        self.pending.push_back(LayerDispatch {
            filter: layer_filter_params(handler, id, event).into_owned(),
            tags: handler_tags(
                handler.handler.as_deref(),
                handler.file.as_deref(),
                handler.label.as_deref(),
                handler.call,
                &handler.params,
                if click { &[("click", "1")] } else { &[] },
            ),
            click,
        });
    }
}
