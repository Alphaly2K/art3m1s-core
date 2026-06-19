//! winit 窗口集成示例
//!
//! 展示如何在真实窗口中渲染 Artemis 引擎的画面

#![allow(deprecated)]

use art3m1s_core::backend::gl::{GlRenderer, GlTextureProvider, ShaderProfile};
use art3m1s_core::compositor::Compositor;
use art3m1s_core::text::GlyphTextRenderer;
use art3m1s_core::Project;
use asb_interpreter::event::WaitReason;
use asb_interpreter::lua_engine::EngineCallbacks;
use asb_interpreter::{CallbackResult, Event};
use glutin::{
    config::ConfigTemplateBuilder,
    context::{ContextApi, ContextAttributesBuilder},
    display::GetGlDisplay,
    prelude::*,
    surface::{SurfaceAttributesBuilder, WindowSurface},
};
use glutin_winit::DisplayBuilder;
use raw_window_handle::HasWindowHandle;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use winit::{
    event::{ElementState, Event as WinitEvent, KeyEvent, MouseButton, WindowEvent},
    event_loop::EventLoop,
    keyboard::PhysicalKey,
    window::Window,
};

// ── 输入状态（宿主与引擎回调共享）─────────────────────────────────

#[derive(Default)]
struct InputState {
    /// 当前鼠标位置（舞台坐标）
    mouse_x: i32,
    mouse_y: i32,
    /// 本帧是否有鼠标左键单击（读取后清除）
    clicked: bool,
    /// 当前按住的键（Windows 虚拟键码）。鼠标左键以 VK=1 记入，使 isDown(1) 生效。
    keys_down: std::collections::HashSet<u32>,
    /// 本帧刚按下的键（边沿），帧末清除。
    keys_down_edge: std::collections::HashSet<u32>,
    /// 本帧刚松开的键（边沿），帧末清除。
    keys_up_edge: std::collections::HashSet<u32>,
    /// e:overrideKey{key,status} 注入的状态覆盖：status==32 强制按下、0 清除覆盖。
    /// 优先级高于真实按键状态，供脚本做 PS ○× 互换等重映射。
    key_overrides: HashMap<u32, bool>,
}

impl InputState {
    /// 帧末清除边沿状态（down/up edge 只在触发的那一帧为真）。
    fn clear_edges(&mut self) {
        self.keys_down_edge.clear();
        self.keys_up_edge.clear();
    }

    /// 综合覆盖后的「按住」判定：override 优先，否则看真实按键。
    fn key_down(&self, vk: u32) -> bool {
        match self.key_overrides.get(&vk) {
            Some(&forced) => forced,
            None => self.keys_down.contains(&vk),
        }
    }
}

// ── EngineCallbacks 实现 ─────────────────────────────────────────

/// 把 winit 的物理键映射到 Artemis 脚本使用的 Windows 虚拟键码（VK）。
///
/// 脚本里的 `e:isDown(13)` / `isDownEdge(27)` 等用的是 Windows VK 码
/// （13=Enter、27=Esc、18=Alt、112-123=F1-F12、37-40=方向键、32=Space 等，
/// 见 adv/vsync.lua、keyconfig.lua）。这里只映射脚本实际查询到的键；未覆盖的
/// 返回 None（不参与按键状态）。
fn keycode_to_vk(code: winit::keyboard::KeyCode) -> Option<u32> {
    use winit::keyboard::KeyCode as K;
    let vk = match code {
        K::Enter | K::NumpadEnter => 13,
        K::Escape => 27,
        K::Space => 32,
        K::Backspace => 8,
        K::Tab => 9,
        K::ShiftLeft | K::ShiftRight => 16,
        K::ControlLeft | K::ControlRight => 17,
        K::AltLeft | K::AltRight => 18,
        K::ArrowLeft => 37,
        K::ArrowUp => 38,
        K::ArrowRight => 39,
        K::ArrowDown => 40,
        K::F1 => 112,
        K::F2 => 113,
        K::F3 => 114,
        K::F4 => 115,
        K::F5 => 116,
        K::F6 => 117,
        K::F7 => 118,
        K::F8 => 119,
        K::F9 => 120,
        K::F10 => 121,
        K::F11 => 122,
        K::F12 => 123,
        // 数字键 0-9：VK 与 ASCII 一致（0x30-0x39）。
        K::Digit0 => 48,
        K::Digit1 => 49,
        K::Digit2 => 50,
        K::Digit3 => 51,
        K::Digit4 => 52,
        K::Digit5 => 53,
        K::Digit6 => 54,
        K::Digit7 => 55,
        K::Digit8 => 56,
        K::Digit9 => 57,
        _ => return None,
    };
    Some(vk)
}

