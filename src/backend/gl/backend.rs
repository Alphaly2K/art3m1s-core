use super::platform::{self, GLPlatformContext, SavedGlContext};
use super::{GlRenderer, GlTextureProvider, ShaderProfile};
use crate::backend::{
    AssetSource, FrameCapture, GpuBackend, GpuProfileStats, OutputSurface, OutputSurfaceKind,
    RenderRegion,
};
use crate::render_pipeline::draw::{DrawList, TextureId, TextureInfo, TextureProvider};
use glow::HasContext;
use std::collections::HashSet;
use std::rc::Rc;

/// Reference OpenGL/ANGLE backend.
///
/// This object owns every GL-specific runtime resource: context, persistent
/// render target, renderer, texture provider, output surface state and the
/// temporary context lease used by the existing libmpv integration.
pub struct GlBackend {
    gl: Rc<glow::Context>,
    framebuffer: glow::Framebuffer,
    framebuffer_texture: glow::Texture,
    renderer: GlRenderer,
    textures: GlTextureProvider,
    output_surface: Option<OutputSurface>,
    access_depth: usize,
    saved_host_context: Option<SavedGlContext>,
    external_render_context: Option<SavedGlContext>,
    // Must drop after all GL-owned fields.
    platform_context: Box<dyn GLPlatformContext>,
}

impl GlBackend {
    pub fn new(config: platform::GfxBackend, width: u32, height: u32) -> Result<Self, String> {
        let (gl, platform_context, effective_backend) =
            platform::create_offscreen_context(config, width, height)?;
        let (framebuffer, framebuffer_texture) = unsafe {
            platform::create_fbo_target(&gl, width as i32, height as i32)
                .map_err(|error| format!("FBO: {error}"))?
        };
        let profile = match effective_backend {
            platform::GfxBackend::Cgl => ShaderProfile::GlCore330,
            platform::GfxBackend::Angle(_) => ShaderProfile::Gles300,
        };
        let renderer = GlRenderer::new(gl.clone(), width, height, profile)
            .map_err(|error| format!("创建渲染器失败: {error}"))?;
        let textures = GlTextureProvider::new(gl.clone());
        Ok(Self {
            gl,
            framebuffer,
            framebuffer_texture,
            renderer,
            textures,
            output_surface: None,
            access_depth: 0,
            saved_host_context: None,
            external_render_context: None,
            platform_context,
        })
    }
}

impl TextureProvider for GlBackend {
    fn resolve(&mut self, name: &str) -> Option<(TextureId, TextureInfo)> {
        self.textures.resolve(name)
    }

    fn upload_rgba(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        self.textures.upload_rgba(name, width, height, data)
    }

    fn upload_rgba_render_only(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        self.textures
            .upload_rgba_render_only(name, width, height, data)
    }

    fn upload_dxt5_render_only(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        self.textures
            .upload_dxt5_render_only(name, width, height, data)
    }

    fn supports_astc_4x4(&self) -> bool {
        self.textures.supports_astc_4x4()
    }

    fn upload_astc_4x4_render_only(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        self.textures
            .upload_astc_4x4_render_only(name, width, height, data)
    }

    fn pixel_alpha(&self, texture: TextureId, x: u32, y: u32) -> Option<u8> {
        self.textures.pixel_alpha(texture, x, y)
    }

    fn texture_is_opaque(&self, texture: TextureId) -> bool {
        self.textures.texture_is_opaque(texture)
    }

    fn retain(&mut self, names: &HashSet<String>) {
        self.textures.retain(names);
    }

    fn solid_texture(&mut self, rgba: [u8; 4]) -> Option<(TextureId, TextureInfo)> {
        self.textures.solid_texture(rgba)
    }

    fn resolve_with_mask(&mut self, file: &str, mask: &str) -> Option<(TextureId, TextureInfo)> {
        self.textures.resolve_with_mask(file, mask)
    }

    fn pixels_of(&mut self, name: &str) -> Option<(u32, u32, Vec<u8>)> {
        self.textures.pixels_of(name)
    }
}

impl GpuBackend for GlBackend {
    fn begin_access(&mut self) {
        if self.access_depth == 0 {
            self.saved_host_context = Some(self.platform_context.bind_save());
        }
        self.access_depth += 1;
    }

