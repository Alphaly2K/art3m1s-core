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
mod dialog;
mod emote;
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

#[derive(Debug, Clone)]
struct PendingDialog {
    varname: Option<String>,
    textfield: Option<String>,
    textfield_size: Option<usize>,
}

#[derive(Debug, Clone)]
struct InlineEventFrame {
    script: String,
    line: usize,
    stack: Vec<asb_interpreter::CallFrame>,
    claimed_by_jump: bool,
}

pub struct CoreRuntime {
    gl: Rc<glow::Context>,
    gl_ctx: Box<dyn platform::GLPlatformContext>,
    fbo: glow::Framebuffer,
    fbo_tex: glow::Texture,

    renderer: GlRenderer,
    texture_provider: GlTextureProvider,
    compositor: Compositor,
    /// 上一帧已经提交的逻辑场景。转场源帧需保留旧图像层，同时按当前状态
    /// 剔除刚隐藏或删除的消息文字，不能直接复用已经烘入文字的 FBO。
    last_rendered_scene: Option<crate::compositor::Scene>,
    last_rendered_clock_ms: u64,
    /// Draw list and texture generation last delivered to the host. Logic still
    /// advances every tick, but identical visual frames skip GPU work/readback.
    last_submitted_frame: Option<crate::render_pipeline::draw::DrawList>,
    last_submitted_texture_revision: u64,
    text_renderer: Option<Box<dyn TextRenderer>>,
    /// core 内部文本注入链。宿主 FFI 注入在该链之前执行。
    text_inject: crate::text::InjectionChain,
    pending_text_translations: HashMap<u64, text::PendingTextTranslation>,
    text_translation_serial: u64,
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
    emote: emote::SharedEmoteState,

    stage_w: u32,
    stage_h: u32,
    /// 上次下发的系统音量 (bgm, se)，用于跳过重复下发。
    last_system_volume: (Option<f32>, Option<f32>),
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
    /// system.ini 的 BOOT 脚本，由 load_project 捕获；gotitle 回标题时优先用它。
    boot_script: Option<String>,
    /// 规范化后的存档逻辑相对前缀（如 `save`/`savedata`），种入 `s.savepath`。
    savepath: String,
    /// `[takess]` 缓存的游戏画面。`[savess]` 后续从这里缩放/编码，不能重新截保存 UI。
    save_screenshot: Option<save_io::ScreenshotBuffer>,
    loaded_font_face: Option<String>,
    pending_dialog: Option<PendingDialog>,
    active_inline_event_frame: Option<InlineEventFrame>,
    /// 引擎侧最近一次写入 `script_status` 的值。用于区分「引擎状态迁移」与
    /// 「脚本经 e:setScriptStatus 强制改写」：原子量与该值不一致即为脚本改写。
    last_engine_status: u8,
    /// 脚本经 e:setScriptStatus 强制设了非 0 停止码（如 4「停止，不接受用户输入」）。
    /// 置位期间剧情不推进（onEnterFrame 仍每帧运行以便自我恢复），直到 setScriptStatus(0)。
    script_forced_stop: bool,
    /// 上一帧是否处于点击等待，用于检测 onClickWaitIn/Out 边沿。
    was_click_wait: bool,
    /// 本帧是否派发了剧情文本（用于已读判定：只在文本展示后的点击等待处标记已读）。
    scenario_text_shown: bool,
    /// 已读记录自上次持久化后是否有新增（syssave 时落 aread.dat）。
    read_dirty: bool,
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

        // load_project 时会带 magic-path 解析重建 provider；这里先建一个
        // 无字节源的裸 provider 占位即可，不必接 FFI 源。
        let texture_provider = GlTextureProvider::new(gl.clone());

        let compositor = Compositor::new();
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
        let emote = Arc::new(Mutex::new(emote::EmoteState::default()));