/// 解析 PNG 的 `tEXt` 块为 `keyword → text` 映射。
///
/// PNG 结构：8 字节签名后是一串 chunk，每个 = 4 字节长度（大端）+ 4 字节类型 +
/// 数据 + 4 字节 CRC。`tEXt` 数据是 `keyword\0text`（Latin-1）。立绘把定位写在
/// `comment` 关键字里（如 `pos,101,136,120,100`）。只解析未压缩 `tEXt`，本项目用它。
fn parse_png_text_chunks(bytes: &[u8]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    const SIG: usize = 8;
    if bytes.len() < SIG || &bytes[..SIG] != b"\x89PNG\r\n\x1a\n" {
        return out;
    }
    let mut i = SIG;
    while i + 8 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]) as usize;
        let typ = &bytes[i + 4..i + 8];
        let data_start = i + 8;
        let data_end = data_start + len;
        if data_end > bytes.len() {
            break;
        }
        if typ == b"tEXt" {
            let data = &bytes[data_start..data_end];
            if let Some(nul) = data.iter().position(|&b| b == 0) {
                // tEXt 是 Latin-1；逐字节映射到 char 即正确解码。
                let keyword: String = data[..nul].iter().map(|&b| b as char).collect();
                let text: String = data[nul + 1..].iter().map(|&b| b as char).collect();
                out.insert(keyword, text);
            }
        }
        if typ == b"IEND" {
            break;
        }
        i = data_end + 4; // 下一个 chunk：跳过数据 + CRC。
    }
    out
}

/// 把一个 Artemis 资源名解析为磁盘上的真实路径（不含扩展名补全）。
///
/// `:name/rest` 中的 `name` 是 Magic Path 短名，由脚本经 `e:setMagicPath` 注册到
/// 真实路径前缀（如 `bg→image/bg`、`fa→image/fg`、`hev→image/evmask`、
/// `sysvo→sound/sysse/vo`）。短名 != 目录名，必须查表，不能假设 `image/<name>`。
/// 无 `:` 前缀的名字按项目根相对路径处理（如 `pc/ja/title/btn`）。表未命中（如
/// boot 早期 magic path 尚未注册）时回退到 `image/<rest>`。
fn resolve_resource_path(
    project_root: &std::path::Path,
    magic_paths: &Mutex<HashMap<String, String>>,
    name: &str,
) -> std::path::PathBuf {
    if let Some(rest) = name.strip_prefix(':') {
        let (ns, tail) = rest.split_once('/').unwrap_or((rest, ""));
        let table = magic_paths.lock().unwrap();
        match table.get(ns) {
            Some(prefix) => project_root.join(prefix).join(tail),
            None => project_root.join("image").join(rest),
        }
    } else {
        project_root.join(name)
    }
}

struct WinitCallbacks {
    input: Arc<Mutex<InputState>>,
    project_root: std::path::PathBuf,
    /// Magic Path 表：脚本经 `e:setMagicPath{name, path}` 注册的「短名 → 真实路径
    /// 前缀」映射（如 `bg → image/bg`、`fa → image/fg`、`sysvo → sound/sysse/vo`）。
    /// 资源名里的 `:name/rest` 引用据此解析。与纹理 provider 共享同一张表。
    magic_paths: Arc<Mutex<HashMap<String, String>>>,
}

