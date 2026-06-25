//! Core runtime — wires together GL context, compositor, interpreter,
//! text rendering and input handling into a single frame-oriented API
//! that the Flutter frontend calls from its game loop.

use crate::backend::gl::platform::{self, GfxBackend};
use crate::backend::gl::{GlRenderer, GlTextureProvider, ShaderProfile};
use crate::compositor::Compositor;
use crate::compositor::renderer::TextureProvider;
use crate::ffi_callbacks::{FfiCallbacks, InputSnapshot, MagicPathTable};
use crate::text::GlyphTextRenderer;
use crate::{Project, core_debug};
use asb_interpreter::event::WaitReason;
use asb_interpreter::tags::{ExecutionContext, TagHandler, TagResult};
use asb_interpreter::{CallbackResult, Event, ExecutionResult};
use glow::HasContext;
use image::ImageEncoder;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// `syssave()`（无 file 的 `[save]`）落盘的全局域文件名。
const SAVEG_FILE: &str = "saveg.dat";
/// `syssave()` 落盘的系统域文件名。
const SYSTEM_FILE: &str = "system.dat";

struct RuntimeResetHandler;

impl TagHandler for RuntimeResetHandler {
    fn execute(
        &self,
        ctx: &mut ExecutionContext<'_>,
    ) -> asb_interpreter::Result<TagResult> {
        ctx.variables.reset();
        Ok(TagResult::Emit(Event::GoTitle))
    }
}

#[derive(Clone)]
struct ScreenshotBuffer {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    scene: crate::compositor::Scene,
}

pub struct CoreRuntime {
    gl: Rc<glow::Context>,
    gl_ctx: Box<dyn platform::GLPlatformContext>,
    fbo: glow::Framebuffer,
    fbo_tex: glow::Texture,

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
    /// system.ini 的 SAVEPATH 原值（可能含反斜杠/CSIDL），由 load_project 捕获。
    project_savepath: Option<String>,
    /// 规范化后的存档逻辑相对前缀（如 `save`/`savedata`），种入 `s.savepath`。
    savepath: String,
    /// `[takess]` 缓存的游戏画面。`[savess]` 后续从这里缩放/编码，不能重新截保存 UI。
    save_screenshot: Option<ScreenshotBuffer>,
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

        let compositor = Compositor::new_with_stage_size(stage_width, stage_height);
        let interpreter =
            asb_interpreter::Interpreter::new(asb_interpreter::InterpreterConfig::default());

