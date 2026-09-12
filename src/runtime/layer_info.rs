//! Synchronous script queries must include emitted, not-yet-dispatched layers.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;

use asb_interpreter::Event;

use crate::compositor::{Compositor, TextureInfo};

#[derive(Default)]
pub(super) struct LayerQueryState {
    scene: Compositor,
    dimensions: HashMap<String, Option<(u32, u32)>>,
}

impl LayerQueryState {
    pub fn sync(
        &mut self,
        scene: &Compositor,
        mut cached: impl FnMut(&str) -> Option<TextureInfo>,
    ) {
        self.scene.sync_query_scene_from(scene);
        let mut live = HashSet::new();
        for layer in scene.scene().all_layers() {
            if let Some(file) = &layer.file {
                live.insert(file.as_str());
                if let Some(info) = cached(file) {
                    self.dimensions
                        .insert(file.clone(), Some((info.width, info.height)));
                }
            }
        }
        self.dimensions
            .retain(|file, _| live.contains(file.as_str()));
    }

    pub fn observes(event: &Event) -> bool {
        matches!(
            event,
            Event::Layer(_)
                | Event::LayerRename { .. }
                | Event::LayerTween { .. }
                | Event::LayerTweenDelete { .. }
                | Event::Anime { .. }
                | Event::TweenSetStart
                | Event::TweenSetEnd
        )
    }

    pub fn observe(&mut self, event: &Event) {
        // Reuse the compositor reducer, including parent creation, subtree
        // deletion, property merge and rename. Never render or dispatch media.
        if Self::observes(event) {
            self.scene.apply_event(event);
        }
    }

    pub fn sync_layer(
        &mut self,
        scene: &Compositor,
        id: &str,
        cached: impl FnOnce(&str) -> Option<TextureInfo>,
    ) {
        self.scene.clock_ms = scene.clock_ms();
        if let Some(layer) = scene.scene().get(id) {
            self.scene.scene.ensure(id);
            *self.scene.scene.get_mut(id).unwrap() = layer.clone();
            if let Some(file) = &layer.file
                && let Some(info) = cached(file)
            {
                self.dimensions
                    .insert(file.clone(), Some((info.width, info.height)));
            }
        } else {
            self.scene.scene.delete(id);
        }
    }

    pub fn get(
        &mut self,
        id: &str,
        mut dimensions: impl FnMut(&str) -> Option<(u32, u32)>,
    ) -> Option<HashMap<String, String>> {
        let layer = self.scene.scene().get(id)?;
        let props = crate::compositor::build::resolved_props(layer, self.scene.clock_ms());
        let needs_dimensions =
            props.clip_rect().is_none() && (props.width.is_none() || props.height.is_none());
        let texture_info = layer
            .file
            .as_deref()
            .filter(|file| needs_dimensions && !file.is_empty())
            .and_then(|file| {
                let size = self
                    .dimensions
                    .entry(file.to_string())
                    .or_insert_with(|| dimensions(file));
                size.map(|(width, height)| TextureInfo { width, height })
            });
        Some(super::events::layer_info_entry(
            layer,
            self.scene.clock_ms(),
            texture_info,
        ))
    }

    pub fn all(
        &mut self,
        mut dimensions: impl FnMut(&str) -> Option<(u32, u32)>,
    ) -> Vec<(String, HashMap<String, String>)> {
        let mut ids: Vec<_> = self
            .scene
            .scene()
            .all_layers()
            .map(|layer| layer.id.clone())
            .collect();
        ids.sort();
        ids.into_iter()
            .filter_map(|id| self.get(&id, &mut dimensions).map(|info| (id, info)))
            .collect()
    }
}

