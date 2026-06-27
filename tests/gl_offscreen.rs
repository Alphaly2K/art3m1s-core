//! GL 后端的离屏端到端测试（仅 macOS）。
//!
//! 在没有窗口的情况下创建一个 CGL 离屏 GL 上下文，用桌面 GL Core profile 跑
//! [`GlRenderer`] 把一帧 [`DrawList`] 画到 FBO，再读回像素断言颜色与位置。这验证
//! 了整条「合成器 DrawList → GPU 绘制」管线，且不依赖 ANGLE 库或真实素材。
//!
//! 着色器在 ANGLE(GLES) 与桌面 GL Core 上主体一致，因此这里用 `GlCore330` 验证
//! 的渲染逻辑同样适用于 ANGLE。

#![cfg(all(target_os = "macos", feature = "gl-backend"))]

use art3m1s_core::Project;
use art3m1s_core::backend::gl::{GlRenderer, GlTextureProvider, PlaceholderKind, ShaderProfile};
use art3m1s_core::compositor::Compositor;
use art3m1s_core::render_pipeline::RenderPipeline;
use art3m1s_core::render_pipeline::draw::Renderer;
use asb_interpreter::event::{Event, LayerEvent};
use glow::HasContext;
use std::collections::HashMap;
use std::ffi::{CString, c_void};
use std::os::raw::{c_int, c_uint};
use std::path::PathBuf;
use std::rc::Rc;

// ── 最小 CGL 离屏上下文 ─────────────────────────────────────────────

type CGLError = c_int;
type CGLPixelFormatObj = *mut c_void;
type CGLContextObj = *mut c_void;

#[link(name = "OpenGL", kind = "framework")]
unsafe extern "C" {
    fn CGLChoosePixelFormat(
        attribs: *const c_uint,
        pix: *mut CGLPixelFormatObj,
        npix: *mut c_int,
    ) -> CGLError;
    fn CGLCreateContext(
        pix: CGLPixelFormatObj,
        share: CGLContextObj,
        ctx: *mut CGLContextObj,
    ) -> CGLError;
    fn CGLSetCurrentContext(ctx: CGLContextObj) -> CGLError;
}

unsafe extern "C" {
    fn dlopen(path: *const i8, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, sym: *const i8) -> *const c_void;
}

const KCGL_PFA_ACCELERATED: c_uint = 73;
const KCGL_PFA_OPENGL_PROFILE: c_uint = 99;
const KCGL_OGL_PROFILE_3_2_CORE: c_uint = 0x3200;
const KCGL_PFA_COLOR_SIZE: c_uint = 8;

/// 创建一个 CGL 离屏上下文并设为当前，返回 glow::Context。
fn make_offscreen_context() -> Rc<glow::Context> {
    unsafe {
        let attribs: [c_uint; 6] = [
            KCGL_PFA_ACCELERATED,
            KCGL_PFA_OPENGL_PROFILE,
            KCGL_OGL_PROFILE_3_2_CORE,
            KCGL_PFA_COLOR_SIZE,
            24,
            0,
        ];
        let mut pix: CGLPixelFormatObj = std::ptr::null_mut();
        let mut npix: c_int = 0;
        assert_eq!(
            CGLChoosePixelFormat(attribs.as_ptr(), &mut pix, &mut npix),
            0,
            "CGLChoosePixelFormat 失败"
        );
        let mut ctx: CGLContextObj = std::ptr::null_mut();
        assert_eq!(
            CGLCreateContext(pix, std::ptr::null_mut(), &mut ctx),
            0,
            "CGLCreateContext 失败"
        );
        assert_eq!(CGLSetCurrentContext(ctx), 0, "CGLSetCurrentContext 失败");

        let fw = dlopen(
            c"/System/Library/Frameworks/OpenGL.framework/OpenGL".as_ptr(),
            2,
        );
        assert!(!fw.is_null(), "dlopen OpenGL.framework 失败");

        let gl = glow::Context::from_loader_function(|s| {
            let cs = CString::new(s).unwrap();
            dlsym(fw, cs.as_ptr())
        });
        Rc::new(gl)
    }
}

