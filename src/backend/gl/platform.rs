//! Cross-platform GL context creation.  Supports CGL (macOS) and
//! ANGLE via EGL.

use std::rc::Rc;

use glow::HasContext;

pub trait GLPlatformContext: Send {
    fn make_current(&self) -> bool;

    /// 保存当前线程的 GL 上下文并把我们的上下文设为 current，返回保存的句柄。
    /// 调用方稍后必须把句柄传给 [`GLPlatformContext::restore`] 以恢复原上下文。
    fn bind_save(&self) -> SavedGlContext;

    /// 恢复之前由 [`GLPlatformContext::bind_save`] 保存的 GL 上下文。
    fn restore(&self, saved: SavedGlContext);

    /// Binds a host-owned platform surface as the zero-copy presentation target.
    /// Kind 1 is an Android `ANativeWindow`; kind 2 is an Apple `IOSurface`;
    /// kind 3 is a Metal `MTLTexture` imported through an EGL image.
    fn set_external_surface(
        &self,
        _kind: i32,
        _handle: *mut std::ffi::c_void,
        _width: i32,
        _height: i32,
    ) -> Result<(), String> {
        Err("external surfaces are unsupported by this GL backend".into())
    }

    fn clear_external_surface(&self) {}

    /// Makes the configured host surface current for a renderer-owned draw pass.
    fn bind_external_surface(&self) -> Result<(), String> {
        Err("external surfaces are unsupported by this GL backend".into())
    }

    /// Presents the current host surface and restores the internal offscreen surface.
    fn present_external_surface(&self) -> Result<(), String> {
        Err("external surfaces are unsupported by this GL backend".into())
    }

    /// Restores the internal offscreen surface after an aborted presentation pass.
    fn restore_internal_surface(&self) -> Result<(), String> {
        self.make_current()
            .then_some(())
            .ok_or_else(|| "failed to restore internal GL surface".into())
    }
}

/// 之前线程当前的 GL 上下文快照（不透明句柄）。
///
/// 用于 [`GLPlatformContext::bind_save`] / [`GLPlatformContext::restore`] 之间保存
/// 宿主（如 Flutter）的 EGL/CGL 上下文，避免我们的离屏上下文长期抢占线程的
/// current context 导致宿主渲染全黑。
pub struct SavedGlContext {
    /// EGL: (display, draw_surface, read_surface, context)。CGL: unused。
    pub(crate) display: *mut std::ffi::c_void,
    pub(crate) draw: *mut std::ffi::c_void,
    pub(crate) read: *mut std::ffi::c_void,
    pub(crate) context: *mut std::ffi::c_void,
}

impl SavedGlContext {
    /// 空快照（没有上下文需要恢复）。
    pub const NONE: Self = Self {
        display: std::ptr::null_mut(),
        draw: std::ptr::null_mut(),
        read: std::ptr::null_mut(),
        context: std::ptr::null_mut(),
    };