pub(super) fn asset_dimensions(
    paths: &super::magic_path::MagicPathTable,
    resources: &crate::host_files::HostResources,
    file: &str,
) -> Option<(u32, u32)> {
    if file.starts_with("image/text/") {
        return None;
    }
    let resolved = super::magic_path::resolve_path(paths, file);
    // Match the texture provider's lookup order, but only read image headers.
    for path in [format!("{resolved}.png"), resolved] {
        for limit in [4096, 65536, 1048576] {
            let mut bytes = vec![0u8; limit];
            let Ok(read) = resources.read_range(&path, 0, &mut bytes) else {
                break;
            };
            let short = read < limit;
            bytes.truncate(read);
            if let Ok(reader) = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()
                && let Ok(size) = reader.into_dimensions()
            {
                return Some(size);
            }
            if short {
                break;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use asb_interpreter::event::LayerEvent;

    #[test]
    fn lua_creation_and_next_lua_query_work_in_one_interpreter_run() {
        use asb_interpreter::{CallbackResult, Interpreter, InterpreterConfig};
        use std::sync::{Arc, Mutex};

        let state = Arc::new(Mutex::new(LayerQueryState::default()));
        state
            .lock()
            .unwrap()
            .dimensions
            .insert("body".into(), Some((1024, 760)));
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter.set_engine_callbacks(Box::new(super::super::callbacks::FfiCallbacks {
            input: Default::default(),
            magic_paths: Default::default(),
            layer_info: Arc::clone(&state),
            resources: crate::host_files::default_resources().clone(),
            volumes: Default::default(),
            debug_skip_active: Default::default(),
            script_status: Default::default(),
            emote: Default::default(),
        }));
        interpreter.set_callback(move |event| {
            state.lock().unwrap().observe(&event);
            CallbackResult::Continue
        });
        interpreter
            .load_script(
                "main",
                r#"
[lua]
function createCharacter(e)
    e:tag{"lyc", id="10.1.6.0.0.1.a.1", file="body"}
end
function placeCharacter(e)
    e:tag{"var", name="q", system="get_layer_info", style="map"}
    e:tag{"var", name="observed_height", data=e:var("q.10.1.6.0.0.1.a.1.height")}
end
[/lua]
[calllua function="createCharacter"]
[calllua function="placeCharacter"]
[stop]
"#,
            )
            .unwrap();
        interpreter.start("main", "").unwrap();
        interpreter.run().unwrap();
        assert_eq!(
            interpreter
                .variables()
                .get("observed_height")
                .unwrap()
                .to_string(),
            "760"
        );
    }

    #[test]
    fn queries_include_pending_create_props_rename_and_delete_without_rendering() {
        let mut state = LayerQueryState::default();
        let mut actual = Compositor::new();
        state.sync(&actual, |_| None);
        let create = Event::Layer(LayerEvent::Create {
            id: "10.1.6.0.0.1.a.1".into(),
            file: "body".into(),
        });
        state.observe(&create);
        assert!(actual.scene().get("10.1.6").is_none());
        let info = state
            .get("10.1.6.0.0.1.a.1", |_| Some((1024, 760)))
            .unwrap();
        assert_eq!(info["width"], "1024");
        assert_eq!(info["height"], "760");
        let props = Event::Layer(LayerEvent::SetProperties {
            id: "10.1.6".into(),
            properties: HashMap::from([
                ("left".into(), "640".into()),
                ("top".into(), "-40".into()),
            ]),
        });
        state.observe(&props);
        assert_eq!(state.get("10.1.6", |_| None).unwrap()["top"], "-40");
        assert!(
            state
                .all(|_| panic!("cached dimensions"))
                .iter()
                .any(|(id, _)| id == "10.1.6.0.0.1.a.1")
        );
        actual.apply_event(&create);
        actual.apply_event(&props);
        state.sync(&actual, |_| None);
        state.observe(&Event::LayerRename {
            id: "10.1.6".into(),
            to: "10.2.6".into(),
        });
        assert!(state.get("10.1.6", |_| None).is_none());
        assert_eq!(
            state
                .get("10.2.6.0.0.1.a.1", |_| panic!("cached dimensions"))
                .unwrap()["width"],
            "1024"
        );
        state.observe(&Event::Layer(LayerEvent::Delete {
            id: "10.2.6".into(),
        }));
        assert!(state.get("10.2.6.0.0.1.a.1", |_| None).is_none());
        state.sync(&Compositor::new(), |_| None);
        assert!(state.dimensions.is_empty());
    }
}
