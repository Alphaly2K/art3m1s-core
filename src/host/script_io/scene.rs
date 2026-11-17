use super::*;
use crate::compositor::query::layer_info_entry;
use crate::compositor::{Compositor, TextureInfo, TweenHandler};
use crate::host::frame::FrameTexture;
use asb_interpreter::event::{LayerEvent, WaitReason};
use asb_interpreter::lua_engine::EngineCallbacks;
use std::io::Cursor;
use std::sync::{Arc, Mutex};

mod frames;

const MAX_TEXTURES: usize = 4096;
const MAX_TEXTURE_BYTES: usize = 128 * 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

impl ScriptIoSession {
    pub(in crate::host) fn open_queued_project(
        &mut self,
        ini: &[u8],
        platform: &str,
        reported_os: Option<&str>,
    ) -> Result<u64, ScriptIoError> {
        let epoch = self.open_project(ini, platform, reported_os)?;
        let scene = SceneCallbacks::default();
        scene.0.lock().unwrap().dirty = true;
        let reduced_scene = scene.0.clone();
        self.scene = Some(SceneIo {
            scene: scene.0.clone(),
            pending: None,
            clock_origin_ns: None,
            clock_base_ms: 0,
            tween_handlers: Vec::new(),
        });
        self.interpreter.set_engine_callbacks(Box::new(scene));
        self.interpreter.set_callback(move |event| match event {
            Event::Layer(_) | Event::LayerRename { .. } | Event::LayerTweenDelete { .. } => {
                let mut scene = reduced_scene.lock().unwrap();
                if scene.missing_asset(&event).is_some() {
                    return CallbackResult::Pause;
                }
                scene.apply(&event);
                CallbackResult::Continue
            }
            Event::TweenSetStart => {
                let mut scene = reduced_scene.lock().unwrap();
                scene.group_sync_layers.clear();
                scene.apply(&event);
                CallbackResult::Continue
            }
            Event::TweenSetEnd => {
                let mut scene = reduced_scene.lock().unwrap();
                scene.apply(&event);
                if scene.group_sync_layers.is_empty() {
                    CallbackResult::Continue
                } else {
                    CallbackResult::Pause
                }
            }
            Event::LayerTween { sync, ref id, .. } => {
                let mut scene = reduced_scene.lock().unwrap();
                scene.apply(&event);
                if !sync {
                    CallbackResult::Continue
                } else if scene.compositor.collecting_tweens() {
                    scene.group_sync_layers.push(id.clone());
                    CallbackResult::Continue
                } else {
                    CallbackResult::Pause
                }
            }
            _ => CallbackResult::Pause,
        });
        Ok(epoch)
    }

    pub(super) fn scene_handlers_ready(&self) -> bool {
        matches!(self.shutdown, Shutdown::Open)
            && matches!(self.scenario_event, None | Some(Event::Wait { .. }))
            && self
                .scene
                .as_ref()
                .is_some_and(|scene| !scene.tween_handlers.is_empty())
    }

    pub(super) fn dispatch_scene_handlers(&mut self) -> Result<(), ScriptIoError> {
        if !self.scene_handlers_ready() {
            return Ok(());
        }
        let tags: Vec<_> = self
            .scene
            .as_mut()
            .unwrap()
            .tween_handlers
            .drain(..)
            .flat_map(|h| {
                crate::script_handler::handler_tags(
                    h.handler.as_deref(),
                    h.file.as_deref(),
                    h.label.as_deref(),
                    h.call,
                    &h.extra_params,
                    &[],
                )
            })
            .collect();
        if !tags.is_empty() {
            if let Some(event) = self.scenario_event.take() {
                self.interpreter
                    .park_wait_for_host_dispatch(&event, self.timed_wait_start.take())?;
            }
            self.interpreter.append_queued_tags(tags);
            self.completed = false;
        }
        Ok(())
    }

    pub(super) fn sync_tween_finished(&self) -> bool {
        let (
            Some(scene),
            Some(Event::Wait {
                reason: WaitReason::LayerTweens { layers },
            }),
        ) = (&self.scene, &self.scenario_event)
        else {
            return false;
        };
        let scene = scene.scene.lock().unwrap();
        layers.iter().all(|id| {
            scene
                .compositor
                .scene()
                .get(id)
                .is_none_or(|layer| layer.tweens.is_empty())
        })
    }
}

#[derive(Default)]
struct SceneProjection {
    compositor: Compositor,
    textures: HashMap<String, Option<Arc<FrameTexture>>>,
    texture_bytes: usize,
    dirty: bool,
    group_sync_layers: Vec<String>,
}

impl SceneProjection {
    fn apply(&mut self, event: &Event) {
        self.compositor.apply_event(event);
        self.dirty = true;
    }

    fn missing_asset<'a>(&self, event: &'a Event) -> Option<&'a str> {
        let file = match event {
            Event::Layer(LayerEvent::Create { file, .. } | LayerEvent::Create2 { file, .. }) => {
                file
            }
            Event::Layer(LayerEvent::SetProperties { id, properties }) => {
                let shader = properties
                    .get("shader")
                    .map(String::as_str)
                    .or_else(|| self.compositor.scene().get(id)?.props.shader.as_deref());
                if shader.is_some_and(|shader| !shader.is_empty()) {
                    return None;
                }
                properties.get("mask")?
            }
            _ => return None,
        };
        (!file.is_empty() && !self.textures.contains_key(file)).then_some(file)
    }

    fn layer_info(&self, layer: &crate::compositor::Layer) -> HashMap<String, String> {
        layer_info_entry(
            layer,
            self.compositor.clock_ms(),
            layer
                .file
                .as_deref()
                .and_then(|file| self.textures.get(file)?.as_ref())
                .map(|texture| TextureInfo {
                    width: texture.width,
                    height: texture.height,
                }),
        )
    }
}