/// 绑定一个 RGBA8 离屏 FBO 作为渲染目标，返回 (fbo, tex)。
unsafe fn make_target(gl: &glow::Context, w: i32, h: i32) -> (glow::Framebuffer, glow::Texture) {
    unsafe {
        let tex = gl.create_texture().unwrap();
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA as i32,
            w,
            h,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );
        let fbo = gl.create_framebuffer().unwrap();
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(tex),
            0,
        );
        assert_eq!(
            gl.check_framebuffer_status(glow::FRAMEBUFFER),
            glow::FRAMEBUFFER_COMPLETE,
            "FBO 不完整"
        );
        (fbo, tex)
    }
}

/// 读回整张 RGBA 缓冲。
unsafe fn read_pixels(gl: &glow::Context, w: i32, h: i32) -> Vec<u8> {
    let mut buf = vec![0u8; (w * h * 4) as usize];
    unsafe {
        gl.read_pixels(
            0,
            0,
            w,
            h,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut buf)),
        );
    }
    buf
}

/// 取 (x, y) 处像素（注意 GL 读回的原点在左下，这里传入的 y 也按左下计）。
fn pixel_at(buf: &[u8], w: i32, x: i32, y: i32) -> [u8; 4] {
    let idx = ((y * w + x) * 4) as usize;
    [buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]]
}

