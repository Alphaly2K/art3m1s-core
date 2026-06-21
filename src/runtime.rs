//! Core runtime — wires together GL context, compositor, interpreter,
//! text rendering and input handling into a single frame-oriented API
//! that the Flutter frontend calls from its game loop.

use crate::Project;
use crate::backend::gl::platform::{self, GfxBackend};
use crate::backend::gl::{GlRenderer, GlTextureProvider, ShaderProfile};
use crate::compositor::Compositor;
use crate::compositor::renderer::TextureProvider;
use crate::ffi_callbacks::{FfiCallbacks, InputSnapshot, MagicPathTable};
use crate::text::GlyphTextRenderer;
use asb_interpreter::event::WaitReason;
use asb_interpreter::{CallbackResult, Event, ExecutionResult};
use glow::HasContext;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

pub struct CoreRuntime {
    gl: Rc<glow::Context>,
    _gl_ctx: Box<dyn platform::GLPlatformContext>,
    _fbo: glow::Framebuffer,
    _fbo_tex: glow::Texture,

    renderer: GlRenderer,
    texture_provider: GlTextureProvider,
    compositor: Compositor,
    interpreter: asb_interpreter::Interpreter,
    input: Arc<Mutex<InputSnapshot>>,
    events: Arc<Mutex<Vec<Event>>>,
    video_finished: Arc<AtomicBool>,
    magic_paths: Arc<MagicPathTable>,

    stage_w: u32,
    stage_h: u32,
    wait_reason: Option<WaitReason>,
    timed_remaining_ms: u64,
    hovered_layer: Option<String>,
    volumes: Arc<Mutex<HashMap<String, f32>>>,
    exit_requested: Arc<AtomicBool>,
}