impl EngineCallbacks for WinitCallbacks {
    fn debug(&self, _level: i32, data: &str, _raw: bool) {
        if std::env::var("ART3M1S_DEBUG").is_ok() {
            eprintln!("[debug] {}", data);
        }
    }
    fn enqueue_tag(&self, _tag: String, _params: HashMap<String, String>) {}
    fn set_event_handler(&self, _handlers: HashMap<String, String>) {}
    fn set_magic_path(&self, name: &str, path: &str) {
        self.magic_paths
            .lock()
            .unwrap()
            .insert(name.to_string(), path.to_string());
    }
    fn get_script_status(&self) -> u8 { 0 }
    fn is_key_down(&self, key_id: u32) -> bool {
        self.input.lock().unwrap().key_down(key_id)
    }
    fn is_key_down_edge(&self, key_id: u32) -> bool {
        self.input.lock().unwrap().keys_down_edge.contains(&key_id)
    }
    fn is_key_up_edge(&self, key_id: u32) -> bool {
        self.input.lock().unwrap().keys_up_edge.contains(&key_id)
    }
    /// e:overrideKey{key, status}：status==32 强制该键为按下，==0 清除覆盖。
    /// 参数经 lua_engine 映射为 (from=key, to=status)。
    fn override_key(&self, from: u32, to: u32) {
        let mut s = self.input.lock().unwrap();
        if to == 0 {
            s.key_overrides.remove(&from);
        } else {
            s.key_overrides.insert(from, true);
        }
    }
    fn is_decide(&self) -> bool {
        self.input.lock().unwrap().clicked
    }
    fn get_mouse_point(&self) -> (i32, i32) {
        let s = self.input.lock().unwrap();
        (s.mouse_x, s.mouse_y)
    }
    fn get_touch_count(&self) -> u32 { 0 }
    fn get_touch_point(&self, _index: u32) -> (i32, i32) { (0, 0) }
    fn is_file_exists(&self, path: &str) -> bool {
        // 脚本可能传 `:fa/...` 这类 magic path 引用（如 getfgfilepos 里的 isFile
        // gate、readImage 探测 `.ipt`），先经 magic-path 解析再判断。注意：必须按
        // 给定路径精确判断，**不能**自动补 `.png`——脚本靠它探测 `xxx.ipt` 等
        // 元数据文件是否存在，补 `.png` 会把不存在的 `.ipt` 误判为存在，触发脚本
        // include 一个不存在的文件而整链崩溃。
        resolve_resource_path(&self.project_root, &self.magic_paths, path).exists()
    }
    fn load_png_comments(&self, path: &str) -> Option<HashMap<String, String>> {
        // 立绘差分把定位信息存在 PNG 的 tEXt chunk 里（key=`comment`，
        // value 形如 `pos,x,y,w,h`）。脚本 getfgfilepos 据此算脸/部件相对身体的
        // 偏移。path 是 `:fa/...` magic path 引用，先解析到真实文件。
        let base = resolve_resource_path(&self.project_root, &self.magic_paths, path);
        let file = if base.exists() { base } else { base.with_extension("png") };
        let bytes = std::fs::read(&file).ok()?;
        let comments = parse_png_text_chunks(&bytes);
        if comments.is_empty() { None } else { Some(comments) }
    }
    fn file_operation(&self, _command: &str, _params: HashMap<String, String>) {}
    fn include(&self, _path: &str) {}
    fn set_flick_sensitivity(&self, _sensitivity: f64) {}
    fn get_script_block(&self) -> HashMap<String, String> { HashMap::new() }
    fn get_script_stack(&self) -> Vec<HashMap<String, String>> { vec![] }
    fn get_script_wait_reason(&self) -> u8 { 0 }
}

// ── 通用事件派发 ─────────────────────────────────────────────────
//
// 引擎对图层/输入事件的处理完全遵循 Artemis 的 lyevent / seton* 语义：命中后
// 把已注册的 handler 标签（如 calllua）以及可选的 jump/call 推回解释器的标签
// 队列，由解释器自行执行。引擎不认识任何游戏函数名（btn_clickex、
// setonpush_calllua…）或游戏状态（btn.cursor、scr.btnfunc…）——那些都活在脚本侧。