#[derive(Default)]
struct SceneCallbacks(Arc<Mutex<SceneProjection>>);

pub(super) struct SceneIo {
    scene: Arc<Mutex<SceneProjection>>,
    pending: Option<PendingAsset>,
    clock_origin_ns: Option<u64>,
    clock_base_ms: u64,
    tween_handlers: Vec<TweenHandler>,
}

struct PendingAsset {
    event: Event,
    read: TextureRead,
}

pub(super) struct TextureRead {
    file: String,
    try_exact: bool,
}

impl TextureRead {
    pub(super) fn start(file: String) -> (Self, IoRequest) {
        let request = IoRequest::Read {
            path: format!("{file}.png"),
        };
        (
            Self {
                file,
                try_exact: true,
            },
            request,
        )
    }
}

impl SceneIo {
    pub(super) fn take_scene_wait(&self, event: Event) -> Event {
        let layers = match event {
            Event::LayerTween { id, sync: true, .. } => vec![id],
            Event::TweenSetEnd => std::mem::take(&mut self.scene.lock().unwrap().group_sync_layers),
            other => return other,
        };
        Event::Wait {
            reason: WaitReason::LayerTweens { layers },
        }
    }

    pub(super) fn advance_clock(&mut self, monotonic_ns: u64) {
        let origin = *self.clock_origin_ns.get_or_insert(monotonic_ns);
        let elapsed_ms = self
            .clock_base_ms
            .saturating_add((monotonic_ns - origin) / 1_000_000);
        let mut scene = self.scene.lock().unwrap();
        let delta_ms = elapsed_ms - scene.compositor.clock_ms();
        scene.dirty |= scene.compositor.advance(delta_ms);
        self.tween_handlers.extend(
            scene
                .compositor
                .poll_tween_events()
                .into_iter()
                .filter(|h| h.handler.is_some() || h.file.is_some() || h.label.is_some()),
        );
    }

    pub(super) fn asset_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn save_snapshot(&self) -> crate::compositor::Scene {
        self.scene.lock().unwrap().compositor.snapshot_for_host()
    }

    pub(super) fn restore_snapshot(
        &mut self,
        snapshot: Option<crate::compositor::Scene>,
        frame: Option<HostFrame>,
    ) {
        let mut scene = self.scene.lock().unwrap();
        scene
            .compositor
            .restore_host_snapshot(snapshot.unwrap_or_default());
        self.clock_base_ms = scene.compositor.clock_ms();
        self.clock_origin_ns = frame.map(|frame| frame.monotonic_ns);
        scene.group_sync_layers.clear();
        self.tween_handlers.clear();
        scene.dirty = true;
    }

    pub(super) fn missing_snapshot_textures(
        &self,
        snapshot: &crate::compositor::Scene,
    ) -> VecDeque<String> {
        let scene = self.scene.lock().unwrap();
        snapshot
            .all_layers()
            .flat_map(|layer| [layer.file.as_deref(), layer.mask.as_deref()])
            .flatten()
            .chain(snapshot.anime_states.values().flat_map(|state| {
                state
                    .frames
                    .iter()
                    .flat_map(|frame| [Some(frame.file.as_str()), frame.mask.as_deref()])
                    .flatten()
            }))
            .filter(|file| !file.is_empty() && !scene.textures.contains_key(*file))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    pub(super) fn prepare_asset(&mut self, event: &Event) -> Option<IoRequest> {
        let scene = self.scene.lock().unwrap();
        let file = scene.missing_asset(event)?;
        let (read, request) = TextureRead::start(file.to_owned());
        self.pending = Some(PendingAsset {
            event: event.clone(),
            read,
        });
        Some(request)
    }

    pub(super) fn complete_asset(&mut self, response: IoResponse) -> Option<IoRequest> {
        let mut pending = self.pending.take().unwrap();
        if let Some(request) = self.complete_texture(&mut pending.read, response) {
            self.pending = Some(pending);
            return Some(request);
        }
        self.scene.lock().unwrap().apply(&pending.event);
        None
    }

    pub(super) fn complete_texture(
        &self,
        read: &mut TextureRead,
        response: IoResponse,
    ) -> Option<IoRequest> {
        let mut scene = self.scene.lock().unwrap();
        let image = match &response {
            IoResponse::Read(bytes) if scene.textures.len() < MAX_TEXTURES => {
                decode_texture(bytes, MAX_TEXTURE_BYTES - scene.texture_bytes)
            }
            _ => None,
        };
        if image.is_none()
            && read.try_exact
            && !matches!(
                response,
                IoResponse::Failed {
                    kind: IoFailure::Cancelled,
                    ..
                }
            )
        {
            read.try_exact = false;
            return Some(IoRequest::Read {
                path: read.file.clone(),
            });
        }
        if scene.textures.len() < MAX_TEXTURES {
            let texture = image.map(|image| {
                let (width, height) = image.dimensions();
                let rgba = image.into_raw().into_boxed_slice();
                scene.texture_bytes += rgba.len();
                Arc::new(FrameTexture {
                    revision: scene.textures.len() as u64 + 1,
                    width,
                    height,
                    rgba,
                })
            });
            scene
                .textures
                .insert(std::mem::take(&mut read.file), texture);
        }
        None
    }
}

fn decode_texture(bytes: &[u8], available: usize) -> Option<image::RgbaImage> {
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(MAX_IMAGE_BYTES);
    reader.limits(limits);
    let image = reader.decode().ok()?;
    let rgba_len = u64::from(image.width()) * u64::from(image.height()) * 4;
    (rgba_len <= MAX_IMAGE_BYTES && rgba_len <= available as u64).then(|| image.into_rgba8())
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