fn raw(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

const W: i32 = 128;
const H: i32 = 128;

#[test]
fn renders_solid_layer_to_framebuffer() {
    let gl = make_offscreen_context();
    unsafe {
        make_target(&gl, W, H);
    }

    // 用纯红占位纹理铺满整个舞台的图层。
    let mut provider = GlTextureProvider::new(gl.clone())
        .with_placeholder(PlaceholderKind::Solid([255, 0, 0, 255]), 64);
    let mut renderer =
        GlRenderer::new(gl.clone(), W as u32, H as u32, ShaderProfile::GlCore330).unwrap();

    // 合成器：建一个铺满舞台的图层（缩放到 128x128）。
    let mut comp = Compositor::new();
    comp.apply_event(&Event::Layer(LayerEvent::Create {
        id: "1".into(),
        file: "bg".into(),
    }));
    // 占位纹理是 64x64，放大到 128x128 铺满。
    comp.apply_event(&Event::Layer(LayerEvent::SetProperties {
        id: "1".into(),
        properties: raw(&[("xscale", "200"), ("yscale", "200")]),
    }));

    let frame = RenderPipeline::new(&comp).build(&mut provider);
    assert_eq!(frame.len(), 1);
    renderer.render(&frame);
    unsafe {
        gl.finish();
    }

    let buf = unsafe { read_pixels(&gl, W, H) };
    // 中心应为红色（占位纯红）。
    let center = pixel_at(&buf, W, W / 2, H / 2);
    assert_eq!(
        center,
        [255, 0, 0, 255],
        "中心像素应为红色, 实际 {center:?}"
    );
}

#[test]
fn alpha_blends_against_cleared_background() {
    let gl = make_offscreen_context();
    unsafe {
        make_target(&gl, W, H);
    }

    // 白色占位纹理，图层 alpha=128，混合到黑色清屏背景上 → 约 50% 灰。
    let mut provider = GlTextureProvider::new(gl.clone())
        .with_placeholder(PlaceholderKind::Solid([255, 255, 255, 255]), 64);
    let mut renderer =
        GlRenderer::new(gl.clone(), W as u32, H as u32, ShaderProfile::GlCore330).unwrap();

    let mut comp = Compositor::new();
    comp.apply_event(&Event::Layer(LayerEvent::Create {
        id: "1".into(),
        file: "fg".into(),
    }));
    comp.apply_event(&Event::Layer(LayerEvent::SetProperties {
        id: "1".into(),
        properties: raw(&[("xscale", "200"), ("yscale", "200"), ("alpha", "128")]),
    }));

    let frame = RenderPipeline::new(&comp).build(&mut provider);
    renderer.render(&frame);
    unsafe {
        gl.finish();
    }

    let buf = unsafe { read_pixels(&gl, W, H) };
    let center = pixel_at(&buf, W, W / 2, H / 2);
    // 128/255 ≈ 0.5，白叠黑约为 128 左右，给宽容区间。
    assert!(
        (center[0] as i32 - 128).abs() <= 12,
        "alpha 混合后应约为中灰, 实际 {center:?}"
    );
    assert_eq!(center[0], center[1], "灰度应等量");
}

#[test]
fn negative_filter_inverts_color() {
    let gl = make_offscreen_context();
    unsafe {
        make_target(&gl, W, H);
    }

    // 纯红纹理 + negative 滤镜 → 青色 (0,255,255)。
    let mut provider = GlTextureProvider::new(gl.clone())
        .with_placeholder(PlaceholderKind::Solid([255, 0, 0, 255]), 64);
    let mut renderer =
        GlRenderer::new(gl.clone(), W as u32, H as u32, ShaderProfile::GlCore330).unwrap();

    let mut comp = Compositor::new();
    comp.apply_event(&Event::Layer(LayerEvent::Create {
        id: "1".into(),
        file: "bg".into(),
    }));
    comp.apply_event(&Event::Layer(LayerEvent::SetProperties {
        id: "1".into(),
        properties: raw(&[("xscale", "200"), ("yscale", "200"), ("negative", "1")]),
    }));

    let frame = RenderPipeline::new(&comp).build(&mut provider);
    renderer.render(&frame);
    unsafe {
        gl.finish();
    }

    let buf = unsafe { read_pixels(&gl, W, H) };
    let center = pixel_at(&buf, W, W / 2, H / 2);
    assert_eq!(center[0], 0, "红通道应被反相为 0, 实际 {center:?}");
    assert_eq!(center[1], 255, "绿通道应被反相为 255");
    assert_eq!(center[2], 255, "蓝通道应被反相为 255");
}

#[test]
fn empty_frame_clears_to_black() {
    let gl = make_offscreen_context();
    unsafe {
        make_target(&gl, W, H);
    }
    let mut renderer =
        GlRenderer::new(gl.clone(), W as u32, H as u32, ShaderProfile::GlCore330).unwrap();
    let comp = Compositor::new();
    let mut provider = GlTextureProvider::new(gl.clone());
    let frame = RenderPipeline::new(&comp).build(&mut provider);
    assert!(frame.is_empty());
    renderer.render(&frame);
    unsafe {
        gl.finish();
    }
    let buf = unsafe { read_pixels(&gl, W, H) };
    let center = pixel_at(&buf, W, W / 2, H / 2);
    assert_eq!(center, [0, 0, 0, 255], "空帧应清为黑");
}

/// 半舞台图层只覆盖一部分区域，验证世界变换/投影把图层放对了位置。
#[test]
fn layer_offset_positions_correctly() {
    let gl = make_offscreen_context();
    unsafe {
        make_target(&gl, W, H);
    }
    // 64x64 红块，放在舞台左上角 (0,0)，不缩放。
    let mut provider = GlTextureProvider::new(gl.clone())
        .with_placeholder(PlaceholderKind::Solid([255, 0, 0, 255]), 64);
    let mut renderer =
        GlRenderer::new(gl.clone(), W as u32, H as u32, ShaderProfile::GlCore330).unwrap();

    let mut comp = Compositor::new();
    comp.apply_event(&Event::Layer(LayerEvent::Create {
        id: "1".into(),
        file: "bg".into(),
    }));
    let frame = RenderPipeline::new(&comp).build(&mut provider);
    renderer.render(&frame);
    unsafe {
        gl.finish();
    }

    let buf = unsafe { read_pixels(&gl, W, H) };
    // 舞台坐标原点左上；GL 读回原点左下。图层占舞台 y∈[0,64]（顶部），
    // 在 GL 读回缓冲里对应 y∈[64,128]。取读回坐标 (32, 96) 应为红，(32, 16) 应为黑。
    let top = pixel_at(&buf, W, 32, 96);
    let bottom = pixel_at(&buf, W, 32, 16);
    assert_eq!(top, [255, 0, 0, 255], "图层覆盖区应为红, 实际 {top:?}");
    assert_eq!(bottom, [0, 0, 0, 255], "图层外应为黑, 实际 {bottom:?}");
}

// ── 真实素材：解码 + 上传 + 渲染读回 ──────────────────────────────────

/// 定位 example 项目根。测试运行目录是 crate 根。
fn sample_project_root() -> std::path::PathBuf {
    PathBuf::from("/Users/alphaly/lfpm/hamidashi")
}

/// 用真实 PNG 素材跑通整条「项目文件加载 → image 解码 → GL 上传 → 渲染 → 像素回读」
/// 路径，断言渲染结果与独立解码的源像素一致，且没有上下翻转。
#[test]
fn renders_real_png_asset_without_flip() {
    // 用一张内容上下不对称的真实素材，这样能顺带抓出纹理 V 方向翻转的 bug。
    // ev_com_00.png 是 260x140 的事件 CG 缩略图。
    let asset = "image/thumb/ev_com_00.png";
    let project = Project::open(sample_project_root(), "WINDOWS").unwrap();

    // 独立解码同一张图，作为「真值」与渲染结果比对。
    let bytes = project.read_file(asset).expect("读取素材字节");
    let truth = image::load_from_memory(&bytes).unwrap().to_rgba8();
    let (iw, ih) = truth.dimensions();
    assert_eq!((iw, ih), (260, 140), "素材尺寸应为 260x140");

    // 舞台就用素材原尺寸，图层不缩放、放在原点，做 1:1 像素比对。
    let sw = iw as i32;
    let sh = ih as i32;
    let gl = make_offscreen_context();
    unsafe {
        make_target(&gl, sw, sh);
    }

    // provider 接项目文件加载作为素材字节源；解码失败才回退占位。
    let proj_for_source = project.clone();
    let mut provider = GlTextureProvider::new(gl.clone())
        .with_source(move |name| proj_for_source.read_file(name).ok());
    let mut renderer =
        GlRenderer::new(gl.clone(), sw as u32, sh as u32, ShaderProfile::GlCore330).unwrap();

    let mut comp = Compositor::new();
    comp.apply_event(&Event::Layer(LayerEvent::Create {
        id: "1".into(),
        file: asset.into(),
    }));
    let frame = RenderPipeline::new(&comp).build(&mut provider);
    assert_eq!(frame.len(), 1, "应有一条绘制命令");
    assert_eq!(
        (frame.commands[0].size.width, frame.commands[0].size.height),
        (260, 140),
        "绘制命令应携带真实素材尺寸（解码得到，而非占位）"
    );

    renderer.render(&frame);
    unsafe {
        gl.finish();
    }
    let buf = unsafe { read_pixels(&gl, sw, sh) };

    // 在几个点上比对源像素与渲染结果。舞台坐标原点左上、GL 读回原点左下，
    // 所以源图第 sy 行对应读回缓冲第 (sh-1-sy) 行。
    let mut checked = 0;
    for (sx, sy) in [(10, 10), (130, 70), (250, 130), (60, 120), (200, 30)] {
        let src = truth.get_pixel(sx as u32, sy as u32).0;
        let got = pixel_at(&buf, sw, sx, sh - 1 - sy);
        // 允许 1:1 blit 经 LINEAR 采样的极小误差。
        for c in 0..4 {
            let diff = (src[c] as i32 - got[c] as i32).abs();
            assert!(
                diff <= 2,
                "像素({sx},{sy}) 通道{c} 源={} 渲染={} 差异过大 (src={src:?} got={got:?})",
                src[c],
                got[c]
            );
        }
        checked += 1;
    }
    assert_eq!(checked, 5);
}

/// 字节源对未知资源返回 None 时，应回退到占位纹理而非报错。
#[test]
fn missing_asset_falls_back_to_placeholder() {
    let gl = make_offscreen_context();
    unsafe {
        make_target(&gl, W, H);
    }
    let mut provider = GlTextureProvider::new(gl.clone())
        .with_source(|_name| None) // 一律「找不到」
        .with_placeholder(PlaceholderKind::Solid([0, 255, 0, 255]), 64);
    let mut renderer =
        GlRenderer::new(gl.clone(), W as u32, H as u32, ShaderProfile::GlCore330).unwrap();

    let mut comp = Compositor::new();
    comp.apply_event(&Event::Layer(LayerEvent::Create {
        id: "1".into(),
        file: "does/not/exist.png".into(),
    }));
    comp.apply_event(&Event::Layer(LayerEvent::SetProperties {
        id: "1".into(),
        properties: raw(&[("xscale", "200"), ("yscale", "200")]),
    }));
    let frame = RenderPipeline::new(&comp).build(&mut provider);
    renderer.render(&frame);
    unsafe {
        gl.finish();
    }
    let buf = unsafe { read_pixels(&gl, W, H) };
    let center = pixel_at(&buf, W, W / 2, H / 2);
    assert_eq!(
        center,
        [0, 255, 0, 255],
        "缺失资源中心应为绿色占位, 实际 {center:?}"
    );
}