    fn end_access(&mut self) {
        if self.access_depth == 0 {
            crate::core_warn!("[GlBackend] unbalanced end_access");
            return;
        }
        self.access_depth -= 1;
        if self.access_depth == 0
            && let Some(saved) = self.saved_host_context.take()
        {
            self.platform_context.restore(saved);
        }
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<(), String> {
        let (new_framebuffer, new_texture) = unsafe {
            platform::create_fbo_target(&self.gl, width as i32, height as i32)
                .map_err(|error| format!("重新创建 FBO 失败: {error}"))?
        };
        unsafe {
            self.gl.delete_framebuffer(self.framebuffer);
            self.gl.delete_texture(self.framebuffer_texture);
        }
        self.framebuffer = new_framebuffer;
        self.framebuffer_texture = new_texture;
        self.renderer.set_viewport_size(width, height);
        self.renderer.set_stage_size(width, height);
        Ok(())
    }

    fn begin_frame(&mut self) {
        unsafe {
            self.gl
                .bind_framebuffer(glow::FRAMEBUFFER, Some(self.framebuffer));
        }
    }

    fn end_frame(&mut self) {
        unsafe { self.gl.bind_framebuffer(glow::FRAMEBUFFER, None) };
    }

    fn capture_frame(&mut self, name: &str, width: u32, height: u32) -> FrameCapture {
        self.textures
            .copy_bound_framebuffer_render_only(name, width, height)
            .map(|(texture, info)| FrameCapture::Texture(texture, info))
            .unwrap_or_else(|| {
                FrameCapture::Pixels(unsafe {
                    platform::read_pixels(&self.gl, width as i32, height as i32)
                })
            })
    }

    fn read_frame_into(&mut self, width: u32, height: u32, out: &mut [u8]) -> usize {
        self.begin_frame();
        let written =
            unsafe { platform::read_pixels_into(&self.gl, width as i32, height as i32, out) };
        self.end_frame();
        written
    }

    fn read_frame(&mut self, width: u32, height: u32) -> Vec<u8> {
        self.begin_frame();
        unsafe { self.gl.finish() };
        let pixels = unsafe { platform::read_pixels(&self.gl, width as i32, height as i32) };
        self.end_frame();
        pixels
    }

    fn render(&mut self, frame: &DrawList) -> RenderRegion {
        crate::render_pipeline::draw::Renderer::render(&mut self.renderer, frame);
        RenderRegion::Full
    }

    fn render_damage(&mut self, frame: &DrawList, damage: [f32; 4]) -> RenderRegion {
        self.renderer.render_damage(frame, damage)
    }

    fn render_damage_visualized(&mut self, frame: &DrawList, damage: [f32; 4]) -> RenderRegion {
        self.renderer.render_damage_visualized(frame, damage)
    }

    fn render_visualized(&mut self, frame: &DrawList) -> RenderRegion {
        self.renderer.render_visualized(frame)
    }

    fn clear_damage_overlay(&mut self, frame: &DrawList) -> Option<RenderRegion> {
        self.renderer.clear_damage_overlay(frame)
    }

    fn replace_asset_source(&mut self, source: Box<AssetSource>) {
        self.textures = GlTextureProvider::new(self.gl.clone()).with_source(source);
    }

    fn cached_texture_info(&self, name: &str) -> Option<TextureInfo> {
        self.textures.cached_info(name)
    }

    fn texture_content_revision(&self) -> u64 {
        self.textures.content_revision()
    }

    fn changed_texture_ids_since(&self, revision: u64) -> HashSet<TextureId> {
        self.textures.changed_texture_ids_since(revision)
    }

    fn evict_texture_prefix(&mut self, prefix: &str) -> usize {
        self.textures.evict_prefix(prefix)
    }

    fn upload_video_rgba(&mut self, name: &str, width: u32, height: u32, rgba: &[u8]) -> bool {
        self.textures.upload_video_rgba(name, width, height, rgba)
    }

    fn set_output_surface(&mut self, surface: OutputSurface) -> Result<(), String> {
        let width = i32::try_from(surface.width).map_err(|_| "external width overflow")?;
        let height = i32::try_from(surface.height).map_err(|_| "external height overflow")?;
        self.output_surface = None;
        self.platform_context.set_external_surface(
            surface.kind.legacy_int(),
            surface.handle,
            width,
            height,
        )?;
        self.output_surface = Some(surface);
        Ok(())
    }

    fn clear_output_surface(&mut self) {
        self.platform_context.clear_external_surface();
        self.output_surface = None;
    }

    fn present(&mut self, damage: Option<[f32; 4]>) -> Result<(), String> {
        let surface = self
            .output_surface
            .ok_or_else(|| "external surface is not configured".to_string())?;
        let width = i32::try_from(surface.width).map_err(|_| "external width overflow")?;
        let height = i32::try_from(surface.height).map_err(|_| "external height overflow")?;
        let top_left_memory = matches!(
            surface.kind,
            OutputSurfaceKind::AppleIoSurface | OutputSurfaceKind::AppleMetalTexture
        );
        let damage = if surface.kind == OutputSurfaceKind::AndroidNativeWindow {
            None
        } else {
            damage
        };
        self.platform_context.bind_external_surface()?;
        if let Err(error) = self.renderer.present_texture(
            self.framebuffer_texture,
            width,
            height,
            damage,
            top_left_memory,
        ) {
            let _ = self.platform_context.restore_internal_surface();
            return Err(error);
        }
        self.platform_context.present_external_surface()
    }

    fn register_hlsl_shader(&mut self, name: &str, source: &[u8]) -> Result<(), String> {
        self.renderer.register_hlsl_shader(name, source)
    }

    fn set_profile_enabled(&self, enabled: bool) {
        self.renderer.set_profile_enabled(enabled);
        self.textures.set_profile_enabled(enabled);
    }

    fn take_profile_stats(&self) -> GpuProfileStats {
        let uploads = self.textures.take_profile_uploads();
        let render = self.renderer.take_profile_stats();
        let (texture_count, texture_gpu_bytes, texture_cpu_bytes) = self.textures.profile_memory();
        GpuProfileStats {
            texture_upload_ns: uploads.elapsed_ns,
            uploaded_bytes: uploads.bytes,
            video_upload_ns: uploads.video_elapsed_ns,
            video_uploaded_bytes: uploads.video_bytes,
            video_uploaded_frames: uploads.video_frames,
            draw_calls: render.draw_calls,
            vertices: render.vertices,
            texture_binds: render.texture_binds,
            dynamic_mesh_uploaded_bytes: render.dynamic_mesh_uploaded_bytes,
            texture_count: texture_count as u64,
            texture_gpu_bytes,
            texture_cpu_bytes,
        }
    }

    fn external_renderer_proc_address(&self, name: &str) -> *const std::ffi::c_void {
        self.platform_context.get_proc_address(name)
    }

    fn begin_external_render(&mut self) -> Result<(), String> {
        if self.external_render_context.is_some() {
            return Err("external render lease is already active".into());
        }
        let saved = self.platform_context.bind_save();
        if !self.platform_context.make_current() {
            self.platform_context.restore(saved);
            return Err("failed to make backend context current".into());
        }
        self.external_render_context = Some(saved);
        Ok(())
    }

    fn external_render_target(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
    ) -> Result<u64, String> {
        if self.external_render_context.is_none() {
            return Err("external render lease is not active".into());
        }
        self.textures
            .ensure_video_render_target(name, width, height)
            .map(u64::from)
    }

    fn commit_external_render_target(&mut self, name: &str) -> bool {
        self.external_render_context.is_some() && self.textures.commit_video_render_target(name)
    }

    fn end_external_render(&mut self) {
        if let Some(saved) = self.external_render_context.take() {
            self.platform_context.restore(saved);
        }
    }
}

impl Drop for GlBackend {
    fn drop(&mut self) {
        if self.platform_context.make_current() {
            unsafe {
                self.gl.delete_framebuffer(self.framebuffer);
                self.gl.delete_texture(self.framebuffer_texture);
            }
        } else {
            crate::core_warn!("[GlBackend] context unavailable during destruction");
        }
    }
}
