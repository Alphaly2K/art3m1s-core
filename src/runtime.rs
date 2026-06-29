//! Core runtime — wires together GL context, compositor, interpreter,
//! text rendering and input handling into a single frame-oriented API
//! that the Flutter frontend calls from its game loop.

use crate::audio::AudioBackend;
use crate::backend::gl::platform::{self, GfxBackend};
use crate::backend::gl::{GlRenderer, GlTextureProvider, ShaderProfile};
use crate::compositor::Compositor;
use crate::text::TextRenderer;
use crate::video::VideoBackend;
use asb_interpreter::Event;
use asb_interpreter::event::WaitReason;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU8};
use std::sync::{Arc, Mutex};

mod callbacks;
mod control;
mod events;
mod input;
mod magic_path;
mod media;
mod project;
mod render;
mod save_io;
mod script;
mod text;

#[derive(Default)]
struct PointerDragState {
    layer_id: Option<String>,
    start_mouse_x: f32,
    start_mouse_y: f32,
    start_left: f32,
    start_top: f32,
}

pub struct CoreRuntime {
    gl: Rc<glow::Context>,
    gl_ctx: Box<dyn platform::GLPlatformContext>,
    fbo: glow::Framebuffer,
    fbo_tex: glow::Texture,

    renderer: GlRenderer,
    texture_provider: GlTextureProvider,
    compositor: Compositor,
    text_renderer: Option<Box<dyn TextRenderer>>,
    audio: Box<dyn AudioBackend>,
    video: Box<dyn VideoBackend>,
    interpreter: asb_interpreter::Interpreter,
    input: Arc<Mutex<callbacks::InputSnapshot>>,
    events: Arc<Mutex<Vec<Event>>>,
    video_finished: Arc<AtomicBool>,
    debug_skip_active: Arc<AtomicBool>,
    script_status: Arc<AtomicU8>,
    magic_paths: Arc<magic_path::MagicPathTable>,
    layer_info: callbacks::LayerInfoTable,

    stage_w: u32,
    stage_h: u32,
    wait_reason: Option<WaitReason>,
    timed_remaining_ms: u64,
    control: control::RuntimeControlState,
    voice_serial: u64,
    hovered_layers: HashSet<String>,
    pointer_drag: PointerDragState,
    volumes: Arc<Mutex<HashMap<String, f32>>>,
    exit_requested: Arc<AtomicBool>,
    /// system.ini 的 SAVEPATH 原值（可能含反斜杠/CSIDL），由 load_project 捕获。
    project_savepath: Option<String>,
    /// 规范化后的存档逻辑相对前缀（如 `save`/`savedata`），种入 `s.savepath`。
    savepath: String,
    /// `[takess]` 缓存的游戏画面。`[savess]` 后续从这里缩放/编码，不能重新截保存 UI。
    save_screenshot: Option<save_io::ScreenshotBuffer>,
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
        let audio = Box::new(crate::audio::AudioStateBackend::new()) as Box<dyn AudioBackend>;
        let video = Box::new(crate::video::VideoStateBackend::new()) as Box<dyn VideoBackend>;
        let interpreter =
            asb_interpreter::Interpreter::new(asb_interpreter::InterpreterConfig::default());

        let input = Arc::new(Mutex::new(callbacks::InputSnapshot::default()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let video_finished = Arc::new(AtomicBool::new(false));
        let debug_skip_active = Arc::new(AtomicBool::new(false));
        let script_status = Arc::new(AtomicU8::new(0));
        let magic_paths: Arc<magic_path::MagicPathTable> = Arc::new(Mutex::new(HashMap::new()));
        let layer_info = Arc::new(Mutex::new(HashMap::new()));

        Ok(Self {
            gl,
            gl_ctx: gl_ctx,
            fbo,
            fbo_tex,
            renderer,
            texture_provider,
            compositor,
            text_renderer: None,
            audio,
            video,
            interpreter,
            input,
            events,
            video_finished,
            debug_skip_active,
            script_status,
            magic_paths: Arc::clone(&magic_paths),
            layer_info: Arc::clone(&layer_info),
            stage_w: stage_width,
            stage_h: stage_height,
            wait_reason: None,
            timed_remaining_ms: 0,
            control: control::RuntimeControlState::default(),
            voice_serial: 0,
            hovered_layers: HashSet::new(),
            pointer_drag: PointerDragState::default(),
            volumes: Arc::new(Mutex::new(HashMap::new())),
            exit_requested: Arc::new(AtomicBool::new(false)),
            project_savepath: None,
            savepath: "save".to_string(),
            save_screenshot: None,
        })
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

    /// Advance logic and render one frame. Returns the RGBA pixel buffer.
    /// The caller owns the returned `Vec<u8>`.
    pub fn advance_and_render(&mut self, delta_ms: u64) -> Vec<u8> {
        // 抢占当前线程的 GL 上下文前，先保存宿主（Flutter）的上下文；
        // 渲染完后必须 restore，否则宿主后续的 GL 调用全打到我们的离屏 FBO，
        // 宿主窗口就黑了。
        let saved_ctx = self.gl_ctx.bind_save();

        let clicked = self.process_pointer_handlers();
        self.advance_script(clicked, delta_ms);

        let collected = self.drain_events();
        self.dispatch_events(&collected);

        self.apply_system_audio_volume();
        let pending_volumes: Vec<(String, f32)> = {
            let mut pending = self.volumes.lock().unwrap();
            ["master", "bgm", "se", "voice"]
                .into_iter()
                .filter_map(|key| pending.remove(key).map(|value| (key.to_string(), value)))
                .collect()
        };
        for (kind, value) in pending_volumes {
            self.set_volume(&kind, value);
        }

        self.compositor.advance(delta_ms);
        self.advance_text(delta_ms);
        self.advance_media_and_enqueue_finish_handlers(delta_ms);

        let pixels = self.render_current_frame();
        self.clear_input_edges();

        // 渲染完毕，把 GL 上下文还给宿主。
        self.gl_ctx.restore(saved_ctx);

        pixels
    }

    pub fn is_exit_requested(&self) -> bool {
        self.exit_requested
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}