/// 把一个事件处理器的标签序列塞进解释器的标签队列。
///
/// 顺序与 lyevent spec 一致：先执行内联 `handler` 标签（携带处理器登记的全部
/// 参数），再按 `call` 走 jump 或 call 跳转到 `(file, label)`。
#[allow(clippy::too_many_arguments)]
fn enqueue_handler_tags(
    interpreter: &asb_interpreter::Interpreter,
    handler_tag: Option<&str>,
    file: Option<&str>,
    label: Option<&str>,
    call: bool,
    params: &std::collections::HashMap<String, String>,
    runtime_params: &[(&str, &str)],
) {
    let ctx = interpreter.engine_context();
    let mut queue = ctx.lock().unwrap();

    // 1) 内联 handler 标签（最常见的是 handler="calllua" function="..."）。
    if let Some(tag) = handler_tag {
        let mut p = params.clone();
        // 触发时的运行时参数（如 click=1、type=click、key=1）覆盖登记时的值。
        for (k, v) in runtime_params {
            p.insert((*k).to_string(), (*v).to_string());
        }
        queue.tag_queue.push((tag.to_string(), p));
    }

    // 2) 可选的脚本跳转：call=1 用 call 标签（压栈，handler 内 return 返回），
    //    否则用 jump。
    if file.is_some() || label.is_some() {
        let mut p = std::collections::HashMap::new();
        if let Some(f) = file {
            p.insert("file".to_string(), f.to_string());
        }
        if let Some(l) = label {
            p.insert("label".to_string(), l.to_string());
        }
        let tag = if call { "call" } else { "jump" };
        queue.tag_queue.push((tag.to_string(), p));
    }
}

