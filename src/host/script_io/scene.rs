use super::*;
use crate::compositor::query::layer_info_entry;
use crate::compositor::{Compositor, TextureInfo, TweenHandler};
use crate::host::frame::FrameTexture;
use crate::host::input::InputFrame;
use crate::host::media::VideoFrame;
use asb_interpreter::event::{LayerEvent, WaitReason};
use asb_interpreter::lua_engine::EngineCallbacks;
use std::io::Cursor;
use std::sync::{Arc, Mutex};

mod edit;
mod emote;
mod frames;
mod pointer;
mod text;
pub(super) use text::FontRead;

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
        let scene = SceneCallbacks(Arc::new(Mutex::new(SceneProjection {
            dirty: true,
            emote_enabled: self.emote_enabled,
            ..Default::default()
        })));
        let reduced_scene = scene.0.clone();
        self.scene = Some(SceneIo {
            scene: scene.0.clone(),
            pending: None,
            pending_font: None,
            clock_origin_ns: None,
            clock_base_ms: 0,
            tween_handlers: Vec::new(),
            pointer: Default::default(),
        });
        self.interpreter.set_engine_callbacks(Box::new(scene));
        self.interpreter.set_callback(move |event| match event {
            Event::Layer(_)
            | Event::LayerEventHandler { .. }
            | Event::LayerRename { .. }
            | Event::LayerEdit { .. }
            | Event::LayerTweenDelete { .. }
            | Event::Anime { .. } => {
                let mut scene = reduced_scene.lock().unwrap();
                if scene.missing_assets(&event).next().is_some() {
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
            && (!self.audio.ready_handlers.is_empty()
                || !self.video.ready_handlers.is_empty()
                || self.scene.as_ref().is_some_and(|scene| {
                    !scene.tween_handlers.is_empty() || !scene.pointer.tags.is_empty()
                }))
    }

    pub(super) fn dispatch_scene_handlers(&mut self) -> Result<(), ScriptIoError> {
        if !self.scene_handlers_ready() {
            return Ok(());
        }
        let mut tags = std::mem::take(&mut self.audio.ready_handlers);
        tags.append(&mut self.video.ready_handlers);
        if let Some(scene) = self.scene.as_mut() {
            tags.append(&mut scene.pointer.tags);
            tags.extend(scene.tween_handlers.drain(..).flat_map(|h| {
                crate::script_handler::handler_tags(
                    h.handler.as_deref(),
                    h.file.as_deref(),
                    h.label.as_deref(),
                    h.call,
                    &h.extra_params,
                    &[],
                )
            }));
        }
        if !tags.is_empty() {
            if let Some(event) = self.scenario_event.take() {
                self.interpreter
                    .park_wait_for_host_dispatch(&event, self.wait_start.take())?;
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
    input: InputFrame,
    magic_paths: crate::magic_path::MagicPathTable,
    textures: HashMap<String, Option<Arc<FrameTexture>>>,
    texture_bytes: usize,
    texture_revision: u64,
    video_frames: HashMap<String, Arc<FrameTexture>>,
    pending_video_frames: HashMap<String, Arc<FrameTexture>>,
    video_revision: u64,
    edited: edit::LayerEdits,
    text: text::SceneText,
    emote: emote::SceneEmote,
    emote_enabled: bool,
    dirty: bool,
    group_sync_layers: Vec<String>,
}

impl SceneProjection {
    fn apply(&mut self, event: &Event) {
        if let Event::LayerEdit { .. } = event {
            self.dirty |= self
                .edited
                .apply(self.compositor.scene(), &self.textures, event);
            return;
        }
        self.compositor.apply_event(event);
        if let Event::Layer(LayerEvent::Create { id, .. } | LayerEvent::Create2 { id, .. }) = event
        {
            self.edited.remove(id);
        }
        self.edited.retain_bound(self.compositor.scene());
        self.emote.retain(self.compositor.scene());
        self.dirty = true;
    }

    fn missing_assets<'a>(&'a self, event: &'a Event) -> impl Iterator<Item = &'a str> {
        let files = match event {
            Event::Layer(LayerEvent::Create { file, .. } | LayerEvent::Create2 { file, .. }) => {
                [Some(file.as_str()), None]
            }
            Event::Layer(LayerEvent::SetProperties { id, properties }) => {
                let shader = properties
                    .get("shader")
                    .map(String::as_str)
                    .or_else(|| self.compositor.scene().get(id)?.props.shader.as_deref());
                let mask = shader
                    .is_none_or(str::is_empty)
                    .then(|| properties.get("mask").map(String::as_str))
                    .flatten();
                [
                    mask,
                    properties
                        .get("intermediate_render_mask")
                        .map(String::as_str)
                        .filter(|file| Some(*file) != mask),
                ]
            }
            Event::Anime {
                mode, file, mask, ..
            } if matches!(mode.as_str(), "init" | "add") => [
                file.as_deref(),
                mask.as_deref()
                    .filter(|mask| Some(*mask) != file.as_deref()),
            ],
            Event::LayerEdit { mode, file, .. } if mode == "blend" => [file.as_deref(), None],
            _ => [None, None],
        };
        files
            .into_iter()
            .flatten()
            .filter(|file| !file.is_empty() && !self.textures.contains_key(*file))
    }

    fn layer_info(&self, layer: &crate::compositor::Layer) -> HashMap<String, String> {
        layer_info_entry(
            layer,
            self.compositor.clock_ms(),
            self.video_frames
                .get(&layer.id)
                .or_else(|| {
                    layer
                        .file
                        .as_deref()
                        .and_then(|file| self.textures.get(file)?.as_ref())
                })
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
    pending_font: Option<(Event, FontRead)>,
    clock_origin_ns: Option<u64>,
    clock_base_ms: u64,
    tween_handlers: Vec<TweenHandler>,
    pointer: pointer::PointerDispatch,
}

struct PendingAsset {
    event: Event,
    read: TextureRead,
    remaining: Option<String>,
}

pub(super) enum AssetProgress {
    More(IoRequest),
    Transition(Event),
    Applied,
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
    pub(super) fn invalidate_texture(&self, key: &str) {
        let mut scene = self.scene.lock().unwrap();
        for key in [Some(key), key.strip_suffix(".png")].into_iter().flatten() {
            if let Some(Some(texture)) = scene.textures.remove(key) {
                scene.texture_bytes -= texture.rgba().len();
            }
        }
    }

    pub(super) fn transition_rule(&self, name: &str) -> Option<Arc<FrameTexture>> {
        let key = self.resolve_media_asset(name).ok()?;
        self.scene.lock().unwrap().textures.get(&key)?.clone()
    }

    pub(super) fn attach_video_layer(&self, id: &str) {
        let mut scene = self.scene.lock().unwrap();
        scene.compositor.set_layer_file(id, None);
        scene.edited.remove(id);
        scene.video_frames.remove(id);
        scene.pending_video_frames.remove(id);
        scene.dirty = true;
    }

    pub(super) fn clear_video_layer(&self, id: &str) {
        let mut scene = self.scene.lock().unwrap();
        scene.pending_video_frames.remove(id);
        scene.dirty |= scene.video_frames.remove(id).is_some();
    }

    pub(super) fn submit_video_pixels(
        &self,
        id: &str,
        frame: VideoFrame<'_>,
    ) -> Result<(), ProtocolError> {
        let [width, height] = [frame.width, frame.height];
        if width == 0
            || height == 0
            || width > 8192
            || height > 8192
            || frame.rgba.len() as u64 != u64::from(width) * u64::from(height) * 4
        {
            return Err(ProtocolError::Argument("invalid video frame size"));
        }
        if frame.rgba.len() as u64 > MAX_IMAGE_BYTES {
            return Err(ProtocolError::Limit);
        }
        let mut scene = self.scene.lock().unwrap();
        let used: usize = scene
            .video_frames
            .values()
            .chain(
                scene
                    .pending_video_frames
                    .iter()
                    .filter_map(|(layer, pixels)| (layer != id).then_some(pixels)),
            )
            .map(|texture| texture.rgba.len())
            .sum();
        if frame.rgba.len() > MAX_TEXTURE_BYTES - used {
            return Err(ProtocolError::WouldBlock);
        }
        let revision = scene
            .video_revision
            .checked_add(1)
            .filter(|id| *id < 1 << 62)
            .ok_or(ProtocolError::Limit)?;
        scene.video_revision = revision;
        scene.pending_video_frames.insert(
            id.to_owned(),
            Arc::new(FrameTexture {
                revision: (1 << 62) | revision,
                width,
                height,
                rgba: frame.rgba.into(),
            }),
        );
        Ok(())
    }

    pub(super) fn advance_video_frames(&self) {
        let mut scene = self.scene.lock().unwrap();
        let pending = std::mem::take(&mut scene.pending_video_frames);
        if !pending.is_empty() {
            scene.video_frames.extend(pending);
            scene.dirty = true;
        }
    }

    pub(super) fn resolve_media_asset(&self, name: &str) -> Result<String, ProtocolError> {
        let scene = self.scene.lock().unwrap();
        logical_key(&crate::magic_path::resolve_path(&scene.magic_paths, name))
    }

    pub(super) fn with_input<R>(&self, f: impl FnOnce(&mut InputFrame) -> R) -> R {
        f(&mut self.scene.lock().unwrap().input)
    }

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
        scene.dirty |= scene.text.advance(delta_ms);
        scene.dirty |= scene.emote.advance(delta_ms);
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
        scene.video_frames.clear();
        scene.pending_video_frames.clear();
        scene.edited.clear();
        scene.emote.clear();
        self.tween_handlers.clear();
        self.pointer = Default::default();
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
        if let Event::Trans {
            trans_type: 2,
            rule: Some(rule),
            ..
        } = event
        {
            let key = self.resolve_media_asset(rule).ok()?;
            if self.scene.lock().unwrap().textures.contains_key(&key) {
                return None;
            }
            let (read, request) = TextureRead::start(key);
            self.pending = Some(PendingAsset {
                event: event.clone(),
                read,
                remaining: None,
            });
            return Some(request);
        }
        let scene = self.scene.lock().unwrap();
        let mut files = scene.missing_assets(event);
        let file = files.next()?;
        let (read, request) = TextureRead::start(file.to_owned());
        self.pending = Some(PendingAsset {
            event: event.clone(),
            read,
            remaining: files.next().map(str::to_owned),
        });
        Some(request)
    }

    pub(super) fn complete_asset(&mut self, response: IoResponse) -> AssetProgress {
        let mut pending = self.pending.take().unwrap();
        let cancelled = matches!(
            &response,
            IoResponse::Failed {
                kind: IoFailure::Cancelled,
                ..
            }
        );
        if let Some(request) = self.complete_texture(&mut pending.read, response) {
            self.pending = Some(pending);
            return AssetProgress::More(request);
        }
        if !cancelled && let Some(file) = pending.remaining.take() {
            let (read, request) = TextureRead::start(file);
            pending.read = read;
            self.pending = Some(pending);
            return AssetProgress::More(request);
        }
        if matches!(pending.event, Event::Trans { .. }) {
            return AssetProgress::Transition(pending.event);
        }
        self.scene.lock().unwrap().apply(&pending.event);
        AssetProgress::Applied
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
            let texture = image.and_then(|image| {
                let (width, height) = image.dimensions();
                let rgba = image.into_raw().into_boxed_slice();
                let revision = scene
                    .texture_revision
                    .checked_add(1)
                    .filter(|id| *id < emote::EMOTE_TEXTURE_BASE)?;
                scene.texture_revision = revision;
                scene.texture_bytes += rgba.len();
                Some(Arc::new(FrameTexture {
                    revision,
                    width,
                    height,
                    rgba,
                }))
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
    fn resolve_emote_model_path(&self, path: &str) -> asb_interpreter::Result<String> {
        self.resolve_emote_path(path)
    }

    fn create_emote_layer_from_bytes(
        &self,
        id: &str,
        file: &str,
        bytes: Vec<u8>,
        width: u32,
        height: u32,
    ) -> asb_interpreter::Result<bool> {
        self.create_emote_model(id, file, bytes, [width, height])
    }

    fn get_emote_layer(&self, id: &str, next: bool) -> Option<bool> {
        self.get_emote_model(id, next)
    }

    fn command_emote_layer(
        &self,
        id: &str,
        next: bool,
        command: asb_interpreter::EmoteLayerCommand,
    ) -> asb_interpreter::Result<()> {
        self.update_emote_model(id, next, command)
    }

    fn set_magic_path(&self, name: &str, path: &str) {
        let scene = self.0.lock().unwrap();
        let mut paths = scene.magic_paths.lock().unwrap();
        if path.is_empty() {
            paths.remove(name);
        } else {
            paths.insert(name.to_owned(), path.to_owned());
        }
    }

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
    fn is_key_down(&self, code: u32) -> bool {
        self.0.lock().unwrap().input.is_down(code)
    }
    fn is_key_down_edge(&self, code: u32) -> bool {
        self.0.lock().unwrap().input.is_pressed(code)
    }
    fn is_key_up_edge(&self, code: u32) -> bool {
        self.0.lock().unwrap().input.is_released(code)
    }
    fn is_decide(&self) -> bool {
        self.is_decide_key(1)
    }
    fn is_decide_key(&self, code: u32) -> bool {
        self.is_key_down_edge(code)
    }
    fn is_push(&self, code: u32) -> bool {
        self.0.lock().unwrap().input.is_push(code)
    }
    fn get_mouse_point(&self) -> (i32, i32) {
        self.0.lock().unwrap().input.pointer
    }
    fn get_touch_count(&self) -> u32 {
        self.0.lock().unwrap().input.touches.len() as u32
    }
    fn get_touch_point(&self, index: u32) -> (i32, i32) {
        self.0
            .lock()
            .unwrap()
            .input
            .touches
            .get(index as usize)
            .map_or((0, 0), |&(_, x, y)| (x, y))
    }
    fn get_touch_points(&self) -> Vec<(u32, i32, i32)> {
        self.0.lock().unwrap().input.touches.clone()
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
