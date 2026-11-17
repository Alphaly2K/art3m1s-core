use super::*;
use crate::compositor::Compositor;
use crate::compositor::query::layer_info_entry;
use asb_interpreter::lua_engine::EngineCallbacks;
use std::sync::{Arc, Mutex};

impl ScriptIoSession {
    pub(in crate::host) fn open_queued_project(
        &mut self,
        ini: &[u8],
        platform: &str,
        reported_os: Option<&str>,
    ) -> Result<u64, ScriptIoError> {
        let epoch = self.open_project(ini, platform, reported_os)?;
        let scene = SceneCallbacks::default();
        let reduced_scene = scene.0.clone();
        self.interpreter.set_engine_callbacks(Box::new(scene));
        self.interpreter.set_callback(move |event| match event {
            Event::Layer(_) | Event::LayerRename { .. } => {
                reduced_scene.lock().unwrap().apply_event(&event);
                CallbackResult::Continue
            }
            _ => CallbackResult::Pause,
        });
        Ok(epoch)
    }
}

#[derive(Default)]
struct SceneCallbacks(Arc<Mutex<Compositor>>);

impl EngineCallbacks for SceneCallbacks {
    fn get_layer_info(&self, id: &str) -> Option<HashMap<String, String>> {
        let scene = self.0.lock().unwrap();
        let layer = scene.scene().get(id)?;
        Some(layer_info_entry(layer, scene.clock_ms(), None))
    }

    fn get_layer_info_all(&self) -> Vec<(String, HashMap<String, String>)> {
        let scene = self.0.lock().unwrap();
        let mut layers: Vec<_> = scene.scene().all_layers().collect();
        layers.sort_unstable_by(|a, b| a.id.cmp(&b.id));
        layers
            .into_iter()
            .map(|layer| {
                (
                    layer.id.clone(),
                    layer_info_entry(layer, scene.clock_ms(), None),
                )
            })
            .collect()
    }

    fn debug(&self, _: i32, _: &str, _: bool) {}
    fn enqueue_tag(&self, _: String, _: HashMap<String, String>) {}
    fn set_event_handler(&self, _: HashMap<String, String>) {}
    fn get_script_status(&self) -> u8 {
        0
    }
    fn is_key_down(&self, _: u32) -> bool {
        false
    }
    fn is_key_down_edge(&self, _: u32) -> bool {
        false
    }
    fn is_key_up_edge(&self, _: u32) -> bool {
        false
    }
    fn is_decide(&self) -> bool {
        false
    }
    fn get_mouse_point(&self) -> (i32, i32) {
        (0, 0)
    }
    fn get_touch_count(&self) -> u32 {
        0
    }
    fn get_touch_point(&self, _: u32) -> (i32, i32) {
        (0, 0)
    }
    fn is_file_exists(&self, _: &str) -> bool {
        false
    }
    fn file_operation(&self, _: &str, _: HashMap<String, String>) {}
    fn include(&self, _: &str) {}
    fn override_key(&self, _: u32, _: u32) {}
    fn set_flick_sensitivity(&self, _: f64) {}
    fn get_script_block(&self) -> HashMap<String, String> {
        HashMap::new()
    }
    fn get_script_stack(&self) -> Vec<HashMap<String, String>> {
        Vec::new()
    }
    fn get_script_wait_reason(&self) -> u8 {
        0
    }
}
