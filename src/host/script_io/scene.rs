use super::*;
use crate::compositor::query::layer_info_entry;
use crate::compositor::{Compositor, TextureInfo};
use asb_interpreter::event::LayerEvent;
use asb_interpreter::lua_engine::EngineCallbacks;
use std::io::Cursor;
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
        self.scene = Some(SceneIo {
            scene: scene.0.clone(),
            pending: None,
        });
        self.interpreter.set_engine_callbacks(Box::new(scene));
        self.interpreter.set_callback(move |event| match event {
            Event::Layer(_) | Event::LayerRename { .. } => {
                let mut scene = reduced_scene.lock().unwrap();
                if let Some(file) = layer_asset(&event)
                    && !scene.dimensions.contains_key(file)
                {
                    return CallbackResult::Pause;
                }
                scene.compositor.apply_event(&event);
                CallbackResult::Continue
            }
            _ => CallbackResult::Pause,
        });
        Ok(epoch)
    }
}

#[derive(Default)]
struct SceneProjection {
    compositor: Compositor,
    dimensions: HashMap<String, TextureInfo>,
}

impl SceneProjection {
    fn layer_info(&self, layer: &crate::compositor::Layer) -> HashMap<String, String> {
        layer_info_entry(
            layer,
            self.compositor.clock_ms(),
            layer
                .file
                .as_deref()
                .and_then(|file| self.dimensions.get(file))
                .copied(),
        )
    }
}

#[derive(Default)]
struct SceneCallbacks(Arc<Mutex<SceneProjection>>);

pub(super) struct SceneIo {
    scene: Arc<Mutex<SceneProjection>>,
    pending: Option<PendingAsset>,
}

struct PendingAsset {
    event: Event,
    file: String,
    try_exact: bool,
}

impl SceneIo {
    pub(super) fn asset_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn prepare_asset(&mut self, event: &Event) -> Option<IoRequest> {
        let file = layer_asset(event)?;
        self.pending = Some(PendingAsset {
            event: event.clone(),
            file: file.to_owned(),
            try_exact: true,
        });
        Some(IoRequest::Read {
            path: format!("{file}.png"),
        })
    }

    pub(super) fn complete_asset(&mut self, response: IoResponse) -> Option<IoRequest> {
        let pending = self.pending.as_mut().unwrap();
        let dimensions = match &response {
            IoResponse::Read(bytes) => image::ImageReader::new(Cursor::new(bytes))
                .with_guessed_format()
                .ok()
                .and_then(|image| image.into_dimensions().ok()),
            _ => None,
        };
        if dimensions.is_none()
            && pending.try_exact
            && !matches!(
                response,
                IoResponse::Failed {
                    kind: IoFailure::Cancelled,
                    ..
                }
            )
        {
            pending.try_exact = false;
            return Some(IoRequest::Read {
                path: pending.file.clone(),
            });
        }
        let PendingAsset { event, file, .. } = self.pending.take().unwrap();
        let mut scene = self.scene.lock().unwrap();
        if let Some((width, height)) = dimensions {
            scene.dimensions.insert(file, TextureInfo { width, height });
        }
        scene.compositor.apply_event(&event);
        None
    }
}

fn layer_asset(event: &Event) -> Option<&str> {
    match event {
        Event::Layer(LayerEvent::Create { file, .. } | LayerEvent::Create2 { file, .. })
            if !file.is_empty() =>
        {
            Some(file)
        }
        _ => None,
    }
}

impl EngineCallbacks for SceneCallbacks {
    fn get_layer_info(&self, id: &str) -> Option<HashMap<String, String>> {
        let scene = self.0.lock().unwrap();
        let layer = scene.compositor.scene().get(id)?;
        Some(scene.layer_info(layer))
    }

    fn get_layer_info_all(&self) -> Vec<(String, HashMap<String, String>)> {
        let scene = self.0.lock().unwrap();
        let mut layers: Vec<_> = scene.compositor.scene().all_layers().collect();
        layers.sort_unstable_by(|a, b| a.id.cmp(&b.id));
        layers
            .into_iter()
            .map(|layer| (layer.id.clone(), scene.layer_info(layer)))
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
