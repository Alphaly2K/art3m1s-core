//! Cross-platform GL context creation.  Supports CGL (macOS) and
//! ANGLE via EGL.

use std::rc::Rc;

use glow::HasContext;

pub trait GLPlatformContext: Send {
    fn make_current(&self) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GfxBackend {
    /// macOS native Core OpenGL.
    Cgl,
    /// ANGLE via EGL — choose the underlying graphics API.
    Angle(AngleBackend),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AngleBackend {
    OpenGL,
    Vulkan,
    Metal,
    D3D11,
}

impl GfxBackend {
    pub fn from_int(v: i32) -> Self {
        match v {
            1 => GfxBackend::Angle(AngleBackend::OpenGL),
            2 => GfxBackend::Angle(AngleBackend::Vulkan),
            3 => GfxBackend::Angle(AngleBackend::Metal),
            4 => GfxBackend::Angle(AngleBackend::D3D11),
            _ => GfxBackend::Cgl,
        }
    }
}

pub fn create_offscreen_context(
    backend: GfxBackend,
    stage_w: u32,
    stage_h: u32,
) -> Result<(Rc<glow::Context>, Box<dyn GLPlatformContext>, GfxBackend), String> {
    match backend {
        GfxBackend::Cgl => create_cgl().map(|(g, c)| (g, c, GfxBackend::Cgl)),
        GfxBackend::Angle(sub) => match create_egl(sub, stage_w, stage_h) {
            Ok((g, c)) => Ok((g, c, GfxBackend::Angle(sub))),
            Err(e) => {
                crate::core_warn!("ANGLE failed ({e}), falling back to CGL");
                create_cgl().map(|(g, c)| (g, c, GfxBackend::Cgl))
            }
        },
    }
}

// ── CGL (macOS Core OpenGL) ────────────────────────────────────

#[cfg(target_os = "macos")]
fn create_cgl() -> Result<(Rc<glow::Context>, Box<dyn GLPlatformContext>), String> {
    mod imp {
        use super::GLPlatformContext;
        use std::ffi::{CString, c_int, c_uint, c_void};
        use std::rc::Rc;

        type CGLError = c_int;
        type CGLPixelFormatObj = *mut c_void;
        type CGLContextObj = *mut c_void;

        #[link(name = "OpenGL", kind = "framework")]
        unsafe extern "C" {
            fn CGLChoosePixelFormat(
                a: *const c_uint,
                p: *mut CGLPixelFormatObj,
                n: *mut c_int,
            ) -> CGLError;
            fn CGLCreateContext(
                p: CGLPixelFormatObj,
                s: CGLContextObj,
                c: *mut CGLContextObj,
            ) -> CGLError;
            fn CGLSetCurrentContext(c: CGLContextObj) -> CGLError;
            fn CGLReleaseContext(c: CGLContextObj) -> CGLError;
            fn CGLReleasePixelFormat(p: CGLPixelFormatObj) -> CGLError;
        }

        unsafe extern "C" {
            fn dlopen(path: *const i8, mode: c_int) -> *mut c_void;
            fn dlsym(h: *mut c_void, sym: *const i8) -> *const c_void;
        }

        pub struct Ctx {
            h: CGLContextObj,
        }
        unsafe impl Send for Ctx {}

        impl GLPlatformContext for Ctx {
            fn make_current(&self) -> bool {
                unsafe { CGLSetCurrentContext(self.h) == 0 }
            }
        }
        impl Drop for Ctx {
            fn drop(&mut self) {
                unsafe {
                    CGLReleaseContext(self.h);
                }
            }
        }

        pub fn make() -> Result<(Rc<glow::Context>, Ctx), String> {
            const A: c_uint = 73;
            const PROFILE: c_uint = 99;
            const CORE: c_uint = 0x3200;
            const COLOR: c_uint = 8;
            unsafe {
                let attrs: [c_uint; 6] = [A, PROFILE, CORE, COLOR, 24, 0];
                let mut pix = std::ptr::null_mut();
                let mut npix: c_int = 0;
                if CGLChoosePixelFormat(attrs.as_ptr(), &mut pix, &mut npix) != 0 || pix.is_null() {
                    return Err("CGLChoosePixelFormat failed".into());
                }
                let mut h = std::ptr::null_mut();
                if CGLCreateContext(pix, std::ptr::null_mut(), &mut h) != 0 || h.is_null() {
                    CGLReleasePixelFormat(pix);
                    return Err("CGLCreateContext failed".into());
                }
                CGLReleasePixelFormat(pix);
                if CGLSetCurrentContext(h) != 0 {
                    return Err("CGLSetCurrentContext failed".into());
                }
                let fw = dlopen(
                    c"/System/Library/Frameworks/OpenGL.framework/OpenGL".as_ptr(),
                    2,
                );
                if fw.is_null() {
                    return Err("dlopen OpenGL.framework failed".into());
                }
                let gl = glow::Context::from_loader_function(|s| {
                    let cs = CString::new(s).unwrap();
                    dlsym(fw, cs.as_ptr())
                });
                Ok((Rc::new(gl), Ctx { h }))
            }
        }
    }
    let (gl, ctx) = imp::make()?;
    Ok((gl, Box::new(ctx)))
}

#[cfg(not(target_os = "macos"))]
fn create_cgl() -> Result<(Rc<glow::Context>, Box<dyn GLPlatformContext>), String> {
    Err("CGL is only available on macOS".into())
}

// ── EGL / ANGLE ─────────────────────────────────────────────────

fn create_egl(
    backend: AngleBackend,
    stage_w: u32,
    stage_h: u32,
) -> Result<(Rc<glow::Context>, Box<dyn GLPlatformContext>), String> {
    mod egl {
        use super::GLPlatformContext;
        use std::ffi::{CString, c_int, c_uint, c_void};
        use std::rc::Rc;

        type EGLBoolean = c_uint;
        type EGLDisplay = *mut c_void;
        type EGLConfig = *mut c_void;
        type EGLContext = *mut c_void;
        type EGLSurface = *mut c_void;
        type EGLint = i32;

        const EGL_SUCCESS: EGLint = 0x3000;
        const EGL_NONE: EGLint = 0x3038;
        const EGL_RENDERABLE_TYPE: EGLint = 0x3040;
        const EGL_OPENGL_ES3_BIT: EGLint = 0x0040;
        const EGL_OPENGL_ES2_BIT: EGLint = 0x0004;
        const EGL_SURFACE_TYPE: EGLint = 0x3033;
        const EGL_PBUFFER_BIT: EGLint = 0x0001;
        const EGL_BLUE_SIZE: EGLint = 0x3022;
        const EGL_GREEN_SIZE: EGLint = 0x3023;
        const EGL_RED_SIZE: EGLint = 0x3024;
        const EGL_ALPHA_SIZE: EGLint = 0x3021;
        const EGL_DEPTH_SIZE: EGLint = 0x3025;
        const EGL_STENCIL_SIZE: EGLint = 0x3026;
        const EGL_WIDTH: EGLint = 0x3057;
        const EGL_HEIGHT: EGLint = 0x3056;
        const EGL_DEFAULT_DISPLAY: EGLint = 0;
        const EGL_OPENGL_ES_API: EGLint = 0x30A0;

        unsafe extern "C" {
            fn dlopen(path: *const i8, mode: c_int) -> *mut c_void;
            fn dlsym(h: *mut c_void, sym: *const i8) -> *const c_void;
        }

        macro_rules! load {
            ($lib:expr, $name:expr) => {{
                let cs = CString::new($name).unwrap();
                let ptr = dlsym($lib, cs.as_ptr());
                if ptr.is_null() {
                    return Err(format!("dlsym {} failed", $name));
                }
                std::mem::transmute::<*const c_void, _>(ptr)
            }};
        }

        pub struct EglCtx {
            _display: EGLDisplay,
            _surface: EGLSurface,
            ctx: EGLContext,
            destroy: unsafe extern "C" fn(EGLDisplay, EGLContext) -> EGLBoolean,
            destroy_surface: unsafe extern "C" fn(EGLDisplay, EGLSurface) -> EGLBoolean,
            terminate: unsafe extern "C" fn(EGLDisplay) -> EGLBoolean,
            make_current:
                unsafe extern "C" fn(EGLDisplay, EGLSurface, EGLSurface, EGLContext) -> EGLBoolean,
        }

        unsafe impl Send for EglCtx {}

        impl GLPlatformContext for EglCtx {
            fn make_current(&self) -> bool {
                unsafe {
                    (self.make_current)(self._display, self._surface, self._surface, self.ctx) != 0
                }
            }
        }

        impl Drop for EglCtx {
            fn drop(&mut self) {
                unsafe {
                    (self.destroy)(self._display, self.ctx);
                    (self.destroy_surface)(self._display, self._surface);
                    (self.terminate)(self._display);
                }
            }
        }

        // ── ANGLE platform type constants ──────────────────────
        const EGL_PLATFORM_ANGLE_ANGLE: EGLint = 0x3202;
        const EGL_PLATFORM_ANGLE_TYPE_ANGLE: EGLint = 0x3203;
        type EGLAttrib = isize;
        const EGL_PLATFORM_ANGLE_TYPE_OPENGL_ANGLE: EGLAttrib = 0x320D;
        const EGL_PLATFORM_ANGLE_TYPE_VULKAN_ANGLE: EGLAttrib = 0x3450;
        const EGL_PLATFORM_ANGLE_TYPE_METAL_ANGLE: EGLAttrib = 0x34A2;
        const EGL_PLATFORM_ANGLE_TYPE_D3D11_ANGLE: EGLAttrib = 0x3421;

        pub fn make(
            backend: super::AngleBackend,
            stage_w: u32,
            stage_h: u32,
        ) -> Result<(Rc<glow::Context>, EglCtx), String> {
            unsafe {
                // Load libEGL
                let egl_name = if cfg!(target_os = "macos") {
                    crate::ffi::angle_lib_path("libEGL.dylib")
                } else if cfg!(target_os = "linux") {
                    crate::ffi::angle_lib_path("libEGL.so")
                } else {
                    crate::ffi::angle_lib_path("libEGL.dll")
                };
                let egl_lib = dlopen(
                    CString::new(egl_name.as_str()).unwrap().as_ptr() as *const i8,
                    2,
                );
                if egl_lib.is_null() {
                    return Err("ANGLE libEGL not found — install or bundle ANGLE libraries".into());
                }

                let egl_get_display: unsafe extern "C" fn(EGLint) -> EGLDisplay =
                    load!(egl_lib, "eglGetDisplay");
                let egl_initialize: unsafe extern "C" fn(
                    EGLDisplay,
                    *mut EGLint,
                    *mut EGLint,
                ) -> EGLBoolean = load!(egl_lib, "eglInitialize");

                // Try ANGLE platform display: eglGetPlatformDisplay (EGL 1.5) first,
                // fall back to eglGetPlatformDisplayEXT (ANGLE extension alias).
                let angle_type = match backend {
                    super::AngleBackend::Vulkan => EGL_PLATFORM_ANGLE_TYPE_VULKAN_ANGLE,
                    super::AngleBackend::Metal => EGL_PLATFORM_ANGLE_TYPE_METAL_ANGLE,
                    super::AngleBackend::D3D11 => EGL_PLATFORM_ANGLE_TYPE_D3D11_ANGLE,
                    super::AngleBackend::OpenGL => EGL_PLATFORM_ANGLE_TYPE_OPENGL_ANGLE,
                };
                let attribs: [EGLAttrib; 3] = [
                    EGL_PLATFORM_ANGLE_TYPE_ANGLE as EGLAttrib,
                    angle_type,
                    EGL_NONE as EGLAttrib,
                ];

                type PfPlatformDisplay =
                    unsafe extern "C" fn(EGLint, *mut c_void, *const EGLAttrib) -> EGLDisplay;
                let display = ["eglGetPlatformDisplay", "eglGetPlatformDisplayEXT"]
                    .iter()
                    .find_map(|name| {
                        let cs = CString::new(*name).unwrap();
                        let ptr = dlsym(egl_lib, cs.as_ptr());
                        if ptr.is_null() {
                            return None;
                        }
                        let f: PfPlatformDisplay = std::mem::transmute(ptr);
                        let d = f(
                            EGL_PLATFORM_ANGLE_ANGLE,
                            EGL_DEFAULT_DISPLAY as *mut c_void,
                            attribs.as_ptr(),
                        );
                        if d.is_null() { None } else { Some(d) }
                    })
                    .unwrap_or_else(|| egl_get_display(EGL_DEFAULT_DISPLAY));
                let egl_choose_config: unsafe extern "C" fn(
                    EGLDisplay,
                    *const EGLint,
                    *mut EGLConfig,
                    EGLint,
                    *mut EGLint,
                ) -> EGLBoolean = load!(egl_lib, "eglChooseConfig");
                let egl_create_pbuffer_surface: unsafe extern "C" fn(
                    EGLDisplay,
                    EGLConfig,
                    *const EGLint,
                )
                    -> EGLSurface = load!(egl_lib, "eglCreatePbufferSurface");
                let egl_create_context: unsafe extern "C" fn(
                    EGLDisplay,
                    EGLConfig,
                    EGLContext,
                    *const EGLint,
                ) -> EGLContext = load!(egl_lib, "eglCreateContext");
                let egl_make_current: unsafe extern "C" fn(
                    EGLDisplay,
                    EGLSurface,
                    EGLSurface,
                    EGLContext,
                ) -> EGLBoolean = load!(egl_lib, "eglMakeCurrent");
                let egl_destroy_context: unsafe extern "C" fn(
                    EGLDisplay,
                    EGLContext,
                ) -> EGLBoolean = load!(egl_lib, "eglDestroyContext");
                let egl_destroy_surface: unsafe extern "C" fn(
                    EGLDisplay,
                    EGLSurface,
                ) -> EGLBoolean = load!(egl_lib, "eglDestroySurface");
                let egl_terminate: unsafe extern "C" fn(EGLDisplay) -> EGLBoolean =
                    load!(egl_lib, "eglTerminate");

                if display.is_null() {
                    return Err("eglGetDisplay failed".into());
                }
                if egl_initialize(display, std::ptr::null_mut(), std::ptr::null_mut()) == 0 {
                    return Err("eglInitialize failed".into());
                }

                let config_attrs = [
                    EGL_RENDERABLE_TYPE,
                    EGL_OPENGL_ES2_BIT,
                    EGL_SURFACE_TYPE,
                    EGL_PBUFFER_BIT,
                    EGL_RED_SIZE,
                    8,
                    EGL_GREEN_SIZE,
                    8,
                    EGL_BLUE_SIZE,
                    8,
                    EGL_ALPHA_SIZE,
                    8,
                    EGL_NONE,
                ];
                let mut config: EGLConfig = std::ptr::null_mut();
                let mut num_configs: EGLint = 0;
                if egl_choose_config(
                    display,
                    config_attrs.as_ptr(),
                    &mut config,
                    1,
                    &mut num_configs,
                ) == 0
                    || num_configs == 0
                {
                    return Err("eglChooseConfig failed".into());
                }

                let pbuffer_attrs = [
                    EGL_WIDTH,
                    stage_w as EGLint,
                    EGL_HEIGHT,
                    stage_h as EGLint,
                    EGL_NONE,
                ];
                let surface = egl_create_pbuffer_surface(display, config, pbuffer_attrs.as_ptr());
                if surface.is_null() {
                    return Err("eglCreatePbufferSurface failed".into());
                }

                // Request ES 2.0 context (ANGLE Metal works best with this)
                let ctx_attrs = [0x3098 /* EGL_CONTEXT_CLIENT_VERSION */, 2, EGL_NONE];
                let ctx =
                    egl_create_context(display, config, std::ptr::null_mut(), ctx_attrs.as_ptr());
                if ctx.is_null() {
                    return Err("eglCreateContext failed".into());
                }

                if egl_make_current(display, surface, surface, ctx) == 0 {
                    return Err("eglMakeCurrent failed".into());
                }

                // Load GLESv2 functions for glow
                let gles_name = if cfg!(target_os = "macos") {
                    crate::ffi::angle_lib_path("libGLESv2.dylib")
                } else if cfg!(target_os = "linux") {
                    crate::ffi::angle_lib_path("libGLESv2.so")
                } else {
                    crate::ffi::angle_lib_path("libGLESv2.dll")
                };
                let gles_lib = dlopen(
                    CString::new(gles_name.as_str()).unwrap().as_ptr() as *const i8,
                    2,
                );
                if gles_lib.is_null() {
                    return Err("ANGLE libGLESv2 not found".into());
                }

                let gl = glow::Context::from_loader_function(|s| {
                    let cs = CString::new(s).unwrap();
                    dlsym(gles_lib, cs.as_ptr())
                });

                Ok((
                    Rc::new(gl),
                    EglCtx {
                        _display: display,
                        _surface: surface,
                        ctx,
                        destroy: egl_destroy_context,
                        destroy_surface: egl_destroy_surface,
                        terminate: egl_terminate,
                        make_current: egl_make_current,
                    },
                ))
            }
        }
    }
    let (gl, ctx) = egl::make(backend, stage_w, stage_h)?;
    Ok((gl, Box::new(ctx)))
}

pub unsafe fn create_fbo_target(
    gl: &glow::Context,
    width: i32,
    height: i32,
) -> Result<(glow::Framebuffer, glow::Texture), String> {
    unsafe {
        let tex = gl
            .create_texture()
            .map_err(|e| format!("create_texture: {e}"))?;
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        // Use GL_RGBA for both internalformat and format — required by
        // GLES 2.0 and ANGLE Metal/GL backends (sized formats like RGBA8
        // are GLES 3.0+ only).
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA as i32,
            width,
            height,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );
        let fbo = gl
            .create_framebuffer()
            .map_err(|e| format!("create_framebuffer: {e}"))?;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(tex),
            0,
        );
        let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
        if status != glow::FRAMEBUFFER_COMPLETE {
            // Some ANGLE backends (Metal/GL) reject RGBA8 as FBO color attachment.
            // Try RGBA4 as fallback.
            gl.delete_texture(tex);
            gl.delete_framebuffer(fbo);
            let tex2 = gl
                .create_texture()
                .map_err(|e| format!("create_texture(retry): {e}"))?;
            gl.bind_texture(glow::TEXTURE_2D, Some(tex2));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA4 as i32,
                width,
                height,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            let fbo2 = gl
                .create_framebuffer()
                .map_err(|e| format!("create_framebuffer(retry): {e}"))?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo2));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(tex2),
                0,
            );
            let status2 = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            if status2 != glow::FRAMEBUFFER_COMPLETE {
                return Err(format!("FBO incomplete: {status:#x} / retry={status2:#x}"));
            }
            return Ok((fbo2, tex2));
        }
        Ok((fbo, tex))
    }
}

pub unsafe fn read_pixels(gl: &glow::Context, width: i32, height: i32) -> Vec<u8> {
    let row_bytes = (width * 4) as usize;
    let total = row_bytes * height as usize;
    let mut buf = vec![0u8; total];
    unsafe {
        gl.read_pixels(
            0,
            0,
            width,
            height,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut buf)),
        );
    }
    let mut tmp = vec![0u8; row_bytes];
    for y in 0..(height / 2) as usize {
        let top = y * row_bytes;
        let bottom = (height as usize - 1 - y) * row_bytes;
        tmp.copy_from_slice(&buf[top..top + row_bytes]);
        let (left, right) = buf.split_at_mut(bottom);
        left[top..top + row_bytes].copy_from_slice(&right[..row_bytes]);
        right[..row_bytes].copy_from_slice(&tmp);
    }
    buf
}