        Ok(Self {
            gl,
            gl_ctx: gl_ctx,
            fbo,
            fbo_tex,
            renderer,
            texture_provider,
            compositor,
            last_rendered_scene: None,
            last_rendered_clock_ms: 0,
            last_submitted_frame: None,
            last_submitted_texture_revision: 0,
            text_renderer: None,
            text_inject: crate::text::InjectionChain::new(),
            pending_text_translations: HashMap::new(),
            text_translation_serial: 0,
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
            emote,
            stage_w: stage_width,
            stage_h: stage_height,
            last_system_volume: (None, None),
            wait_reason: None,
            timed_remaining_ms: 0,
            control: control::RuntimeControlState::default(),
            voice_serial: 0,
            hovered_layers: HashSet::new(),
            pointer_drag: PointerDragState::default(),
            volumes: Arc::new(Mutex::new(HashMap::new())),
            exit_requested: Arc::new(AtomicBool::new(false)),
            project_savepath: None,
            boot_script: None,
            savepath: "save".to_string(),
            save_screenshot: None,
            loaded_font_face: None,
            pending_dialog: None,
            active_inline_event_frame: None,
            last_engine_status: 0,
            script_forced_stop: false,
            was_click_wait: false,
            scenario_text_shown: false,
            read_dirty: false,
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
        (self.stage_w as usize)
            .saturating_mul(self.stage_h as usize)
            .saturating_mul(4)
    }

    /// Advance logic and render one frame. Returns the RGBA pixel buffer.
    /// The caller owns the returned `Vec<u8>`.
    pub fn advance_and_render(&mut self, delta_ms: u64) -> Vec<u8> {
        let mut pixels = vec![0; self.pixel_buffer_size()];
        let written = self.advance_and_render_into(delta_ms, &mut pixels);
        pixels.truncate(written);
        pixels
    }

    /// Advance logic and render directly into a caller-owned RGBA buffer.
    /// Returns the number of bytes written, or zero when the buffer is too small.
    pub fn advance_and_render_into(&mut self, delta_ms: u64, out_pixels: &mut [u8]) -> usize {
        if out_pixels.len() < self.pixel_buffer_size() {
            return 0;
        }

        // 抢占当前线程的 GL 上下文前，先保存宿主（Flutter）的上下文；
        // 渲染完后必须 restore，否则宿主后续的 GL 调用全打到我们的离屏 FBO，
        // 宿主窗口就黑了。
        let saved_ctx = self.gl_ctx.bind_save();

        // isPush 的按键重复语义依赖每键按下时间戳，逐帧维护。
        self.input
            .lock()
            .unwrap()
            .note_frame_for_push(std::time::Instant::now());
        // getScriptStatus 的引擎状态自动迁移 + setScriptStatus(0) 的唤醒语义。
        self.sync_script_status();
        let clicked = self.process_pointer_handlers();
        self.advance_script(clicked, delta_ms);

        let collected = self.drain_events();
        self.dispatch_events(&collected);
        // 已读跟踪 + 未读停跳：在文本展示后的点击等待处标记已读，
        // 已读跳过遇未读剧情时停止跳过（[alreadyread]/[skip unread=] 语义）。
        self.track_read_and_stop_skip_on_unread();
        // 点击等待进入/退出边沿：触发 e:setEventHandler{onClickWaitIn/Out}。
        self.sync_click_wait_handlers();
        self.sync_emote_scene();

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
        // get_layer_info 必须反映本帧缓动后的实际位置，而不是缓动开始前的
        // 静态 LayerProps。下一帧输入回调执行 Lua 前会读取这份快照。
        self.sync_layer_info_all();
        self.dispatch_tween_handlers();
        self.emote.lock().unwrap().advance(delta_ms);
        self.advance_text(delta_ms);
        self.apply_ready_text_translations();
        self.advance_media_and_enqueue_finish_handlers(delta_ms);

        let written = self.render_current_frame_into(out_pixels);
        self.clear_input_edges();

        // 渲染完毕，把 GL 上下文还给宿主。
        self.gl_ctx.restore(saved_ctx);

        written
    }

    pub fn is_exit_requested(&self) -> bool {
        self.exit_requested
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// 每帧同步脚本引擎执行状态（getScriptStatus 语义，见
    /// docs/lua/engine/getScriptStatus.txt）到 `script_status` 原子量，
    /// 并落实 e:setScriptStatus 的两类强制改写：
    /// - 设 0（运行中）：从等待/停止状态唤醒（setScriptStatus.txt 提到的
    ///   「相对安全用法」——从停止切换到执行）；
    /// - 设非 0：尊重脚本值，直到引擎自身状态迁移产生新状态。
    fn sync_script_status(&mut self) {
        use std::sync::atomic::Ordering;

        let current = self.script_status.load(Ordering::SeqCst);
        if current != self.last_engine_status {
            // 原子量与引擎上次写入不一致 ⇒ 脚本经 e:setScriptStatus 改写过。
            if current == 0 {
                // 设 0 唤醒：清除当前等待并越过触发等待的指令，解除强制停止。
                if self.wait_reason.is_some() {
                    self.advance_wait_line();
                }
                self.script_forced_stop = false;
                self.last_engine_status = 0;
            } else {
                // 非零强制值：脚本强制停止执行（如 setScriptStatus(4)「停止，不接受
                // 用户输入」）。置位后剧情暂停，直到脚本 setScriptStatus(0) 自我恢复。
                self.script_forced_stop = true;
                self.last_engine_status = current;
                return;
            }
        }

        // 强制停止期间不做引擎态自动迁移：保持脚本设定的停止码，直到 setScriptStatus(0)。
        if self.script_forced_stop {
            return;
        }

        let computed = engine_status_for(
            self.wait_reason.as_ref(),
            self.pending_dialog.is_some(),
            self.debug_skip_active.load(Ordering::SeqCst),
            self.is_exit_requested(),
        );
        if computed != self.last_engine_status {
            self.script_status.store(computed, Ordering::SeqCst);
            self.last_engine_status = computed;
        }
    }
}

/// 把引擎运行状态映射为脚本可见的执行状态码（getScriptStatus.txt）：
/// 0 执行中 / 1 等待点击 / 2 过渡中 / 3 停止（计时器或输入恢复）/
/// 4 停止（仅计时器）/ 7 全屏视频播放中 / 9 对话框显示中 / 14 引擎退出。
fn engine_status_for(
    wait_reason: Option<&WaitReason>,
    dialog_open: bool,
    debug_skip: bool,
    exit_requested: bool,
) -> u8 {
    if exit_requested {
        return 14;
    }
    if debug_skip {
        // debugSkip 快进：既有约定为 4（不接受用户输入的停止）。
        return 4;
    }
    if dialog_open {
        return 9;
    }
    match wait_reason {
        None => 0,
        Some(WaitReason::Stop { reason: Some(r) }) if r == "video" => 7,
        Some(WaitReason::Stop { reason: Some(r) }) if r == "trans" || r.starts_with("tween:") => 2,
        Some(WaitReason::Stop { .. }) => 3,
        // 等待点击（@ / 文本推进 / wait input=1）。
        Some(WaitReason::Generic) | Some(WaitReason::Generic0) => 1,
        Some(WaitReason::Timed { input: 1, .. }) => 1,
        // 纯计时等待：仅计时器恢复。
        Some(WaitReason::Timed { .. }) => 4,
        // SE / 视频层 / 文本缓动等事件等待：停止、由事件恢复。
        Some(_) => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::engine_status_for;
    use asb_interpreter::event::WaitReason;

    #[cfg(all(target_os = "macos", feature = "gl-backend"))]
    use super::CoreRuntime;
    #[cfg(all(target_os = "macos", feature = "gl-backend"))]
    use crate::backend::gl::platform::GfxBackend;

    #[test]
    fn engine_status_maps_wait_states_to_script_status_codes() {
        // 0 执行中。
        assert_eq!(engine_status_for(None, false, false, false), 0);
        // 1 等待点击（@ 与 wait input=1）。
        assert_eq!(
            engine_status_for(Some(&WaitReason::Generic), false, false, false),
            1
        );
        assert_eq!(
            engine_status_for(
                Some(&WaitReason::Timed {
                    milliseconds: 100,
                    input: 1
                }),
                false,
                false,
                false
            ),
            1
        );
        // 4 纯计时等待。
        assert_eq!(
            engine_status_for(
                Some(&WaitReason::Timed {
                    milliseconds: 100,
                    input: 0
                }),
                false,
                false,
                false
            ),
            4
        );
        // 2 过渡中 / 7 全屏视频 / 3 一般停止。
        assert_eq!(
            engine_status_for(
                Some(&WaitReason::Stop {
                    reason: Some("trans".into())
                }),
                false,
                false,
                false
            ),
            2
        );
        assert_eq!(
            engine_status_for(
                Some(&WaitReason::Stop {
                    reason: Some("video".into())
                }),
                false,
                false,
                false
            ),
            7
        );
        assert_eq!(
            engine_status_for(
                Some(&WaitReason::Stop { reason: None }),
                false,
                false,
                false
            ),
            3
        );
    }

    #[test]
    fn dialog_debug_skip_and_exit_take_precedence() {
        // 9 对话框优先于等待状态。
        assert_eq!(
            engine_status_for(Some(&WaitReason::Generic), true, false, false),
            9
        );
        // 4 debugSkip 快进。
        assert_eq!(
            engine_status_for(Some(&WaitReason::Generic), false, true, false),
            4
        );
        // 14 引擎退出最高优先。
        assert_eq!(engine_status_for(None, true, true, true), 14);
    }

    #[cfg(all(target_os = "macos", feature = "gl-backend"))]
    #[test]
    fn static_runtime_emits_first_frame_then_skips_identical_frame() {
        let mut runtime = CoreRuntime::create(8, 8, GfxBackend::Cgl).unwrap();
        let mut pixels = vec![0; runtime.pixel_buffer_size()];

        assert_eq!(
            runtime.advance_and_render_into(16, &mut pixels),
            pixels.len()
        );
        assert_eq!(runtime.advance_and_render_into(17, &mut pixels), 0);
    }
}