    pub fn is_none(&self) -> bool {
        self.context.is_null()
    }
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
        use std::ffi::{CString, c_char, c_int, c_uint, c_void};
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
            fn CGLGetCurrentContext() -> CGLContextObj;
            fn CGLReleaseContext(c: CGLContextObj) -> CGLError;
            fn CGLReleasePixelFormat(p: CGLPixelFormatObj) -> CGLError;
        }

        unsafe extern "C" {
            fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
            fn dlsym(h: *mut c_void, sym: *const c_char) -> *const c_void;
        }

        pub struct Ctx {
            h: CGLContextObj,
        }
        unsafe impl Send for Ctx {}

        impl GLPlatformContext for Ctx {
            fn make_current(&self) -> bool {
                unsafe { CGLSetCurrentContext(self.h) == 0 }
            }
            fn bind_save(&self) -> super::SavedGlContext {
                let prev = unsafe { CGLGetCurrentContext() };
                let _ = self.make_current();
                super::SavedGlContext {
                    display: std::ptr::null_mut(),
                    draw: std::ptr::null_mut(),
                    read: std::ptr::null_mut(),
                    context: prev,
                }
            }
            fn restore(&self, saved: super::SavedGlContext) {
                // CGL 用 null 表示"解除当前上下文"，非 null 则恢复。
                unsafe { CGLSetCurrentContext(saved.context) };
            }
        }
        impl Drop for Ctx {
            fn drop(&mut self) {
                unsafe {
                    if CGLGetCurrentContext() == self.h {
                        let _ = CGLSetCurrentContext(std::ptr::null_mut());
                    }
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
        use libloading::Library;
        use std::ffi::{CString, c_char, c_uint, c_void};
        use std::rc::Rc;
        use std::sync::Mutex;

        type EGLBoolean = c_uint;
        type EGLDisplay = *mut c_void;
        type EGLConfig = *mut c_void;
        type EGLContext = *mut c_void;
        type EGLSurface = *mut c_void;
        type EGLint = i32;

        const EGL_NONE: EGLint = 0x3038;
        const EGL_RENDERABLE_TYPE: EGLint = 0x3040;
        const EGL_OPENGL_ES2_BIT: EGLint = 0x0004;
        const EGL_OPENGL_ES3_BIT: EGLint = 0x0040;
        const EGL_SURFACE_TYPE: EGLint = 0x3033;
        const EGL_PBUFFER_BIT: EGLint = 0x0001;
        const EGL_WINDOW_BIT: EGLint = 0x0004;
        const EGL_BLUE_SIZE: EGLint = 0x3022;
        const EGL_GREEN_SIZE: EGLint = 0x3023;
        const EGL_RED_SIZE: EGLint = 0x3024;
        const EGL_ALPHA_SIZE: EGLint = 0x3021;
        const EGL_WIDTH: EGLint = 0x3057;
        const EGL_HEIGHT: EGLint = 0x3056;
        const EGL_DEFAULT_DISPLAY: EGLint = 0;
        const EGL_OPENGL_ES_API: EGLint = 0x30A0;
        const EGL_TEXTURE_FORMAT: EGLint = 0x3080;
        const EGL_TEXTURE_TARGET: EGLint = 0x3081;
        const EGL_TEXTURE_RGBA: EGLint = 0x305E;
        const EGL_IOSURFACE_ANGLE: EGLint = 0x3454;
        const EGL_IOSURFACE_PLANE_ANGLE: EGLint = 0x345A;
        const EGL_TEXTURE_RECTANGLE_ANGLE: EGLint = 0x345B;
        const EGL_TEXTURE_TYPE_ANGLE: EGLint = 0x345C;
        const EGL_TEXTURE_INTERNAL_FORMAT_ANGLE: EGLint = 0x345D;
        const EGL_MTL_TEXTURE_MGL: EGLint = 0x3456;
        const GL_BGRA_EXT: EGLint = 0x80E1;
        const GL_UNSIGNED_BYTE: EGLint = 0x1401;
        const GL_TEXTURE_2D: u32 = 0x0DE1;
        const GL_FRAMEBUFFER: u32 = 0x8D40;
        const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
        const GL_FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
        const GL_NO_ERROR: u32 = 0;

        enum ExternalSurface {
            Surface {
                surface: EGLSurface,
                swap_on_present: bool,
            },
            Texture {
                image: *mut c_void,
                texture: u32,
                framebuffer: u32,
            },
        }

        macro_rules! load {
            ($lib:expr, $name:expr) => {{
                *$lib
                    .get(concat!($name, "\0").as_bytes())
                    .map_err(|error| format!("load {} failed: {error}", $name))?
            }};
        }

        unsafe fn open_library(path: &str, global: bool) -> Result<Library, String> {
            #[cfg(unix)]
            if global {
                let library = unsafe {
                    libloading::os::unix::Library::open(
                        Some(path),
                        libloading::os::unix::RTLD_NOW | libloading::os::unix::RTLD_GLOBAL,
                    )
                }
                .map_err(|error| format!("load {path} failed: {error}"))?;
                return Ok(library.into());
            }
            let _ = global;
            unsafe { Library::new(path) }.map_err(|error| format!("load {path} failed: {error}"))
        }

        unsafe fn open_first(candidates: &[String], global: bool) -> Result<Library, String> {
            let mut errors = Vec::new();
            for path in candidates {
                match unsafe { open_library(path, global) } {
                    Ok(library) => return Ok(library),
                    Err(error) => errors.push(error),
                }
            }
            Err(errors.join("; "))
        }

        unsafe fn optional_symbol<T: Copy>(library: &Library, name: &str) -> Option<T> {
            let name = CString::new(name).ok()?;
            unsafe { library.get::<T>(name.as_bytes_with_nul()).ok() }.map(|symbol| *symbol)
        }

        unsafe fn symbol_address(library: &Library, name: &CString) -> *const c_void {
            unsafe {
                library
                    .get::<unsafe extern "C" fn()>(name.as_bytes_with_nul())
                    .map(|symbol| *symbol as *const () as *const c_void)
                    .unwrap_or(std::ptr::null())
            }
        }

        pub struct EglCtx {
            display: EGLDisplay,
            pbuffer: EGLSurface,
            config: EGLConfig,
            ctx: EGLContext,
            destroy: unsafe extern "C" fn(EGLDisplay, EGLContext) -> EGLBoolean,
            destroy_surface: unsafe extern "C" fn(EGLDisplay, EGLSurface) -> EGLBoolean,
            terminate: unsafe extern "C" fn(EGLDisplay) -> EGLBoolean,
            make_current:
                unsafe extern "C" fn(EGLDisplay, EGLSurface, EGLSurface, EGLContext) -> EGLBoolean,
            // 用于 bind_save/restore：查询/恢复当前线程的 EGL 上下文。
            get_current_display: unsafe extern "C" fn() -> EGLDisplay,
            get_current_surface: unsafe extern "C" fn(EGLint) -> EGLSurface,
            get_current_context: unsafe extern "C" fn() -> EGLContext,
            create_window_surface: unsafe extern "C" fn(
                EGLDisplay,
                EGLConfig,
                *mut c_void,
                *const EGLint,
            ) -> EGLSurface,
            create_pbuffer_from_client_buffer: unsafe extern "C" fn(
                EGLDisplay,
                EGLint,
                *mut c_void,
                EGLConfig,
                *const EGLint,
            ) -> EGLSurface,
            create_image: Option<
                unsafe extern "C" fn(
                    EGLDisplay,
                    EGLContext,
                    EGLint,
                    *mut c_void,
                    *const EGLint,
                ) -> *mut c_void,
            >,
            destroy_image: Option<unsafe extern "C" fn(EGLDisplay, *mut c_void) -> EGLBoolean>,
            gl_gen_textures: unsafe extern "C" fn(i32, *mut u32),
            gl_delete_textures: unsafe extern "C" fn(i32, *const u32),
            gl_bind_texture: unsafe extern "C" fn(u32, u32),
            gl_egl_image_target_texture_2d: Option<unsafe extern "C" fn(u32, *mut c_void)>,
            gl_gen_framebuffers: unsafe extern "C" fn(i32, *mut u32),
            gl_delete_framebuffers: unsafe extern "C" fn(i32, *const u32),
            gl_bind_framebuffer: unsafe extern "C" fn(u32, u32),
            gl_framebuffer_texture_2d: unsafe extern "C" fn(u32, u32, u32, u32, i32),
            gl_check_framebuffer_status: unsafe extern "C" fn(u32) -> u32,
            gl_flush: unsafe extern "C" fn(),
            gl_get_error: unsafe extern "C" fn() -> u32,
            swap_buffers: unsafe extern "C" fn(EGLDisplay, EGLSurface) -> EGLBoolean,
            get_error: unsafe extern "C" fn() -> EGLint,
            iosurface_texture_target: EGLint,
            external_surface: Mutex<Option<ExternalSurface>>,
            _egl_library: Library,
            _gles_library: Library,
            _desktop_gl_library: Option<Library>,
        }

        unsafe impl Send for EglCtx {}

        impl EglCtx {
            unsafe fn create_metal_texture_target(
                &self,
                handle: *mut c_void,
            ) -> Result<ExternalSurface, String> {
                unsafe {
                    let create_image = self
                        .create_image
                        .ok_or("EGL image import is unavailable in this ANGLE build")?;
                    let destroy_image = self
                        .destroy_image
                        .ok_or("EGL image destruction is unavailable in this ANGLE build")?;
                    let bind_image = self
                        .gl_egl_image_target_texture_2d
                        .ok_or("GL_OES_EGL_image is unavailable in this ANGLE build")?;
                    let image = create_image(
                        self.display,
                        std::ptr::null_mut(),
                        EGL_MTL_TEXTURE_MGL,
                        handle,
                        [EGL_NONE].as_ptr(),
                    );
                    if image.is_null() {
                        return Err(format!(
                            "failed to import MTLTexture as EGLImage: EGL {:#x}",
                            (self.get_error)()
                        ));
                    }

                    while (self.gl_get_error)() != GL_NO_ERROR {}
                    let mut texture = 0;
                    (self.gl_gen_textures)(1, &mut texture);
                    (self.gl_bind_texture)(GL_TEXTURE_2D, texture);
                    bind_image(GL_TEXTURE_2D, image);
                    let image_error = (self.gl_get_error)();
                    if texture == 0 || image_error != GL_NO_ERROR {
                        if texture != 0 {
                            (self.gl_delete_textures)(1, &texture);
                        }
                        destroy_image(self.display, image);
                        return Err(format!(
                            "failed to bind EGLImage to GL texture: GL {image_error:#x}"
                        ));
                    }

                    let mut framebuffer = 0;
                    (self.gl_gen_framebuffers)(1, &mut framebuffer);
                    (self.gl_bind_framebuffer)(GL_FRAMEBUFFER, framebuffer);
                    (self.gl_framebuffer_texture_2d)(
                        GL_FRAMEBUFFER,
                        GL_COLOR_ATTACHMENT0,
                        GL_TEXTURE_2D,
                        texture,
                        0,
                    );
                    let status = (self.gl_check_framebuffer_status)(GL_FRAMEBUFFER);
                    (self.gl_bind_framebuffer)(GL_FRAMEBUFFER, 0);
                    if framebuffer == 0 || status != GL_FRAMEBUFFER_COMPLETE {
                        if framebuffer != 0 {
                            (self.gl_delete_framebuffers)(1, &framebuffer);
                        }
                        (self.gl_delete_textures)(1, &texture);
                        destroy_image(self.display, image);
                        return Err(format!(
                            "MTLTexture framebuffer is incomplete: GL {status:#x}"
                        ));
                    }

                    Ok(ExternalSurface::Texture {
                        image,
                        texture,
                        framebuffer,
                    })
                }
            }
        }

        impl GLPlatformContext for EglCtx {
            fn make_current(&self) -> bool {
                unsafe {
                    (self.make_current)(self.display, self.pbuffer, self.pbuffer, self.ctx) != 0
                }
            }
            fn bind_save(&self) -> super::SavedGlContext {
                // 先记录当前线程的 EGL 上下文（宿主如 Flutter 的），然后切到我们的。
                let saved = unsafe {
                    super::SavedGlContext {
                        display: (self.get_current_display)().cast(),
                        draw: (self.get_current_surface)(0x305A /* EGL_DRAW */).cast(),
                        read: (self.get_current_surface)(0x305B /* EGL_READ */).cast(),
                        context: (self.get_current_context)().cast(),
                    }
                };
                let _ = self.make_current();
                saved
            }
            fn restore(&self, saved: super::SavedGlContext) {
                // 恢复宿主的 EGL 上下文；如果宿主没有上下文（context==null），
                // 传 EGL_NO_* 解除当前绑定，避免我们的上下文继续占用线程。
                if saved.context.is_null() {
                    // eglMakeCurrent(display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT)
                    let _ = unsafe {
                        (self.make_current)(
                            if saved.display.is_null() {
                                self.display
                            } else {
                                saved.display.cast()
                            },
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                        )
                    };
                } else {
                    let _ = unsafe {
                        (self.make_current)(
                            saved.display.cast(),
                            saved.draw.cast(),
                            saved.read.cast(),
                            saved.context.cast(),
                        )
                    };
                }
            }

            fn set_external_surface(
                &self,
                kind: i32,
                handle: *mut c_void,
                width: i32,
                height: i32,
            ) -> Result<(), String> {
                if handle.is_null() || width <= 0 || height <= 0 {
                    return Err("invalid external surface handle or dimensions".into());
                }
                self.clear_external_surface();
                let external = unsafe {
                    match kind {
                        1 => ExternalSurface::Surface {
                            surface: (self.create_window_surface)(
                                self.display,
                                self.config,
                                handle,
                                [EGL_NONE].as_ptr(),
                            ),
                            swap_on_present: true,
                        },
                        2 => {
                            let attrs = [
                                EGL_WIDTH,
                                width,
                                EGL_HEIGHT,
                                height,
                                EGL_IOSURFACE_PLANE_ANGLE,
                                0,
                                EGL_TEXTURE_TARGET,
                                self.iosurface_texture_target,
                                EGL_TEXTURE_FORMAT,
                                EGL_TEXTURE_RGBA,
                                EGL_TEXTURE_TYPE_ANGLE,
                                GL_UNSIGNED_BYTE,
                                EGL_TEXTURE_INTERNAL_FORMAT_ANGLE,
                                GL_BGRA_EXT,
                                EGL_NONE,
                            ];
                            ExternalSurface::Surface {
                                surface: (self.create_pbuffer_from_client_buffer)(
                                    self.display,
                                    EGL_IOSURFACE_ANGLE,
                                    handle,
                                    self.config,
                                    attrs.as_ptr(),
                                ),
                                swap_on_present: false,
                            }
                        }
                        3 => self.create_metal_texture_target(handle)?,
                        _ => return Err(format!("unknown external surface kind {kind}")),
                    }
                };
                if let ExternalSurface::Surface { surface, .. } = &external
                    && surface.is_null()
                {
                    return Err(format!(
                        "failed to create EGL external surface kind {kind}: EGL {:#x} (target={:#x}, size={}x{})",
                        unsafe { (self.get_error)() },
                        self.iosurface_texture_target,
                        width,
                        height,
                    ));
                }
                *self.external_surface.lock().unwrap() = Some(external);
                Ok(())
            }

            fn clear_external_surface(&self) {
                if let Some(surface) = self.external_surface.lock().unwrap().take() {
                    unsafe {
                        match surface {
                            ExternalSurface::Surface { surface, .. } => {
                                if (self.get_current_surface)(0x305A /* EGL_DRAW */) == surface {
                                    let _ = self.make_current();
                                }
                                (self.destroy_surface)(self.display, surface);
                            }
                            ExternalSurface::Texture {
                                image,
                                texture,
                                framebuffer,
                            } => {
                                let _ = self.make_current();
                                (self.gl_bind_framebuffer)(GL_FRAMEBUFFER, 0);
                                (self.gl_delete_framebuffers)(1, &framebuffer);
                                (self.gl_delete_textures)(1, &texture);
                                if let Some(destroy_image) = self.destroy_image {
                                    destroy_image(self.display, image);
                                }
                            }
                        }
                    }
                }
            }

            fn bind_external_surface(&self) -> Result<(), String> {
                let guard = self.external_surface.lock().unwrap();
                let output = guard
                    .as_ref()
                    .ok_or_else(|| "external surface is not configured".to_string())?;
                match output {
                    ExternalSurface::Surface { surface, .. } => {
                        if unsafe {
                            (self.make_current)(self.display, *surface, *surface, self.ctx)
                        } == 0
                        {
                            return Err(format!(
                                "eglMakeCurrent(external surface) failed: EGL {:#x}",
                                unsafe { (self.get_error)() }
                            ));
                        }
                    }
                    ExternalSurface::Texture { framebuffer, .. } => unsafe {
                        if !self.make_current() {
                            return Err("failed to make Metal texture context current".into());
                        }
                        (self.gl_bind_framebuffer)(GL_FRAMEBUFFER, *framebuffer);
                    },
                }
                Ok(())
            }

            fn present_external_surface(&self) -> Result<(), String> {
                let swap_result = {
                    let guard = self.external_surface.lock().unwrap();
                    let output = guard
                        .as_ref()
                        .ok_or_else(|| "external surface is not configured".to_string())?;
                    match output {
                        ExternalSurface::Surface {
                            surface,
                            swap_on_present,
                        } => {
                            !swap_on_present
                                || unsafe { (self.swap_buffers)(self.display, *surface) != 0 }
                        }
                        ExternalSurface::Texture { .. } => {
                            unsafe {
                                (self.gl_flush)();
                                (self.gl_bind_framebuffer)(GL_FRAMEBUFFER, 0);
                            }
                            true
                        }
                    }
                };
                let restored = self.make_current();
                if !swap_result {
                    return Err(format!(
                        "eglSwapBuffers(external surface) failed: EGL {:#x}",
                        unsafe { (self.get_error)() }
                    ));
                }
                if !restored {
                    return Err("failed to restore internal EGL pbuffer".into());
                }
                Ok(())
            }

            fn restore_internal_surface(&self) -> Result<(), String> {
                if !self.make_current() {
                    return Err("failed to restore internal EGL pbuffer".into());
                }
                unsafe { (self.gl_bind_framebuffer)(GL_FRAMEBUFFER, 0) };
                Ok(())
            }
        }

        impl Drop for EglCtx {
            fn drop(&mut self) {
                unsafe {
                    self.clear_external_surface();
                    if (self.get_current_context)() == self.ctx {
                        (self.make_current)(
                            self.display,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                        );
                    }
                    (self.destroy_surface)(self.display, self.pbuffer);
                    (self.destroy)(self.display, self.ctx);
                    (self.terminate)(self.display);
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
                // ── Linux / mesa 特殊处理 ────────────────────────────────
                // mesa 的 libGLESv2 是 dispatch 层，需要桌面 GL 符号（glTexImage2D
                // 等），这些符号来自 libGL。必须先以 RTLD_GLOBAL 把 libGL 装进全局
                // 符号表，否则 libGLESv2 初始化时 ld.so 会报大量 "undefined symbol"
                // 并标为 fatal，导致后续 GL 调用 SIGSEGV。
                #[cfg(target_os = "linux")]
                let desktop_gl_library = Some(open_library("libGL.so.1", true).map_err(|_| {
                    "libGL.so.1 not found — install mesa/libGL (e.g. libglvnd)".to_string()
                })?);
                #[cfg(not(target_os = "linux"))]
                let desktop_gl_library = None;

                // Load libEGL。Linux 没有独立 ANGLE 时走 mesa libEGL，需要
                // RTLD_GLOBAL 让 EGL/GL 跨库共享符号。
                let egl_candidates = if cfg!(target_os = "ios") {
                    vec![crate::ffi::angle_lib_path("libEGL.framework/libEGL")]
                } else if cfg!(target_os = "macos") {
                    vec![crate::ffi::angle_lib_path("libEGL.dylib")]
                } else if cfg!(target_os = "android") {
                    // Android 系统自带 libEGL.so，无需 ANGLE。
                    vec!["libEGL.so".to_string()]
                } else if cfg!(target_os = "linux") {
                    // 优先加载带版本号的 .so.1（mesa 标准）；fallback 到 libEGL.so。
                    vec![
                        "libEGL.so.1".to_string(),
                        crate::ffi::angle_lib_path("libEGL.so"),
                    ]
                } else {
                    vec![crate::ffi::angle_lib_path("libEGL.dll")]
                };
                let egl_lib = open_first(
                    &egl_candidates,
                    cfg!(target_os = "linux") || cfg!(target_os = "android"),
                )
                .map_err(|error| {
                    format!("libEGL not found — install mesa EGL or bundle ANGLE: {error}")
                })?;

                let egl_get_display: unsafe extern "C" fn(EGLint) -> EGLDisplay =
                    load!(egl_lib, "eglGetDisplay");
                let egl_initialize: unsafe extern "C" fn(
                    EGLDisplay,
                    *mut EGLint,
                    *mut EGLint,
                ) -> EGLBoolean = load!(egl_lib, "eglInitialize");

                // ── Display 创建 ──────────────────────────────────────
                // macOS/Windows ANGLE：优先 eglGetPlatformDisplay (EGL 1.5)，再试
                //   eglGetPlatformDisplayEXT（ANGLE 扩展别名）；都失败则退回 eglGetDisplay。
                // Linux/Android mesa：mesa libEGL 不支持 ANGLE 的 EGL_PLATFORM_ANGLE_* 属性，
                //   走 eglGetPlatformDisplay 反而会返回 NULL 并在 ld.so 留下
                //   "eglGetPlatformDisplayEXT undefined symbol (fatal)" 噪音。
                //   直接用 eglGetDisplay(EGL_DEFAULT_DISPLAY) 即可。
                let display = if cfg!(target_os = "linux") || cfg!(target_os = "android") {
                    egl_get_display(EGL_DEFAULT_DISPLAY)
                } else {
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
                    ["eglGetPlatformDisplay", "eglGetPlatformDisplayEXT"]
                        .iter()
                        .find_map(|name| {
                            let f: PfPlatformDisplay = optional_symbol(&egl_lib, name)?;
                            let d = f(
                                EGL_PLATFORM_ANGLE_ANGLE,
                                EGL_DEFAULT_DISPLAY as *mut c_void,
                                attribs.as_ptr(),
                            );
                            if d.is_null() { None } else { Some(d) }
                        })
                        .unwrap_or_else(|| egl_get_display(EGL_DEFAULT_DISPLAY))
                };
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
                let egl_create_window_surface: unsafe extern "C" fn(
                    EGLDisplay,
                    EGLConfig,
                    *mut c_void,
                    *const EGLint,
                ) -> EGLSurface = load!(egl_lib, "eglCreateWindowSurface");
                let egl_create_pbuffer_from_client_buffer: unsafe extern "C" fn(
                    EGLDisplay,
                    EGLint,
                    *mut c_void,
                    EGLConfig,
                    *const EGLint,
                )
                    -> EGLSurface = load!(egl_lib, "eglCreatePbufferFromClientBuffer");
                let egl_create_image = optional_symbol(&egl_lib, "eglCreateImageKHR");
                let egl_destroy_image = optional_symbol(&egl_lib, "eglDestroyImageKHR");
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
                let egl_swap_buffers: unsafe extern "C" fn(EGLDisplay, EGLSurface) -> EGLBoolean =
                    load!(egl_lib, "eglSwapBuffers");
                let egl_get_error: unsafe extern "C" fn() -> EGLint = load!(egl_lib, "eglGetError");
                // 查询当前线程的 EGL 上下文（bind_save/restore 用来保存/恢复宿主上下文）。
                let egl_get_current_display: unsafe extern "C" fn() -> EGLDisplay =
                    load!(egl_lib, "eglGetCurrentDisplay");
                let egl_get_current_surface: unsafe extern "C" fn(EGLint) -> EGLSurface =
                    load!(egl_lib, "eglGetCurrentSurface");
                let egl_get_current_context: unsafe extern "C" fn() -> EGLContext =
                    load!(egl_lib, "eglGetCurrentContext");

                if display.is_null() {
                    return Err("eglGetDisplay failed".into());
                }
                if egl_initialize(display, std::ptr::null_mut(), std::ptr::null_mut()) == 0 {
                    return Err("eglInitialize failed".into());
                }

                // 告诉 EGL 我们要用 OpenGL ES API（mesa 严格要求；ANGLE 宽松但
                // 也接受此调用）。必须在 eglCreateContext 之前。
                let egl_bind_api: unsafe extern "C" fn(EGLint) -> EGLBoolean =
                    load!(egl_lib, "eglBindAPI");
                if egl_bind_api(EGL_OPENGL_ES_API) == 0 {
                    return Err("eglBindAPI(EGL_OPENGL_ES_API) failed".into());
                }

                let config_attrs = [
                    EGL_RENDERABLE_TYPE,
                    if cfg!(any(target_os = "ios", target_os = "android")) {
                        EGL_OPENGL_ES3_BIT
                    } else {
                        EGL_OPENGL_ES2_BIT
                    },
                    EGL_SURFACE_TYPE,
                    if cfg!(target_os = "android") {
                        EGL_PBUFFER_BIT | EGL_WINDOW_BIT
                    } else {
                        EGL_PBUFFER_BIT
                    },
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

                // EGL_ANGLE_iosurface_client_buffer requires rectangle textures.
                // The bundled MetalANGLE validates this even on iOS and rejects
                // EGL_TEXTURE_2D with EGL_BAD_PARAMETER.
                let iosurface_texture_target = EGL_TEXTURE_RECTANGLE_ANGLE;

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

                let context_version = if cfg!(any(target_os = "ios", target_os = "android")) {
                    3
                } else {
                    2
                };
                let ctx_attrs = [
                    0x3098, /* EGL_CONTEXT_CLIENT_VERSION */
                    context_version,
                    EGL_NONE,
                ];
                let ctx =
                    egl_create_context(display, config, std::ptr::null_mut(), ctx_attrs.as_ptr());
                if ctx.is_null() {
                    return Err("eglCreateContext failed".into());
                }

                if egl_make_current(display, surface, surface, ctx) == 0 {
                    return Err("eglMakeCurrent failed".into());
                }

                // Load GLESv2 for glow. Linux 必须用 RTLD_GLOBAL 让 mesa 的
                // dispatch 层找到已预加载的 libGL 桌面符号。
                let gles_candidates = if cfg!(target_os = "ios") {
                    vec![crate::ffi::angle_lib_path("libGLESv2.framework/libGLESv2")]
                } else if cfg!(target_os = "macos") {
                    vec![crate::ffi::angle_lib_path("libGLESv2.dylib")]
                } else if cfg!(target_os = "android") {
                    // Android 系统自带 libGLESv2.so。
                    vec!["libGLESv2.so".to_string()]
                } else if cfg!(target_os = "linux") {
                    // 先尝试带版本号的 .so.2（mesa 标准命名）。
                    vec![
                        "libGLESv2.so.2".to_string(),
                        crate::ffi::angle_lib_path("libGLESv2.so"),
                    ]
                } else {
                    vec![crate::ffi::angle_lib_path("libGLESv2.dll")]
                };
                let gles_lib = open_first(
                    &gles_candidates,
                    cfg!(target_os = "linux") || cfg!(target_os = "android"),
                )
                .map_err(|error| format!("libGLESv2 not found: {error}"))?;

                let gl_gen_textures: unsafe extern "C" fn(i32, *mut u32) =
                    load!(gles_lib, "glGenTextures");
                let gl_delete_textures: unsafe extern "C" fn(i32, *const u32) =
                    load!(gles_lib, "glDeleteTextures");
                let gl_bind_texture: unsafe extern "C" fn(u32, u32) =
                    load!(gles_lib, "glBindTexture");
                let gl_egl_image_target_texture_2d =
                    optional_symbol(&gles_lib, "glEGLImageTargetTexture2DOES");
                let gl_gen_framebuffers: unsafe extern "C" fn(i32, *mut u32) =
                    load!(gles_lib, "glGenFramebuffers");
                let gl_delete_framebuffers: unsafe extern "C" fn(i32, *const u32) =
                    load!(gles_lib, "glDeleteFramebuffers");
                let gl_bind_framebuffer: unsafe extern "C" fn(u32, u32) =
                    load!(gles_lib, "glBindFramebuffer");
                let gl_framebuffer_texture_2d: unsafe extern "C" fn(u32, u32, u32, u32, i32) =
                    load!(gles_lib, "glFramebufferTexture2D");
                let gl_check_framebuffer_status: unsafe extern "C" fn(u32) -> u32 =
                    load!(gles_lib, "glCheckFramebufferStatus");
                let gl_flush: unsafe extern "C" fn() = load!(gles_lib, "glFlush");
                let gl_get_error: unsafe extern "C" fn() -> u32 = load!(gles_lib, "glGetError");

                // ── GL 函数指针加载 ────────────────────────────────────
                // EGL 标准提供 eglGetProcAddress 取 GLES 函数；对桌面 GL 扩展符号
                // mesa 可能返回 NULL，此时退回 dlsym(gles_lib)。
                // macOS/ANGLE 上 dlsym 已经够用，保留原路径。
                type EglGetProcAddress = unsafe extern "C" fn(*const c_char) -> *const c_void;
                let egl_get_proc_addr: Option<EglGetProcAddress> =
                    optional_symbol(&egl_lib, "eglGetProcAddress");

                let gl = glow::Context::from_loader_function(|s| {
                    let cs = CString::new(s).unwrap();
                    // 1) 优先 eglGetProcAddress（EGL 标准；mesa/ANGLE 都支持）。
                    if let Some(f) = egl_get_proc_addr {
                        let p = f(cs.as_ptr());
                        if !p.is_null() {
                            return p;
                        }
                    }
                    // 2) 退回 dlsym（扩展名或 macOS 路径）。
                    symbol_address(&gles_lib, &cs)
                });

                Ok((
                    Rc::new(gl),
                    EglCtx {
                        display,
                        pbuffer: surface,
                        config,
                        ctx,
                        destroy: egl_destroy_context,
                        destroy_surface: egl_destroy_surface,
                        terminate: egl_terminate,
                        make_current: egl_make_current,
                        get_current_display: egl_get_current_display,
                        get_current_surface: egl_get_current_surface,
                        get_current_context: egl_get_current_context,
                        create_window_surface: egl_create_window_surface,
                        create_pbuffer_from_client_buffer: egl_create_pbuffer_from_client_buffer,
                        create_image: egl_create_image,
                        destroy_image: egl_destroy_image,
                        gl_gen_textures,
                        gl_delete_textures,
                        gl_bind_texture,
                        gl_egl_image_target_texture_2d,
                        gl_gen_framebuffers,
                        gl_delete_framebuffers,
                        gl_bind_framebuffer,
                        gl_framebuffer_texture_2d,
                        gl_check_framebuffer_status,
                        gl_flush,
                        gl_get_error,
                        swap_buffers: egl_swap_buffers,
                        get_error: egl_get_error,
                        iosurface_texture_target,
                        external_surface: Mutex::new(None),
                        _egl_library: egl_lib,
                        _gles_library: gles_lib,
                        _desktop_gl_library: desktop_gl_library,
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
        // Use GL_RGBA for both internalformat and format. This is the GLES 2.0
        // compatible path and avoids ANGLE/Metal treating sized desktop formats
        // differently from the ES context we create below.
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
        configure_render_texture(gl);
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
            configure_render_texture(gl);
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

unsafe fn configure_render_texture(gl: &glow::Context) {
    unsafe {
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MAG_FILTER,
            glow::LINEAR as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_S,
            glow::CLAMP_TO_EDGE as i32,
        );
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_WRAP_T,
            glow::CLAMP_TO_EDGE as i32,
        );
    }
}

/// Reads the current framebuffer into `out` as top-left-origin RGBA8 pixels.
///
/// # Safety
///
/// `gl` must have a current context on this thread, with a readable framebuffer
/// whose dimensions are at least `width` by `height`.
pub unsafe fn read_pixels_into(
    gl: &glow::Context,
    width: i32,
    height: i32,
    out: &mut [u8],
) -> usize {
    let Some((row_bytes, total)) = pixel_buffer_layout(width, height) else {
        return 0;
    };
    if out.len() < total {
        return 0;
    }
    let buf = &mut out[..total];

    unsafe {
        gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);

        gl.read_pixels(
            0,
            0,
            width,
            height,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(buf)),
        );
    }

    // 垂直翻转（OpenGL 原点在左下，我们需要左上）
    flip_rows_in_place(buf, row_bytes, height as usize);
    total
}

pub unsafe fn read_pixels(gl: &glow::Context, width: i32, height: i32) -> Vec<u8> {
    let Some((_, total)) = pixel_buffer_layout(width, height) else {
        return Vec::new();
    };
    let mut buf = vec![0u8; total];
    let written = unsafe { read_pixels_into(gl, width, height, &mut buf) };
    buf.truncate(written);
    buf
}

fn pixel_buffer_layout(width: i32, height: i32) -> Option<(usize, usize)> {
    let width = usize::try_from(width).ok()?;
    let height = usize::try_from(height).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    let row_bytes = width.checked_mul(4)?;
    let total = row_bytes.checked_mul(height)?;
    Some((row_bytes, total))
}

fn flip_rows_in_place(buf: &mut [u8], row_bytes: usize, height: usize) {
    for y in 0..height / 2 {
        let top = y * row_bytes;
        let bottom = (height - 1 - y) * row_bytes;
        let (top_region, bottom_region) = buf.split_at_mut(bottom);
        top_region[top..top + row_bytes].swap_with_slice(&mut bottom_region[..row_bytes]);
    }
}

/// 从当前绑定的 FBO 读取像素（原点在左上，Y 向下）。
///
/// 与 [`read_pixels`] 相同，但语义上明确表示从 FBO 读取。
pub unsafe fn read_pixels_from_fbo(gl: &glow::Context, width: i32, height: i32) -> Vec<u8> {
    unsafe { read_pixels(gl, width, height) }
}

#[cfg(test)]
mod tests {
    use super::{flip_rows_in_place, pixel_buffer_layout};

    #[test]
    fn pixel_buffer_layout_rejects_invalid_or_overflowing_sizes() {
        assert_eq!(pixel_buffer_layout(2, 3), Some((8, 24)));
        assert_eq!(pixel_buffer_layout(0, 3), None);
        assert_eq!(pixel_buffer_layout(-1, 3), None);
    }

    #[test]
    fn flip_rows_swaps_in_place_and_keeps_middle_row() {
        let mut pixels = vec![
            1, 2, 3, 4, // top
            5, 6, 7, 8, // middle
            9, 10, 11, 12, // bottom
        ];

        flip_rows_in_place(&mut pixels, 4, 3);

        assert_eq!(
            pixels,
            vec![
                9, 10, 11, 12, // former bottom
                5, 6, 7, 8, // middle
                1, 2, 3, 4, // former top
            ]
        );
    }
}