impl CoreRuntime {
    /// Create a new runtime with the given rendering backend.
    pub fn create(
        stage_width: u32,
        stage_height: u32,
        backend: GfxBackend,
    ) -> Result<Self, String> {
        let (gl, gl_ctx, effective_backend) =
            platform::create_offscreen_context(backend, stage_width, stage_height)?;

        let (fbo, fbo_tex) = unsafe {
            platform::create_fbo_target(&gl, stage_width as i32, stage_height as i32)
                .map_err(|e| format!("FBO: {e}"))?
        };

        let profile = match effective_backend {
            GfxBackend::Cgl => ShaderProfile::GlCore330,
            GfxBackend::Angle(_) => ShaderProfile::Gles300,
        };
        let renderer = GlRenderer::new(gl.clone(), stage_width, stage_height, profile)
            .map_err(|e| format!("创建渲染器失败: {e}"))?;

        let texture_provider = GlTextureProvider::new(gl.clone()).with_ffi_source();

        let compositor = Compositor::new();
        let interpreter =
            asb_interpreter::Interpreter::new(asb_interpreter::InterpreterConfig::default());

        let input = Arc::new(Mutex::new(InputSnapshot::default()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let video_finished = Arc::new(AtomicBool::new(false));
        let magic_paths: Arc<MagicPathTable> = Arc::new(Mutex::new(HashMap::new()));

        Ok(Self {
            gl,
            _gl_ctx: gl_ctx,
            _fbo: fbo,
            _fbo_tex: fbo_tex,
            renderer,
            texture_provider,
            compositor,
            interpreter,
            input,
            events,
            video_finished,
            magic_paths: Arc::clone(&magic_paths),
            stage_w: stage_width,
            stage_h: stage_height,
            wait_reason: None,
            timed_remaining_ms: 0,
            hovered_layer: None,
            volumes: Arc::new(Mutex::new(HashMap::new())),
            exit_requested: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Load a project from an in-memory system.ini string.
    pub fn load_project(&mut self, ini_content: &str, platform: &str) -> Result<(), String> {
        let project =
            Project::open_from_data("", ini_content, platform).map_err(|e| e.to_string())?;

        self.stage_w = project.config().stage_width;
        self.stage_h = project.config().stage_height;
        self.interpreter = project.create_interpreter();

        // Wire callbacks
        let input_cb = Arc::clone(&self.input);
        let _magic_paths_cb = Arc::clone(&self.magic_paths);
        self.interpreter
            .set_engine_callbacks(Box::new(FfiCallbacks {
                input: input_cb,
                magic_paths: Arc::clone(&self.magic_paths),
                volumes: Arc::clone(&self.volumes),
            }));

        // Wire audio file loading through FFI with magic-path resolution
        let magic_paths_audio = Arc::clone(&self.magic_paths);
        crate::audio::rodio::set_audio_file_reader(Box::new(
            move |path: &str| -> Option<Vec<u8>> {
                let resolved = resolve_texture_path(&magic_paths_audio, path);
                crate::ffi::request_asset(&resolved)
            },
        ));

        // Override the file loader with magic-path-aware FFI version.
        // Scripts can reference files via `:name/rest` notation; the
        // default loader (from create_interpreter) doesn't resolve these.
        let magic_paths_loader = Arc::clone(&self.magic_paths);
        self.interpreter
            .set_file_loader(Box::new(move |name: &str| {
                let resolved = resolve_texture_path(&magic_paths_loader, name);
                crate::ffi::request_file(&resolved).map_err(|m| {
                    asb_interpreter::Error::IoError(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        m,
                    ))
                })
            }));

        // Re-create texture provider with magic-path-aware FFI source
        let gl_for_tex = self.gl.clone();
        let project_name = ini_content
            .lines()
            .find(|l| l.starts_with("TITLE="))
            .and_then(|l| l.split_once('=').map(|(_, v)| v.trim().to_string()))
            .unwrap_or_default();
        let magic_paths_tex = Arc::clone(&self.magic_paths);
        self.texture_provider =
            GlTextureProvider::new(gl_for_tex).with_source(move |name: &str| -> Option<Vec<u8>> {
                let resolved = resolve_texture_path(&magic_paths_tex, name);
                for try_path in [format!("{resolved}.png"), resolved.clone()] {
                    match crate::ffi::request_asset(&try_path) {
                        Some(bytes) => {
                            crate::core_debug!(
                                "[{project_name}] TEX HIT: {name} → {try_path} ({})",
                                bytes.len()
                            );
                            return Some(bytes);
                        }
                        None => {
                            crate::core_debug!(
                                "[{project_name}] TEX TRY: {name} → {try_path} MISS"
                            );
                        }
                    }
                }
                crate::core_debug!("[{project_name}] TEX MISS: {name} → {resolved}");
                None
            });

        // Wire events
        let events_cb = Arc::clone(&self.events);
        let exit_requested_cb = Arc::clone(&self.exit_requested);
        self.interpreter.set_callback(move |e| {
            if matches!(e, Event::Exit) {
                crate::core_info!("[CoreRuntime] Event::Exit received, setting exit flag");
                exit_requested_cb.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            // Pause script execution on VideoPlay, video completion is signalled via
            // video_finished atomic → WaitReason::Stop handler resumes.
            let pause = matches!(
                e,
                Event::Wait { .. }
                    | Event::YesNo { .. }
                    | Event::ShowDialog { .. }
                    | Event::VideoPlay { .. }
            );
            events_cb.lock().unwrap().push(e);
            if pause {
                CallbackResult::Pause
            } else {
                CallbackResult::Continue
            }
        });

        // Load font
        match crate::load_font_ffi("font/sourcehansans-medium.otf") {
            Ok(font) => {
                let mut text = GlyphTextRenderer::new();
                let _ = text.set_font(font);
                self.compositor.set_text_renderer(Box::new(text));
            }
            Err(e) => {
                crate::core_warn!("[CoreRuntime] 字体加载失败: {e}");
            }
        }

        // Pre-register solid color textures
        self.texture_provider
            .upload_rgba(":bg/black", 2, 2, &[0, 0, 0, 255].repeat(4));
        self.texture_provider
            .upload_rgba(":bg/white", 2, 2, &[255, 255, 255, 255].repeat(4));

        // Boot
        project
            .start_boot(&mut self.interpreter)
            .map_err(|e| e.to_string())?;

        Ok(())
    }

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

    /// Advance logic and render one frame. Returns the RGBA pixel buffer.
    /// The caller owns the returned `Vec<u8>`.
    pub fn advance_and_render(&mut self, delta_ms: u64) -> Vec<u8> {
        // Read & clear per-frame click state
        let (clicked, mouse_x, mouse_y) = {
            let mut s = self.input.lock().unwrap();
            let v = s.clicked;
            s.clicked = false;
            (v, s.mouse_x as f32, s.mouse_y as f32)
        };

        // hit test
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

        // onEnterFrame
        if let Err(e) = self.interpreter.fire_enter_frame() {
            crate::core_error!("onEnterFrame 错误: {e:?}");
        }

        // Drain queued tags
        let has_tags = {
            let ctx = self.interpreter.engine_context();
            !ctx.lock().unwrap().tag_queue.is_empty()
        };
        if has_tags {
            self.wait_reason = None;
        }

        // Run interpreter
        if self.wait_reason.is_none() {
            loop {
                match self.interpreter.run() {
                    Ok(ExecutionResult::Wait(Event::Wait { reason })) => {
                        match &reason {
                            WaitReason::Timed { milliseconds } => {
                                self.timed_remaining_ms = *milliseconds;
                            }
                            WaitReason::Stop { .. } => {}
                            _ => {}
                        }
                        self.wait_reason = Some(reason);
                        break;
                    }
                    Ok(ExecutionResult::Wait(Event::VideoPlay { .. })) => {
                        self.wait_reason = Some(WaitReason::Stop {
                            reason: Some("video".into()),
                        });
                        break;
                    }
                    Ok(ExecutionResult::Wait(_)) => {
                        self.wait_reason = Some(WaitReason::Generic);
                        break;
                    }
                    Ok(ExecutionResult::Completed) | Ok(_) => break,
                    Err(e) => {
                        crate::core_error!("解释器错误: {e:?}");
                        break;
                    }
                }
            }
        } else if let Some(ref reason) = self.wait_reason {
            let video_resume = matches!(reason, WaitReason::Stop { .. })
                && self
                    .video_finished
                    .swap(false, std::sync::atomic::Ordering::SeqCst);
            if video_resume {
                self.wait_reason = None;
            } else {
                let advance = match reason {
                    WaitReason::Timed { .. } => {
                        if delta_ms >= self.timed_remaining_ms {
                            self.timed_remaining_ms = 0;
                            true
                        } else {
                            self.timed_remaining_ms -= delta_ms;
                            false
                        }
                    }
                    WaitReason::Stop { .. } => false,
                    _ => !has_tags && clicked,
                };
                if advance {
                    self.wait_reason = None;
                    self.interpreter.advance_line();
                }
            }
        }

        // Collect & apply events
        let collected: Vec<_> = {
            let mut ev = self.events.lock().unwrap();
            let v: Vec<_> = ev.drain(..).collect();
            v
        };

        // 输出视频相关事件日志（始终输出）
        for event in &collected {
            match event {
                Event::VideoPlay { id, file, .. } => {
                    crate::core_debug!("[runtime] VideoPlay: file={}, id={:?}", file, id);
                }
                Event::VideoFinishHandler { file, label, call, handler } => {
                    crate::core_info!("[runtime] VideoFinishHandler: file={:?}, label={:?}, call={}, handler={:?}",
                        file, label, call, handler);
                }
                Event::VideoFinishHandlerDel => {
                    crate::core_info!("[runtime] VideoFinishHandlerDel");
                }
                _ => {}
            }
        }

        for event in &collected {
            if matches!(event, Event::Exit) {
                crate::core_info!("[runtime] Event::Exit received");
                self.exit_requested
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }

            // 存档/读档事件日志
            match event {
                Event::SaveGame { file } if !file.is_empty() => {
                    crate::core_info!("[runtime] 保存存档: {}", file);
                }
                Event::LoadGame { file, .. } if !file.is_empty() => {
                    crate::core_info!("[runtime] 读取存档: {}", file);
                }
                _ => {}
            }

            self.compositor.apply_event(event);
            crate::core_debug!("[event] {}", event_name(event));
        }

        // Apply volume from Artemis system variables (s.bgmvol / s.sevol, range 0-1000)
        // and from Lua e:set*Volume() callbacks
        if let Some(ref mut audio) = *self.compositor.audio_mut() {
            let vars = self.interpreter.variables_handle();
            let vars = vars.lock().unwrap();
            if let Some(asb_interpreter::Value::Int(bgm)) = vars.get("s.bgmvol") {
                audio.set_bgm_volume((*bgm as f32 / 1000.0).clamp(0.0, 1.0));
            }
            if let Some(asb_interpreter::Value::Int(se)) = vars.get("s.sevol") {
                audio.set_se_volume((*se as f32 / 1000.0).clamp(0.0, 1.0));
            }
            drop(vars);

            let mut pending = self.volumes.lock().unwrap();
            if let Some(v) = pending.remove("master") {
                audio.set_master_volume(v);
            }
            if let Some(v) = pending.remove("bgm") {
                audio.set_bgm_volume(v);
            }
            if let Some(v) = pending.remove("se") {
                audio.set_se_volume(v);
            }
            if let Some(v) = pending.remove("voice") {
                audio.set_voice_volume(v);
            }
        }

        self.compositor.advance(delta_ms);

        // Poll video finish events and handle them
        let video_finish_events = self.compositor.poll_video_finish_events();
        for event in video_finish_events {
            // Set video_finished flag for backward compatibility
            self.video_finished
                .store(true, std::sync::atomic::Ordering::SeqCst);

            // Enqueue handler tags if registered
            if let Some(handler) = event.handler {
                enqueue_handler_tags(
                    &self.interpreter,
                    handler.handler.as_deref(),
                    handler.file.as_deref(),
                    handler.label.as_deref(),
                    handler.call,
                    &HashMap::new(),
                    &[],
                );
            }
        }

        self.compositor
            .render(&mut self.renderer, &mut self.texture_provider);
        let mut used_files = self.compositor.scene().collect_files();
        used_files.insert(":text/atlas".to_string());
        used_files.insert("__video_fullscreen__".to_string());
        self.texture_provider.retain(&used_files);
        unsafe {
            self.gl.finish();
        }

        let pixels =
            unsafe { platform::read_pixels(&self.gl, self.stage_w as i32, self.stage_h as i32) };

        self.input.lock().unwrap().clear_edges();

        pixels
    }

    pub fn set_volume(&self, volume_type: &str, value: f32) {
        if let Some(ref mut audio) = *self.compositor.audio_mut() {
            let v = value.clamp(0.0, 1.0);
            match volume_type {
                "master" => audio.set_master_volume(v),
                "bgm" => audio.set_bgm_volume(v),
                "se" => audio.set_se_volume(v),
                "voice" => audio.set_voice_volume(v),
                _ => {}
            }
        }
    }

    pub fn is_exit_requested(&self) -> bool {
        self.exit_requested
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

// ── Event dispatch helpers ──────────────────────────────────────

fn enqueue_handler_tags(
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

/// Resolve a texture name that may contain Artemis `:prefix/rest` magic
/// path notation into a plain relative path suitable for the FFI file
/// reader callback.
///
/// - `:bg/room` → lookup "bg" in the magic-path table → `image/bg/room`
/// - `:fa/char` → lookup "fa" → `image/fg/char`
/// - `image/thumb/xxx` → passed through as-is
///
/// The table is populated by the script engine during boot (via
/// `e:setMagicPath`).  Names without a `:` prefix are returned unchanged.
fn resolve_texture_path(table: &MagicPathTable, name: &str) -> String {
    if let Some(rest) = name.strip_prefix(':') {
        let (ns, tail) = rest.split_once('/').unwrap_or((rest, ""));
        let map = table.lock().unwrap();
        if let Some(prefix) = map.get(ns) {
            return format!("{prefix}/{tail}");
        }
        return format!("image/{rest}");
    }
    name.to_string()
}

fn event_name(e: &Event) -> &str {
    match e {
        Event::Layer(layer_event) => match layer_event {
            asb_interpreter::event::LayerEvent::Create { .. } => "LayerCreate",
            asb_interpreter::event::LayerEvent::Delete { .. } => "LayerDelete",
            asb_interpreter::event::LayerEvent::SetProperty { .. } => "LayerSetProp",
            asb_interpreter::event::LayerEvent::SetProperties { .. } => "LayerSetProps",
            _ => "Layer",
        },
        Event::LayerTween { .. } => "LayerTween",
        Event::LayerTweenDelete { .. } => "LayerTweenDel",
        Event::LayerRename { .. } => "LayerRename",
        Event::LayerEventHandler { .. } => "LayerEvtHandler",
        Event::UiTransition(_) => "UiTrans",
        Event::Trans { .. } => "Trans",
        Event::Flip => "Flip",
        Event::BgmPlay { .. } => "BgmPlay",
        Event::BgmStop { .. } => "BgmStop",
        Event::BgmFade { .. } => "BgmFade",
        Event::BgmCrossFade { .. } => "BgmCrossFade",
        Event::SePlay { .. } => "SePlay",
        Event::SeStop { .. } => "SeStop",
        Event::SeFade { .. } => "SeFade",
        Event::VoicePlay { .. } => "VoicePlay",
        Event::StopAllSounds { .. } => "StopAllSounds",
        Event::SoundFinishHandler { .. } => "SoundFinishHandler",
        Event::SoundFinishHandlerDel { .. } => "SoundFinishHandlerDel",
        Event::VideoPlay { .. } => "VideoPlay",
        Event::VideoFinishHandler { .. } => "VideoFinishHandler",
        Event::VideoFinishHandlerDel => "VideoFinishHandlerDel",
        Event::Text { .. } => "Text",
        Event::ScenarioText { .. } => "ScenarioText",
        Event::LineBreak => "LineBreak",
        Event::PageBreak { .. } => "PageBreak",
        Event::FontSettings(_) => "FontSettings",
        Event::FontClose => "FontClose",
        Event::FontDefault(_) => "FontDefault",
        Event::FontInit => "FontInit",
        Event::MessageLayerSwitch { .. } => "MsgLayerSwitch",
        Event::MessageLayerPop => "MsgLayerPop",
        Event::Wait { reason } => match reason {
            WaitReason::Generic => "Wait(Generic)",
            WaitReason::Stop { reason } => match reason.as_deref() {
                Some(r) => return &*format!("Wait(Stop:{r})").leak(),
                None => "Wait(Stop)",
            },
            WaitReason::Timed { .. } => "Wait(Timed)",
            WaitReason::KeyWait { .. } => "Wait(KeyWait)",
            _ => "Wait",
        },
        Event::SaveGame { .. } => "Save",
        Event::LoadGame { .. } => "Load",
        Event::Exit => "Exit",
        Event::GoTitle => "GoTitle",
        Event::ShowDialog { .. } => "ShowDialog",
        Event::YesNo { .. } => "YesNo",
        Event::SceneIn => "SceneIn",
        Event::SceneOut => "SceneOut",
        _ => "...",
    }
}

trait LeakExt {
    fn leak(self) -> &'static str;
}
impl LeakExt for String {
    fn leak(self) -> &'static str {
        Box::leak(self.into_boxed_str())
    }
}