/// 派发某图层注册的指定类型（click/rollover/rollout/...）事件处理器。
fn enqueue_layer_handler(
    interpreter: &asb_interpreter::Interpreter,
    compositor: &Compositor,
    layer_id: &str,
    event_type: &str,
    runtime_params: &[(&str, &str)],
) {
    let Some(layer) = compositor.scene().get(layer_id) else { return };
    let Some(h) = layer.event_handlers.get(event_type) else { return };
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

/// 派发某 (event_name, key) 注册的输入事件处理器（push 等）。
fn enqueue_input_handler(
    interpreter: &asb_interpreter::Interpreter,
    compositor: &Compositor,
    event_name: &str,
    key: &str,
    runtime_params: &[(&str, &str)],
) {
    let Some(h) = compositor.get_input_handler(event_name, key) else { return };
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 创建事件循环
    let event_loop = EventLoop::new()?;

    // 创建窗口配置
    let window_attributes = Window::default_attributes()
        .with_title("Artemis Engine")
        .with_inner_size(winit::dpi::LogicalSize::new(1280u32, 720u32))
        .with_active(true); // 请求焦点，确保能接收鼠标事件

    // 构建 GL 显示和窗口
    let template = ConfigTemplateBuilder::new();
    let display_builder = DisplayBuilder::new().with_window_attributes(Some(window_attributes));

    let (window, gl_config) = display_builder.build(&event_loop, template, |configs| {
        configs.reduce(|accum, config| {
            let transparency_check = config.supports_transparency().unwrap_or(false)
                & !accum.supports_transparency().unwrap_or(false);
            if transparency_check {
                config
            } else {
                accum
            }
        }).expect("找不到合适的 GL 配置")
    })?;

    let window = window.ok_or("窗口创建失败")?;

    // 创建 GL 上下文
    let raw_window_handle = window.window_handle()?.as_raw();
    let gl_display = gl_config.display();

    let context_attributes = ContextAttributesBuilder::new().build(Some(raw_window_handle));
    let fallback_context_attributes = ContextAttributesBuilder::new()
        .with_context_api(ContextApi::OpenGl(Some(glutin::context::Version::new(3, 3))))
        .build(Some(raw_window_handle));

    let mut not_current_gl_context = Some(unsafe {
        gl_display
            .create_context(&gl_config, &context_attributes)
            .or_else(|_| gl_display.create_context(&gl_config, &fallback_context_attributes))
            .map_err(|e| format!("创建 GL 上下文失败: {}", e))?
    });

    // macOS/Retina：窗口逻辑尺寸是 1280x720，但可绘制表面的物理像素是它乘以
    // 缩放因子（通常 2x → 2560x1440）。表面尺寸和 GL 视口都必须用物理像素。
    let scale_factor = window.scale_factor();
    let physical_size = window.inner_size();
    let fb_width = physical_size.width.max(1);
    let fb_height = physical_size.height.max(1);
    println!(
        "窗口逻辑尺寸=1280x720 缩放={:.2} 物理像素={}x{}",
        scale_factor, fb_width, fb_height
    );

    let attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(
        raw_window_handle,
        NonZeroU32::new(fb_width).unwrap(),
        NonZeroU32::new(fb_height).unwrap(),
    );

    let surface = unsafe {
        gl_config
            .display()
            .create_window_surface(&gl_config, &attrs)?
    };

    // 激活 GL 上下文
    let gl_context = not_current_gl_context
        .take()
        .unwrap()
        .make_current(&surface)?;

    // 创建 glow GL 上下文
    let gl = unsafe {
        glow::Context::from_loader_function_cstr(|s| gl_display.get_proc_address(s))
    };
    let gl = std::rc::Rc::new(gl);

    // 创建渲染器和纹理提供者
    let mut renderer = GlRenderer::new(gl.clone(), 1280, 720, ShaderProfile::GlCore330)?;
    renderer.set_viewport_size(fb_width, fb_height);

    // 项目根目录：优先读环境变量 ART3M1S_PROJECT，否则用默认位置 ~/lfpm/hamidashi。
    let project_root = std::env::var_os("ART3M1S_PROJECT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").unwrap_or_default();
            std::path::Path::new(&home).join("lfpm/hamidashi")
        });
    let project_root_clone = project_root.clone();
    // Magic Path 表，由 WinitCallbacks::set_magic_path 在脚本 boot 期间填充，纹理
    // source 闭包读取它解析 `:name/rest` 引用。两者共享同一 Arc。
    let magic_paths: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
    let magic_paths_tex = Arc::clone(&magic_paths);
    let mut texture_provider = GlTextureProvider::new(gl.clone())
        .with_source(move |name: &str| -> Option<Vec<u8>> {
            // `:name/rest` 经 Magic Path 表解析到真实路径，无前缀按根相对处理。
            let base = resolve_resource_path(&project_root_clone, &magic_paths_tex, name);
            // 资源名通常不带扩展名，先试 `.png` 再试原名。
            for path in [base.with_extension("png"), base.clone()] {
                if let Ok(bytes) = std::fs::read(&path) {
                    return Some(bytes);
                }
            }
            eprintln!("纹理缺失: {}", name);
            None
        });

    // 预登记纯色程序化纹理
    texture_provider.upload_rgba(":bg/black", 2, 2, &[0, 0, 0, 255].repeat(4));
    texture_provider.upload_rgba(":bg/white", 2, 2, &[255, 255, 255, 255].repeat(4));

    // 加载项目
    let project = Project::open(&project_root, "WINDOWS")?;
    let mut interpreter = project.create_interpreter();

    // 注册输入回调，让引擎能查询鼠标位置
    let input = Arc::new(Mutex::new(InputState::default()));
    let input_for_callbacks = Arc::clone(&input);
    interpreter.set_engine_callbacks(Box::new(WinitCallbacks { input: input_for_callbacks, project_root: project_root.clone(), magic_paths: Arc::clone(&magic_paths) }));

    // 创建合成器
    let mut compositor = Compositor::new();
    compositor.set_stage_scale(scale_factor as f32);

    // 字体渲染：加载项目中的 Source Han Sans 字体
    let font_path = project_root.join("font/sourcehansans-medium.otf");
    let mut text_renderer = GlyphTextRenderer::new();
    if let Ok(font_bytes) = std::fs::read(&font_path) {
        // 需要将字节的 ownership 转移到 renderer（'font lifetime）
        // 这里用 leak 简化——字体在整个进程生命周期内有效
        let font_bytes: &'static [u8] = Box::leak(font_bytes.into_boxed_slice());
        let _ = text_renderer.set_font(font_bytes);
    } else {
        eprintln!("字体文件未找到: {}", font_path.display());
    }
    compositor.set_text_renderer(Box::new(text_renderer));

    // 设置事件收集器
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_clone = Arc::clone(&events);
    // 视频缺省：示例项目不含 movie 文件，也没有视频子系统。脚本播完视频后会发
    // [stop exskip] 等视频结束信号才推进（见 system/script.asb 的 movie 流程）。
    // 这里把 VideoPlay 当作「瞬间播放完毕」：标记一个待结算的视频，下方等待循环
    // 在遇到紧随其后的 Stop 时据此放行，等价于视频立刻结束触发 onvideofinish。
    let video_finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let video_finished_cb = Arc::clone(&video_finished);
    interpreter.set_callback(move |e| {
        if matches!(e, Event::VideoPlay { .. }) {
            video_finished_cb.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        // 阻塞型事件必须返回 Pause，让 run() 把控制权交还给窗口循环。否则像
        // [wait]/[stop]/[dialog] 这类等待标签发出的 Wait 会被当普通事件略过，
        // 解释器不会停在等待点，脚本时序错乱（fn.pop 在空栈、sysvo 在 nil 表上
        // 被反复调用而 LuaError 刷屏），第一帧也永远画不出来。
        //
        // 注意：ScenarioText 是 [print] 发出的 `Emit`（即发即走），**不是** Wait。
        // 真正的「读者点击推进」由独立的 [l]/[p] 等待标签负责。若在这里对
        // ScenarioText 返回 Pause，step() 的 Emit+Pause 分支会原地返回 Completed
        // 且不推进 current_line，下一帧 run() 重新执行同一条 [print]，陷入死循环
        // （boot 时 font_cache 反复打印同一字形，卡在缓存阶段进不了 title）。
        let pause = matches!(
            e,
            Event::Wait { .. }
                | Event::YesNo { .. }
                | Event::ShowDialog { .. }
        );
        events_clone.lock().unwrap().push(e);
        if pause {
            CallbackResult::Pause
        } else {
            CallbackResult::Continue
        }
    });

    // 启动 boot 流程
    project.start_boot(&mut interpreter)?;

    // 等待状态：None = 正在运行，Some(reason) = 等待中
    let mut wait_reason: Option<WaitReason> = None;
    // Timed wait 的剩余毫秒
    let mut timed_remaining_ms: u64 = 0;

    // ── 自动点击：每帧扫描屏幕查找可点击区域并点击 ──────────────────
    // 设置 ART3M1S_AUTOCLICK=1 启用。使用引擎自身的 hit_test 查找
    // 注册了事件处理器的图层，不依赖任何游戏特定知识。
    let autoclick = std::env::var("ART3M1S_AUTOCLICK").is_ok();

    // 渲染循环
    let mut last_time = std::time::Instant::now();

    // 舞台尺寸（用于鼠标坐标映射）
    let stage_w = project.config().stage_width as f64;
    let stage_h = project.config().stage_height as f64;

    // 当前 hover 的按钮图层 ID（用于 over/out 事件分发）
    let mut hovered_layer: Option<String> = None;
    let mut frame_count: u64 = 0;
    let mut last_wait_cleared_frame: u64 = 0;

    event_loop.run(move |event, elwt| {
        match event {
            WinitEvent::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => {
                    elwt.exit();
                }
                WindowEvent::Resized(physical_size) => {
                    let w = physical_size.width.max(1);
                    let h = physical_size.height.max(1);
                    surface.resize(
                        &gl_context,
                        NonZeroU32::new(w).unwrap(),
                        NonZeroU32::new(h).unwrap(),
                    );
                    renderer.set_viewport_size(w, h);
                    window.request_redraw();
                }
                WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                    compositor.set_stage_scale(scale_factor as f32);
                }
                WindowEvent::CursorMoved { position, .. } => {
                    // 将物理像素坐标映射到舞台坐标
                    let win_size = window.inner_size();
                    let sx = (position.x / win_size.width as f64 * stage_w) as i32;
                    let sy = (position.y / win_size.height as f64 * stage_h) as i32;
                    let mut s = input.lock().unwrap();
                    s.mouse_x = sx;
                    s.mouse_y = sy;
                }
                WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                    let mut s = input.lock().unwrap();
                    match state {
                        // 鼠标左键以 VK=1（VK_LBUTTON）记入按键状态，使脚本里的
                        // isDown(1)/isDownEdge(1)/isUpEdge(1)（游戏退出、点击判定等）生效。
                        ElementState::Pressed => {
                            s.clicked = true;
                            if s.keys_down.insert(1) {
                                s.keys_down_edge.insert(1);
                            }
                        }
                        ElementState::Released => {
                            if s.keys_down.remove(&1) {
                                s.keys_up_edge.insert(1);
                            }
                        }
                    }
                }
                WindowEvent::KeyboardInput {
                    event: KeyEvent { physical_key, state, repeat, .. },
                    ..
                } => {
                    if let PhysicalKey::Code(code) = physical_key {
                        if let Some(vk) = keycode_to_vk(code) {
                            let mut s = input.lock().unwrap();
                            match state {
                                ElementState::Pressed => {
                                    // 系统按键重复（长按）不产生新的 down 边沿。
                                    if s.keys_down.insert(vk) && !repeat {
                                        s.keys_down_edge.insert(vk);
                                    }
                                }
                                ElementState::Released => {
                                    if s.keys_down.remove(&vk) {
                                        s.keys_up_edge.insert(vk);
                                    }
                                }
                            }
                        }
                    }
                }
                WindowEvent::RedrawRequested => {
                    let now = std::time::Instant::now();
                    let delta_ms = now.duration_since(last_time).as_millis() as u64;
                    last_time = now;

                    // 读取并清除本帧点击状态及鼠标坐标
                    let (mut clicked, mouse_x, mouse_y) = {
                        let mut s = input.lock().unwrap();
                        let v = s.clicked;
                        s.clicked = false;
                        (v, s.mouse_x as f32, s.mouse_y as f32)
                    };

                    // 自动点击：扫描屏幕查找可点击区域，用引擎自身的 hit_test。
                    if autoclick && !clicked {
                        let step = 40;
                        let mut y = step as f32;
                        'scan: while y < stage_h as f32 {
                            let mut x = step as f32;
                            while x < stage_w as f32 {
                                if compositor.hit_test(x, y, &mut texture_provider).is_some() {
                                    let mut s = input.lock().unwrap();
                                    s.mouse_x = x as i32;
                                    s.mouse_y = y as i32;
                                    clicked = true;
                                    break 'scan;
                                }
                                x += step as f32;
                            }
                            y += step as f32;
                        }
                    }

                    // 命中测试：返回最上层、可接收指针的图层 ID（透明遮罩按
                    // clickablethreshold 自动放行，见 Compositor::hit_test）。
                    // 引擎不认识任何游戏函数；它只把已注册的 handler 标签交还
                    // 解释器执行（见下方 enqueue_layer_handler / enqueue_input_handler）。
                    let new_hover_id = compositor.hit_test(mouse_x, mouse_y, &mut texture_provider);

                    // hover 状态变化：对旧图层派发 rollout、对新图层派发 rollover。
                    if new_hover_id != hovered_layer {
                        if let Some(old_id) = &hovered_layer {
                            enqueue_layer_handler(&interpreter, &compositor, old_id, "rollout", &[]);
                        }
                        if let Some(new_id) = &new_hover_id {
                            enqueue_layer_handler(&interpreter, &compositor, new_id, "rollover", &[]);
                        }
                        hovered_layer = new_hover_id.clone();
                    }

                    // 点击分发：对命中图层派发 click，再触发 push 输入事件。
                    if clicked {
                        if let Some(id) = &new_hover_id {
                            enqueue_layer_handler(
                                &interpreter,
                                &compositor,
                                id,
                                "click",
                                &[("click", "1")],
                            );
                            enqueue_input_handler(
                                &interpreter,
                                &compositor,
                                "push",
                                "1",
                                &[("key", "1"), ("type", "click")],
                            );
                        }
                    }

                    // 每帧驱动 onEnterFrame 回调（Artemis 的 vsync）。它负责清除
                    // flg.imageCacheStart 加载等待标志、键盘 edge 检测、自动模式等周期
                    // 逻辑。不驱动它，imageCacheStart 永不清除 → setonpush_calllua 在
                    // 入口直接 return → 所有按钮点击失效。回调内部经 e:tag{} 排队的标签
                    // 会和下面的 click/push 标签一起，由后续 run() 抽干。
                    if let Err(e) = interpreter.fire_enter_frame() {
                        eprintln!("onEnterFrame 错误: {:?}", e);
                    }

                    // 推进解释器
                    //
                    // 事件派发把 handler 标签塞进了解释器队列；run() 会在执行前先
                    // 抽干队列。唯一不会调用 run() 的状态是 Stop 永久等待（title
                    // 画面）——但 title 的 push handler 正是靠其中的 jump 跳出 stop。
                    // 因此若队列有待执行标签且当前停在 stop，清除等待，落入 run 分支。
                    let has_queued_tags = {
                        let ctx = interpreter.engine_context();
                        let q = ctx.lock().unwrap();
                        !q.tag_queue.is_empty()
                    };
                    let had_queued_tags = has_queued_tags;
                    if has_queued_tags {
                        wait_reason = None;
                    }

                    if wait_reason.is_none() {
                        // 没有等待状态，持续执行直到遇到 Wait 或 Completed
                        loop {
                            match interpreter.run() {
                                Ok(asb_interpreter::ExecutionResult::Wait(Event::Wait { reason })) => {
                                    match &reason {
                                        WaitReason::Timed { milliseconds } => {
                                            timed_remaining_ms = *milliseconds;
                                            wait_reason = Some(reason);
                                        }
                                        WaitReason::Stop { .. } => {
                                            // 永久停止，不推进
                                            wait_reason = Some(reason);
                                        }
                                        _ => {
                                            // Generic / Generic0 / KeyWait：等待点击
                                            wait_reason = Some(reason);
                                        }
                                    }
                                    break;
                                }
                                Ok(asb_interpreter::ExecutionResult::Wait(_)) => {
                                    // ScenarioText 等其他 Wait 事件也等点击
                                    wait_reason = Some(WaitReason::Generic);
                                    break;
                                }
                                Ok(asb_interpreter::ExecutionResult::Completed) => {
                                    break;
                                }
                                Ok(_) => {
                                    continue;
                                }
                                Err(e) => {
                                    eprintln!("解释器错误: {:?}", e);
                                    break;
                                }
                            }
                        }
                    } else {
                        // 处于等待状态
                        // 视频缺省：若刚发过 VideoPlay，则把紧随其后的 [stop exskip]
                        // 当作「视频已结束」清除一次，使 brandlogo → title 的流程不被
                        // 无视频子系统卡死。
                        let video_resume = matches!(wait_reason, Some(WaitReason::Stop { .. }))
                            && video_finished.swap(false, std::sync::atomic::Ordering::SeqCst);
                        if video_resume {
                            wait_reason = None;
                        } else {
                            let should_advance = match wait_reason.as_ref().unwrap() {
                                WaitReason::Timed { .. } => {
                                    if delta_ms >= timed_remaining_ms {
                                        timed_remaining_ms = 0;
                                        true
                                    } else {
                                        timed_remaining_ms -= delta_ms;
                                        false
                                    }
                                }
                                WaitReason::Stop { .. } => false,
                                _ => {
                                    // 排队标签已消费本次点击 → 不重复推进
                                    !had_queued_tags && (clicked || autoclick)
                                },
                            };

                            if should_advance {
                                // 诊断：记录 wait_reason 类型和清除原因
                                if std::env::var("ART3M1S_DIAG").is_ok() {
                                    let reason_type = match wait_reason.as_ref() {
                                        Some(WaitReason::Stop { .. }) => "Stop",
                                        Some(WaitReason::Timed { .. }) => "Timed",
                                        Some(WaitReason::Generic) => "Generic",
                                        Some(WaitReason::Generic0) => "Generic0",
                                        Some(WaitReason::KeyWait { .. }) => "KeyWait",
                                        _ => "Unknown",
                                    };
                                    eprintln!("[diag] frame={} wait={} cleared by click={} autoclick={}", frame_count, reason_type, clicked, autoclick);
                                }
                                wait_reason = None;
                                interpreter.advance_line();
                            }
                        }
                    }

                    // 收集并应用事件到合成器
                    let collected_events: Vec<Event> = {
                        let mut ev = events.lock().unwrap();
                        ev.drain(..).collect()
                    };
                    if std::env::var("ART3M1S_DEBUG").is_ok() && !collected_events.is_empty() {
                        for e in &collected_events {
                            eprintln!("[event] {:?}", e);
                        }
                    }
                    for event in &collected_events {
                        compositor.apply_event(event);
                    }

                    // 推进动画时钟
                    compositor.advance(delta_ms);

                    // 渲染
                    compositor.render(&mut renderer, &mut texture_provider);
                    surface.swap_buffers(&gl_context).unwrap();

                    // 帧末清除按键边沿：down/up edge 只在发生的那一帧对脚本可见，
                    // 本帧的解释器执行已读过（vsync.lua 的 isDownEdge 等）。
                    input.lock().unwrap().clear_edges();
                }
                _ => {}
            },
            WinitEvent::AboutToWait => {
                window.request_redraw();
            }
            _ => {}
        }
    })?;

    Ok(())
}