        let input = Arc::new(Mutex::new(InputSnapshot::default()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let video_finished = Arc::new(AtomicBool::new(false));
        let magic_paths: Arc<MagicPathTable> = Arc::new(Mutex::new(HashMap::new()));

        Ok(Self {
            gl,
            gl_ctx: gl_ctx,
            fbo,
            fbo_tex,
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
            project_savepath: None,
            savepath: "save".to_string(),
            save_screenshot: None,
        })
    }

    /// 重新创建 FBO 并更新渲染器的 viewport/projection。
    /// 当舞台尺寸改变时调用（例如加载不同分辨率的项目）。
    fn resize_stage(&mut self, new_width: u32, new_height: u32) -> Result<(), String> {
        // 删除旧的 FBO 和纹理
        unsafe {
            self.gl.delete_framebuffer(self.fbo);
            self.gl.delete_texture(self.fbo_tex);
        }

        // 创建新的 FBO
        let (new_fbo, new_fbo_tex) = unsafe {
            platform::create_fbo_target(&self.gl, new_width as i32, new_height as i32)
                .map_err(|e| format!("重新创建 FBO 失败: {e}"))?
        };

        self.fbo = new_fbo;
        self.fbo_tex = new_fbo_tex;
        self.stage_w = new_width;
        self.stage_h = new_height;

        // 更新渲染器的 viewport 和 projection
        self.renderer.set_viewport_size(new_width, new_height);
        self.renderer.set_stage_size(new_width, new_height);

        Ok(())
    }

    /// Load a project from an in-memory system.ini string.
    pub fn load_project(&mut self, ini_content: &str, platform: &str) -> Result<(), String> {
        let project =
            Project::open_from_data("", ini_content, platform).map_err(|e| e.to_string())?;

        let new_width = project.config().stage_width;
        let new_height = project.config().stage_height;

        // 如果分辨率改变，重新创建 FBO 和更新渲染器
        if new_width != self.stage_w || new_height != self.stage_h {
            self.resize_stage(new_width, new_height)?;
        }

        self.project_savepath = project.config().savepath.clone();
        self.save_screenshot = None;
        self.interpreter = project.create_interpreter();
        self.interpreter
            .register_tag("reset", RuntimeResetHandler);

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

        // Seed `s.savepath` —— 真实 Artemis 由引擎按 system.ini 的 SAVEPATH 种入此系统
        // 变量；脚本到处用 `e:var("s.savepath").."/"..file` 拼存档/缩略图路径，且 boot
        // 期间（boot.lua）就会读取它来检测既有存档，故必须在 start_boot 之前种好。
        //
        // 我们把它当作沙箱内的逻辑相对子目录前缀：所有存档路径形如
        // `<savepath>/save0001.dat`，由宿主统一解析到 appSupport 基准下（方案 A1 +
        // save-files-in-app-sandbox）。原始 SAVEPATH 可能含反斜杠/CSIDL（如
        // hamidashi 的 `まどそふと\ハミダシクリエイティブ`），这里规范化为正斜杠
        // 相对路径，缺省退回 `save`。
        let savepath = sanitize_savepath(self.project_savepath.as_deref());
        self.interpreter.set_variable(
            "s.savepath",
            asb_interpreter::Value::String(savepath.clone()),
        );
        self.savepath = savepath;

        // 读回上次 syssave() 落下的全局/系统域（saveg.dat / system.dat），使
        // boot.lua 期间 system_dataloading() 能拿到既有的 sys/gscr/conf。
        // 必须在 start_boot 之前，且在 s.savepath 种好之后（save_path_for 依赖它）。
        self.sysload();

        // Boot
        project
            .start_boot(&mut self.interpreter)
            .map_err(|e| e.to_string())?;

        Ok(())
    }

    pub fn stage_width(&self) -> u32 {
        self.stage_w
    }

    pub fn stage_height(&self) -> u32 {
        self.stage_h
    }

    /// 返回一帧像素数据的字节数（width * height * 4）。
    pub fn pixel_buffer_size(&self) -> usize {
        (self.stage_w * self.stage_h * 4) as usize
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
        // 抢占当前线程的 GL 上下文前，先保存宿主（Flutter）的上下文；
        // 渲染完后必须 restore，否则宿主后续的 GL 调用全打到我们的离屏 FBO，
        // 宿主窗口就黑了。
        let saved_ctx = self.gl_ctx.bind_save();

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
            if let Some(reason @ WaitReason::Stop { .. }) = self.wait_reason.clone() {
                self.drain_queued_tags_while_stopped(reason);
            } else {
                self.wait_reason = None;
            }
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
                Event::VideoFinishHandler {
                    file,
                    label,
                    call,
                    handler,
                } => {
                    crate::core_info!(
                        "[runtime] VideoFinishHandler: file={:?}, label={:?}, call={}, handler={:?}",
                        file,
                        label,
                        call,
                        handler
                    );
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

            // 存档 / 读档 / 文件删除——通过宿主回调真正落盘（方案 B + A1）
            match event {
                Event::SaveGame { file } => {
                    crate::core_info!("[runtime] Event::SaveGame file={:?}", file);
                    if file.is_empty() {
                        // 不带 file 的 [save] 即 syssave()：持久化全局/系统域到
                        // saveg.dat / system.dat（fileio.lua eqtag{"save"}）。
                        if let Err(e) = self.syssave() {
                            crate::core_error!("[runtime] syssave 失败: {}", e);
                        }
                    } else if let Err(e) = self.handle_save_game(file) {
                        crate::core_error!("[runtime] 保存存档失败 {}: {}", file, e);
                    }
                }
                Event::LoadGame { file, .. } => {
                    crate::core_info!("[runtime] Event::LoadGame file={:?}", file);
                    if file.is_empty() {
                        crate::core_warn!("[runtime] LoadGame 的 file 为空，跳过");
                    } else if let Err(e) = self.handle_load_game(file) {
                        crate::core_error!("[runtime] 读取存档失败 {}: {}", file, e);
                    }
                }
                Event::GoTitle => {
                    crate::core_info!("[runtime] Event::GoTitle");
                    if let Err(e) = self.handle_go_title() {
                        crate::core_error!("[runtime] 返回标题失败: {}", e);
                    }
                }
                Event::FileOperation {
                    command, target, ..
                } if command == "delete" => {
                    crate::core_info!("[runtime] Event::FileOperation delete target={:?}", target);
                    if let Some(t) = target {
                        match self.save_path_for(t) {
                            Ok(path) => match crate::ffi::request_delete(&path) {
                                Ok(()) => {
                                    crate::core_info!("[runtime] 已删除 {}", path);
                                }
                                Err(e) => {
                                    crate::core_warn!("[runtime] 删除文件失败 {}: {}", path, e);
                                }
                            },
                            Err(e) => {
                                crate::core_warn!("[runtime] 删除文件路径非法 {}: {}", t, e);
                            }
                        }
                    }
                }
                Event::TakeScreenshot => {
                    self.capture_save_screenshot();
                }
                Event::SaveScreenshot {
                    file,
                    width,
                    height,
                } => {
                    crate::core_info!(
                        "[runtime] Event::SaveScreenshot file={:?} width={:?} height={:?}",
                        file,
                        width,
                        height
                    );
                    if let Err(e) = self.handle_save_screenshot(file, *width, *height) {
                        crate::core_error!("[runtime] 保存缩略图失败 {}: {}", file, e);
                    }
                }
                _ => {}
            }

            self.compositor.apply_event(event);
            crate::core_debug!("[event] {}", event_name(event));
        }

        self.apply_system_audio_volume();
        if let Some(ref mut audio) = *self.compositor.audio_mut() {
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

        // 绑定 FBO，渲染到纹理而不是默认帧缓冲
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
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

        // 从 FBO 读取像素（使用 glReadPixels，对所有后端都可靠）
        let pixels = unsafe {
            platform::read_pixels(&self.gl, self.stage_w as i32, self.stage_h as i32)
        };

        // 解绑 FBO
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }

        self.input.lock().unwrap().clear_edges();

        // 渲染完毕，把 GL 上下文还给宿主。
        self.gl_ctx.restore(saved_ctx);

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

    fn apply_system_audio_volume(&self) {
        let vars = self.interpreter.variables_handle();
        let vars = vars.lock().unwrap();
        let bgm_volume = vars.get("s.bgmvol").and_then(|value| match value {
            asb_interpreter::Value::Int(v) => Some((*v as f32 / 1000.0).clamp(0.0, 1.0)),
            _ => None,
        });
        let se_volume = vars.get("s.sevol").and_then(|value| match value {
            asb_interpreter::Value::Int(v) => Some((*v as f32 / 1000.0).clamp(0.0, 1.0)),
            _ => None,
        });
        drop(vars);

        if let Some(ref mut audio) = *self.compositor.audio_mut() {
            if let Some(v) = bgm_volume {
                audio.set_bgm_volume(v);
            }
            if let Some(v) = se_volume {
                audio.set_se_volume(v);
            }
        }
    }

    pub fn is_exit_requested(&self) -> bool {
        self.exit_requested
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn drain_queued_tags_while_stopped(&mut self, stop_reason: WaitReason) {
        let mut should_resume = false;
        for _ in 0..64 {
            let drain = match self.interpreter.drain_queued_tags_only() {
                Ok(drain) => drain,
                Err(e) => {
                    crate::core_error!("解释器错误: {e:?}");
                    self.wait_reason = Some(stop_reason);
                    return;
                }
            };
            should_resume |= drain.saw_return || drain.changed_position;
            if drain.wait.is_some() {
                self.interpreter.advance_line();
                continue;
            }
            break;
        }

        if should_resume {
            self.wait_reason = None;
        } else {
            self.wait_reason = Some(stop_reason);
        }
    }

    /// 把脚本存档路径归一成宿主 saveDir 内的相对路径。
    ///
    /// 真实 Artemis 的 `[save file="save0001.dat"]` / `[load file=...]`
    /// 默认落在 `SAVEPATH` 下；脚本里 `isSaveFile()` 又会检查
    /// `s.savepath.."/"..file`。所以宿主回调必须看到同一种脚本相对路径：
    /// `savedata/save0001.dat`。若脚本已经传了 `savedata/...`，保持原样。
    fn save_path_for(&self, file: &str) -> Result<String, String> {
        qualify_save_path(file, &self.savepath)
    }

    /// 处理 [save]：触发 onSave 序列化 `sys` 等 Lua 表 → 抽干 [var] 队列 →
    /// 快照解释器状态 → JSON → 经宿主写回调落盘。
    fn handle_save_game(&mut self, file: &str) -> Result<(), String> {
        // onSave（store）把 sys/gscr/conf 经 pluto 序列化进 Artemis 变量，
        // 这些 [var] 标签排入队列后必须抽干，快照才能包含它们。
        self.interpreter
            .fire_save_handler()
            .map_err(|e| format!("onSave 处理器失败: {e:?}"))?;
        self.interpreter
            .flush_pending_tags()
            .map_err(|e| format!("抽干存档标签失败: {e:?}"))?;

        let mut data = crate::save::SaveData::from_interpreter(&self.interpreter);
        if let Some(snapshot) = &self.save_screenshot {
            data = data.with_scene(snapshot.scene.clone());
        } else {
            data = data.with_scene(self.compositor.scene_snapshot());
        }
        data = data.with_audio(self.compositor.audio_snapshot());
        let json = serde_json::to_string_pretty(&data).map_err(|e| e.to_string())?;
        let path = self.save_path_for(file)?;
        crate::ffi::request_write(&path, json.as_bytes())?;
        crate::core_info!("[runtime] 已保存存档: {}", path);

        // `store()`/`saveconv(true)` 已把最新 `sys.saveslot` 写入 `g.system`。
        // 编号存档文件本身只负责读档恢复；存档/读档列表重启后的槽位索引来自
        // saveg.dat。若这里只写 `save0001.dat`，本次会话内能读，但重启后列表为空。
        self.syssave()
            .map_err(|e| format!("同步系统存档失败: {e}"))?;
        Ok(())
    }

    /// 处理 `syssave()`——即不带 `file` 的 `[save]`（fileio.lua `eqtag{"save"}`）。
    ///
    /// 与编号存档不同，它只持久化**全局/系统**两个变量域（脚本先经
    /// `fsave_pluto` 把 sys/gscr/conf 序列化进 `g.*` 变量，再触发本保存）。落两份
    /// 固定文件：`saveg.dat`（全局域 `g.`）与 `system.dat`（系统域 `s.`），下次启动
    /// 由 [`Self::sysload`] 读回。
    fn syssave(&mut self) -> Result<(), String> {
        // fileio.lua 的 syssave() 已在排入 `[save]` 前同步执行 saveconv()，
        // 将 sys/gscr/conf 写入 g.*。这里不能再触发 onSave/flush_tag_queue：
        // 运行到 YES 对话框时，file="" 的 syssave 事件会早于 UI 关闭队列完成到达；
        // 若此处抽干全局 tag_queue，会把 dialog_return 的 return+jump 提前消费，
        // 破坏后续恢复到 popfunc02 第二个 fn.pop 的控制流。
        let store = self.interpreter.variables();
        let global: HashMap<String, asb_interpreter::Value> = store
            .iter_global()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let system: HashMap<String, asb_interpreter::Value> = store
            .iter_system()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        for (file, map) in [(SAVEG_FILE, &global), (SYSTEM_FILE, &system)] {
            let json = serde_json::to_string_pretty(map).map_err(|e| e.to_string())?;
            let path = self.save_path_for(file)?;
            crate::ffi::request_write(&path, json.as_bytes())?;
            crate::core_info!("[runtime] syssave 已保存 {} ({} 项)", path, map.len());
        }
        Ok(())
    }

    /// 启动时读回 `syssave()` 落下的全局/系统域。缺文件（首次启动）静默跳过。
    fn sysload(&mut self) {
        for (file, is_global) in [(SAVEG_FILE, true), (SYSTEM_FILE, false)] {
            let Ok(path) = self.save_path_for(file) else {
                continue;
            };
            let bytes = match crate::ffi::request_file(&path) {
                Ok(b) => b,
                Err(e) => {
                    // 首次启动尚无文件属正常；记 debug 便于排查"读路径不对"的情况。
                    crate::core_debug!("[runtime] sysload 跳过 {} ({})", path, e);
                    continue;
                }
            };
            let map: HashMap<String, asb_interpreter::Value> = match serde_json::from_slice(&bytes)
            {
                Ok(m) => m,
                Err(e) => {
                    crate::core_warn!("[runtime] sysload 解析 {} 失败: {}", path, e);
                    continue;
                }
            };
            let prefix = if is_global { "g." } else { "s." };
            let n = map.len();
            for (k, v) in map {
                self.interpreter.set_variable(&format!("{prefix}{k}"), v);
            }
            crate::core_info!("[runtime] sysload 已读回 {} ({} 项)", path, n);
        }
    }

    /// 处理 [load]：经宿主读回调读取 JSON → 恢复变量与执行位置 → 触发 onLoad
    /// 把变量反序列化回 `sys` 等 Lua 表 → 抽干队列 → 清等待状态让脚本续跑。
    fn handle_load_game(&mut self, file: &str) -> Result<(), String> {
        let path = self.save_path_for(file)?;
        let bytes = crate::ffi::request_file(&path)?;
        let data: crate::save::SaveData =
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        self.compositor.reset_for_load();
        self.hovered_layer = None;
        if let Some(scene) = data.scene.clone() {
            self.compositor.restore_scene(scene);
        }
        data.restore(&mut self.interpreter)
            .map_err(|e| format!("恢复存档状态失败: {e:?}"))?;

        // onLoad（restore）把恢复的变量经 pluto 反序列化回 sys/gscr/scr/log 等表，
        // 否则承载游戏态与存档槽位的 Lua 表仍是旧的。
        self.interpreter
            .fire_load_handler()
            .map_err(|e| format!("onLoad 处理器失败: {e:?}"))?;
        self.interpreter
            .flush_pending_tags()
            .map_err(|e| format!("抽干读档标签失败: {e:?}"))?;
        self.apply_system_audio_volume();
        if let Some(audio) = &data.audio {
            self.compositor.restore_audio(audio);
        }

        // 清除等待状态，使下一帧 run() 从恢复后的位置继续执行。
        self.wait_reason = None;
        crate::core_info!("[runtime] 已读取存档: {}", path);
        Ok(())
    }

    fn handle_go_title(&mut self) -> Result<(), String> {
        self.compositor.reset_for_load();
        self.hovered_layer = None;
        self.save_screenshot = None;
        self.timed_remaining_ms = 0;
        self.wait_reason = None;
        self.interpreter
            .start("system/first.iet", "title")
            .map_err(|e| format!("{e:?}"))?;
        Ok(())
    }

    fn capture_save_screenshot(&mut self) {
        // 确保 FBO 已绑定并渲染完成
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            self.gl.finish();
        }
        // 从 FBO 读取像素（使用 glReadPixels，对所有后端都可靠）
        let rgba = unsafe {
            platform::read_pixels(&self.gl, self.stage_w as i32, self.stage_h as i32)
        };
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        self.save_screenshot = Some(ScreenshotBuffer {
            width: self.stage_w,
            height: self.stage_h,
            rgba,
            scene: self.compositor.scene_snapshot(),
        });
        crate::core_info!(
            "[runtime] takess 已缓存游戏画面 {}x{}",
            self.stage_w,
            self.stage_h
        );
    }

    fn handle_save_screenshot(
        &mut self,
        file: &str,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Result<(), String> {
        let screenshot = self
            .save_screenshot
            .clone()
            .ok_or_else(|| "savess 前没有 takess 缓存，跳过以避免截到保存界面".to_string())?;
        let target_width = width.unwrap_or(screenshot.width).max(1);
        let target_height = height.unwrap_or(screenshot.height).max(1);
        let rgba = resize_screenshot_rgba(&screenshot, target_width, target_height)?;
        let png = encode_png_rgba(&rgba, target_width, target_height)?;
        let (resource_name, path) = self.screenshot_paths_for(file)?;

        crate::ffi::request_write(&path, &png)?;
        self.texture_provider
            .upload_rgba(&resource_name, target_width, target_height, &rgba);
        crate::core_info!(
            "[runtime] 已保存缩略图: {} (resource={}, {}x{})",
            path,
            resource_name,
            target_width,
            target_height
        );
        Ok(())
    }

    fn screenshot_paths_for(&self, file: &str) -> Result<(String, String), String> {
        let name =
            normalize_relative_path(file).ok_or_else(|| format!("非法缩略图路径: {file}"))?;
        let lower = name.to_ascii_lowercase();
        let resource_name = if lower.ends_with(".png") {
            name[..name.len() - 4].to_string()
        } else {
            name.clone()
        };
        let png_name = if lower.ends_with(".png") {
            name
        } else {
            format!("{name}.png")
        };
        Ok((
            self.save_path_for(&resource_name)?,
            self.save_path_for(&png_name)?,
        ))
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

/// 规范化 system.ini 的 SAVEPATH 为沙箱内的逻辑相对子目录前缀。
///
/// 原值可能是 Windows 风格（含反斜杠、盘符、CSIDL 特殊文件夹名），桌面/移动端都
/// 不能直接当文件系统路径用。这里：反斜杠转正斜杠、去掉首尾分隔符、剔除
/// `..`/盘符等危险段，得到一个干净的相对前缀；为空则退回 `save`。物理落盘基准由
/// 宿主（Flutter）解析到应用沙箱目录。
fn sanitize_savepath(raw: Option<&str>) -> String {
    let raw = raw.map(|s| s.trim()).unwrap_or("");
    if raw.is_empty() {
        return "save".to_string();
    }
    let normalized = raw.replace('\\', "/");
    let cleaned: Vec<&str> = normalized
        .split('/')
        .map(|seg| seg.trim())
        .filter(|seg| !seg.is_empty() && *seg != "." && *seg != ".." && !seg.ends_with(':'))
        .collect();
    if cleaned.is_empty() {
        "save".to_string()
    } else {
        cleaned.join("/")
    }
}

fn normalize_relative_path(path: &str) -> Option<String> {
    let normalized = path.trim().trim_start_matches('/').replace('\\', "/");
    let mut parts = Vec::new();
    for part in normalized.split('/') {
        let part = part.trim();
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." || part.contains(':') {
            return None;
        }
        parts.push(part);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

fn qualify_save_path(file: &str, savepath: &str) -> Result<String, String> {
    let f = normalize_relative_path(file).ok_or_else(|| format!("非法存档路径: {file}"))?;
    let prefix = savepath.trim_matches('/');
    if prefix.is_empty() || f == prefix || f.starts_with(&format!("{prefix}/")) {
        Ok(f)
    } else {
        Ok(format!("{prefix}/{f}"))
    }
}

fn resize_screenshot_rgba(
    screenshot: &ScreenshotBuffer,
    target_width: u32,
    target_height: u32,
) -> Result<Vec<u8>, String> {
    if screenshot.width == target_width && screenshot.height == target_height {
        return Ok(screenshot.rgba.clone());
    }
    let image =
        image::RgbaImage::from_raw(screenshot.width, screenshot.height, screenshot.rgba.clone())
            .ok_or_else(|| {
                format!(
                    "截图 RGBA 长度不匹配: {}x{} len={}",
                    screenshot.width,
                    screenshot.height,
                    screenshot.rgba.len()
                )
            })?;
    let resized = image::imageops::resize(
        &image,
        target_width,
        target_height,
        image::imageops::FilterType::Triangle,
    );
    Ok(resized.into_raw())
}

fn encode_png_rgba(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let expected_len = width as usize * height as usize * 4;
    if rgba.len() != expected_len {
        return Err(format!(
            "PNG RGBA 长度不匹配: {}x{} expected={} actual={}",
            width,
            height,
            expected_len,
            rgba.len()
        ));
    }
    let mut png = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut png);
    encoder
        .write_image(rgba, width, height, image::ColorType::Rgba8.into())
        .map_err(|e| e.to_string())?;
    Ok(png)
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

#[cfg(test)]
mod tests {
    use super::{
        ScreenshotBuffer, encode_png_rgba, qualify_save_path, resize_screenshot_rgba,
        sanitize_savepath,
    };
    use asb_interpreter::{CallbackResult, Event, ExecutionResult, Interpreter};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[test]
    fn save_paths_are_qualified_with_savepath_once() {
        assert_eq!(
            qualify_save_path("save0001.dat", "savedata").unwrap(),
            "savedata/save0001.dat"
        );
        assert_eq!(
            qualify_save_path("savedata/save0001.dat", "savedata").unwrap(),
            "savedata/save0001.dat"
        );
        assert_eq!(
            qualify_save_path(r"savedata\\saveg.dat", "savedata").unwrap(),
            "savedata/saveg.dat"
        );
    }

    #[test]
    fn save_paths_reject_parent_traversal() {
        assert!(qualify_save_path("../save0001.dat", "savedata").is_err());
        assert!(qualify_save_path("C:/save0001.dat", "savedata").is_err());
    }

    #[test]
    fn savepath_from_ini_is_sanitized() {
        assert_eq!(
            sanitize_savepath(Some(r"Citrus\\hokeloli")),
            "Citrus/hokeloli"
        );
        assert_eq!(sanitize_savepath(Some("../bad/save")), "bad/save");
        assert_eq!(sanitize_savepath(None), "save");
    }

    #[test]
    fn screenshot_resize_and_png_encode_work() {
        let screenshot = ScreenshotBuffer {
            width: 2,
            height: 2,
            rgba: vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
            ],
            scene: crate::compositor::Scene::new(),
        };
        let resized = resize_screenshot_rgba(&screenshot, 1, 1).unwrap();
        assert_eq!(resized.len(), 4);

        let png = encode_png_rgba(&resized, 1, 1).unwrap();
        let decoded = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (1, 1));
    }

    #[test]
    fn go_title_reset_triggers_event() {
        let mut interpreter = Interpreter::new(asb_interpreter::InterpreterConfig::default());
        interpreter.register_tag("reset", super::RuntimeResetHandler);
        interpreter
            .load_script(
                "test",
                r#"
*main
[reset]
"#,
            )
            .unwrap();
        interpreter.start("test", "main").unwrap();
        let saw_go_title = Arc::new(AtomicBool::new(false));
        let saw_go_title_c = Arc::clone(&saw_go_title);
        interpreter.set_callback(move |event| {
            if matches!(event, Event::GoTitle) {
                saw_go_title_c.store(true, Ordering::SeqCst);
            }
            CallbackResult::Continue
        });
        let result = interpreter.run().unwrap();
        assert!(matches!(result, ExecutionResult::Completed | ExecutionResult::Wait(_)));
        assert!(saw_go_title.load(Ordering::SeqCst));
    }
}

/// 事件摘要：名称 + 关键参数，供 `[event]` 调试日志使用。
///
/// 返回拥有所有权的 `String`（不再用 `Box::leak`——旧实现对每个 `Wait(Stop)`
/// 事件泄漏一段堆内存）。仅展开常用事件的关键字段，其余只给变体名。
fn event_name(e: &Event) -> String {
    use asb_interpreter::event::LayerEvent;
    match e {
        Event::Layer(layer_event) => match layer_event {
            LayerEvent::Create { id, file } => format!("LayerCreate id={id} file={file}"),
            LayerEvent::Create2 { id, file, alpha } => {
                format!("LayerCreate2 id={id} file={file} alpha={alpha:?}")
            }
            LayerEvent::Delete { id } => format!("LayerDelete id={id}"),
            LayerEvent::SetProperty {
                id,
                property,
                value,
            } => {
                format!("LayerSetProp id={id} {property}={value}")
            }
            LayerEvent::SetProperties { id, properties } => {
                format!(
                    "LayerSetProps id={id} keys={:?}",
                    properties.keys().collect::<Vec<_>>()
                )
            }
        },
        Event::LayerTween { id, param, .. } => format!("LayerTween id={id} param={param}"),
        Event::LayerTweenDelete { .. } => "LayerTweenDel".to_string(),
        Event::LayerRename { id, to } => format!("LayerRename id={id} -> {to}"),
        Event::LayerEventHandler { .. } => "LayerEvtHandler".to_string(),
        Event::UiTransition(_) => "UiTrans".to_string(),
        Event::Trans {
            trans_type,
            time,
            rule,
            ..
        } => {
            format!("Trans type={trans_type} time={time:?} rule={rule:?}")
        }
        Event::Flip => "Flip".to_string(),
        Event::BgmPlay {
            file,
            loop_play,
            gain,
            ..
        } => {
            format!("BgmPlay file={file} loop={loop_play} gain={gain:?}")
        }
        Event::BgmStop { .. } => "BgmStop".to_string(),
        Event::BgmFade { .. } => "BgmFade".to_string(),
        Event::BgmCrossFade { .. } => "BgmCrossFade".to_string(),
        Event::SePlay { id, file, .. } => format!("SePlay id={id} file={file}"),
        Event::SeStop { .. } => "SeStop".to_string(),
        Event::SeFade { .. } => "SeFade".to_string(),
        Event::VoicePlay { file, .. } => format!("VoicePlay file={file}"),
        Event::StopAllSounds { .. } => "StopAllSounds".to_string(),
        Event::SoundFinishHandler { .. } => "SoundFinishHandler".to_string(),
        Event::SoundFinishHandlerDel { .. } => "SoundFinishHandlerDel".to_string(),
        Event::VideoPlay { id, file, .. } => format!("VideoPlay id={id:?} file={file}"),
        Event::VideoFinishHandler { .. } => "VideoFinishHandler".to_string(),
        Event::VideoFinishHandlerDel => "VideoFinishHandlerDel".to_string(),
        Event::Text { content } => format!("Text {content:?}"),
        Event::ScenarioText { content, inline } => {
            format!("ScenarioText inline={inline} {content:?}")
        }
        Event::LineBreak => "LineBreak".to_string(),
        Event::PageBreak { .. } => "PageBreak".to_string(),
        Event::FontSettings(_) => "FontSettings".to_string(),
        Event::FontClose => "FontClose".to_string(),
        Event::FontDefault(_) => "FontDefault".to_string(),
        Event::FontInit => "FontInit".to_string(),
        Event::MessageLayerSwitch { .. } => "MsgLayerSwitch".to_string(),
        Event::MessageLayerPop => "MsgLayerPop".to_string(),
        Event::Wait { reason } => match reason {
            WaitReason::Generic => "Wait(Generic)".to_string(),
            WaitReason::Stop { reason } => match reason.as_deref() {
                Some(r) => format!("Wait(Stop:{r})"),
                None => "Wait(Stop)".to_string(),
            },
            WaitReason::Timed { .. } => "Wait(Timed)".to_string(),
            WaitReason::KeyWait { .. } => "Wait(KeyWait)".to_string(),
            _ => "Wait".to_string(),
        },
        Event::SaveGame { file } => format!("Save file={file:?}"),
        Event::LoadGame { file, trans_type } => {
            format!("Load file={file:?} type={trans_type:?}")
        }
        Event::FileOperation {
            command,
            src,
            dst,
            target,
        } => {
            format!("FileOp {command} src={src:?} dst={dst:?} target={target:?}")
        }
        Event::SaveScreenshot {
            file,
            width,
            height,
        } => {
            format!("SaveScreenshot file={file:?} {width:?}x{height:?}")
        }
        Event::Exit => "Exit".to_string(),
        Event::GoTitle => "GoTitle".to_string(),
        Event::ShowDialog { .. } => "ShowDialog".to_string(),
        Event::YesNo { .. } => "YesNo".to_string(),
        Event::SceneIn => "SceneIn".to_string(),
        Event::SceneOut => "SceneOut".to_string(),
        e => {
            core_debug!("[event] {:?}", e);
            "Not implemented event".to_string()
        }
    }
}
