use super::platform::{self, GLPlatformContext, SavedGlContext};
use super::{GlRenderer, GlTextureProvider, ShaderProfile};
use crate::backend::{
    AssetSource, BackendCapabilities, BackendInfo, BackendKind, BackendStability, Extent2D,
    ExternalImage, ExternalImageKind, ExternalTextureHandle, FrameCapture, FrameTarget, GpuBackend,
    GpuProfileStats, NativeSurface, NativeSurfaceKind, RenderRegion, RenderTarget,
    RenderTargetDesc, RenderTargetId, ShaderId, TextureData, TextureDesc, TextureFormat,
    TextureOrigin, TextureUpdate, TextureUsage, VideoImportCapability, VideoSurfaceHandle,
};
use crate::render_pipeline::draw::{DrawList, TextureId, TextureInfo, TextureProvider};
use glow::HasContext;
use std::collections::HashMap;
use std::collections::HashSet;
use std::num::NonZeroU32;
use std::rc::Rc;

/// Reference OpenGL/ANGLE backend.
///
/// This object owns every GL-specific runtime resource: context, persistent
/// render target, renderer, texture provider, output surface state and the
/// temporary context lease used by the existing libmpv integration.
pub struct GlBackend {
    gl: Rc<glow::Context>,
    framebuffer: glow::Framebuffer,
    main_target: RenderTarget,
    renderer: GlRenderer,
    textures: GlTextureProvider,
    texture_descs: HashMap<TextureId, TextureDesc>,
    render_targets: HashMap<RenderTargetId, RenderTarget>,
    native_surface: Option<NativeSurface>,
    shader_ids: HashMap<String, ShaderId>,
    next_shader_id: u64,
    access_depth: usize,
    saved_host_context: Option<SavedGlContext>,
    external_render_context: Option<SavedGlContext>,
    video_surfaces: HashMap<VideoSurfaceHandle, (String, u32)>,
    next_video_handle: u64,
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
        let main_target = RenderTarget {
            id: RenderTargetId::from_opaque(framebuffer.0.get() as u64),
            color: TextureId(framebuffer_texture.0.get() as u64),
            desc: RenderTargetDesc::sampled_rgba8(width, height),
        };
        Ok(Self {
            gl,
            framebuffer,
            main_target,
            renderer,
            textures,
            texture_descs: HashMap::new(),
            render_targets: HashMap::new(),
            native_surface: None,
            shader_ids: HashMap::new(),
            next_shader_id: 1,
            access_depth: 0,
            saved_host_context: None,
            external_render_context: None,
            video_surfaces: HashMap::new(),
            next_video_handle: 1,
            platform_context,
        })
    }

    fn remember_video_name(&mut self, name: &str) -> ExternalTextureHandle {
        if let Some((handle, _)) = self
            .video_surfaces
            .iter()
            .find(|(_, (existing, _))| existing == name)
        {
            return ExternalTextureHandle::from_opaque(handle.opaque());
        }
        let handle = VideoSurfaceHandle::from_opaque(self.next_video_handle);
        self.next_video_handle = self.next_video_handle.wrapping_add(1).max(1);
        self.video_surfaces.insert(handle, (name.to_owned(), 0));
        ExternalTextureHandle::from_opaque(handle.opaque())
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
    fn backend_info(&self) -> BackendInfo {
        BackendInfo {
            kind: BackendKind::GlReference,
            name: "OpenGL/ANGLE reference",
            stability: BackendStability::Legacy,
            capabilities: BackendCapabilities {
                runtime_shader: true,
                hlsl_shader: true,
                offscreen_render_target: true,
                readback: true,
                external_texture: true,
                zero_copy_video: true,
                compressed_astc: self.supports_astc_4x4(),
                compressed_bc: self.textures.supports_bc(),
                stencil: true,
                custom_shader: true,
                dynamic_mesh: true,
                ..BackendCapabilities::default()
            },
        }
    }

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

    fn create_texture(
        &mut self,
        name: &str,
        desc: TextureDesc,
        data: TextureData<'_>,
    ) -> Result<TextureId, String> {
        if desc.extent.is_empty() {
            return Err("texture extent must be non-zero".into());
        }
        if let Some((old, _)) = self.textures.cached_entry(name) {
            self.texture_descs.remove(&old);
            self.render_targets.retain(|_, target| target.color != old);
        }
        let (width, height) = (desc.extent.width, desc.extent.height);
        let entry = match (desc.format, data) {
            (TextureFormat::Rgba8Unorm, TextureData::Rgba8(bytes)) => {
                let expected = desc.extent.rgba8_len().ok_or("texture size overflow")?;
                if bytes.len() != expected {
                    return Err(format!(
                        "texture {name} has {} bytes, expected {expected}",
                        bytes.len()
                    ));
                }
                if desc.usage.contains(TextureUsage::CPU_READABLE) {
                    self.textures.upload_rgba(name, width, height, bytes)
                } else {
                    self.textures
                        .upload_rgba_render_only(name, width, height, bytes)
                }
            }
            (TextureFormat::Bc3RgbaUnorm, TextureData::Bc3(bytes)) => {
                validate_block_data(name, desc.extent, bytes)?;
                self.textures
                    .upload_dxt5_render_only(name, width, height, bytes)
            }
            (TextureFormat::Astc4x4RgbaUnorm, TextureData::Astc4x4(bytes)) => {
                validate_block_data(name, desc.extent, bytes)?;
                self.textures
                    .upload_astc_4x4_render_only(name, width, height, bytes)
            }
            (TextureFormat::Rgba8Unorm, TextureData::Uninitialized) => {
                let bytes = vec![0; desc.extent.rgba8_len().ok_or("texture size overflow")?];
                self.textures
                    .upload_rgba_render_only(name, width, height, &bytes)
            }
            (format, _) => {
                return Err(format!("texture data does not match format {format:?}"));
            }
        };
        let texture = entry
            .map(|(texture, _)| texture)
            .ok_or_else(|| format!("failed to create texture {name}"))?;
        self.texture_descs.insert(texture, desc);
        Ok(texture)
    }

    fn update_texture(
        &mut self,
        texture: TextureId,
        update: TextureUpdate<'_>,
    ) -> Result<(), String> {
        let desc = self
            .texture_descs
            .get(&texture)
            .ok_or_else(|| "unknown texture descriptor".to_string())?;
        if !desc.usage.contains(TextureUsage::TRANSFER_DST) {
            return Err("texture was not created with TRANSFER_DST usage".into());
        }
        if !matches!(
            (desc.format, &update.data),
            (TextureFormat::Rgba8Unorm, TextureData::Rgba8(_))
        ) {
            return Err("texture update payload does not match its format".into());
        }
        self.textures.update_texture(texture, update)
    }

    fn destroy_texture(&mut self, texture: TextureId) {
        self.texture_descs.remove(&texture);
        self.render_targets
            .retain(|_, target| target.color != texture);
        self.textures.destroy_texture(texture);
    }

    fn create_render_target(
        &mut self,
        name: &str,
        desc: RenderTargetDesc,
    ) -> Result<RenderTarget, String> {
        if desc.extent.is_empty() {
            return Err("render target extent must be non-zero".into());
        }
        if desc.color_format != TextureFormat::Rgba8Unorm || desc.stencil {
            return Err("the GL reference backend currently supports RGBA8 color targets without a native stencil attachment".into());
        }
        let framebuffer =
            self.textures
                .ensure_render_target(name, desc.extent.width, desc.extent.height)?;
        let (color, _) = self
            .textures
            .resolve(name)
            .ok_or_else(|| "render target texture was not registered".to_string())?;
        let target = RenderTarget {
            id: RenderTargetId::from_opaque(framebuffer as u64),
            color,
            desc,
        };
        let mut usage = TextureUsage::RENDER_TARGET | TextureUsage::TRANSFER_SRC;
        if desc.sampled {
            usage |= TextureUsage::SAMPLED;
        }
        self.texture_descs.insert(
            color,
            TextureDesc {
                extent: desc.extent,
                format: desc.color_format,
                usage,
            },
        );
        self.render_targets.insert(target.id, target);
        Ok(target)
    }

    fn destroy_render_target(&mut self, target: RenderTargetId) {
        if let Some(target) = self.render_targets.remove(&target) {
            self.texture_descs.remove(&target.color);
        }
        self.textures.destroy_render_target(target);
    }

    fn resize(&mut self, extent: Extent2D) -> Result<(), String> {
        let (width, height) = (extent.width, extent.height);
        let (new_framebuffer, new_texture) = unsafe {
            platform::create_fbo_target(&self.gl, width as i32, height as i32)
                .map_err(|error| format!("重新创建 FBO 失败: {error}"))?
        };
        unsafe {
            self.gl.delete_framebuffer(self.framebuffer);
            if let Some(texture) = NonZeroU32::new(self.main_target.color.0 as u32) {
                self.gl.delete_texture(glow::NativeTexture(texture));
            }
        }
        self.framebuffer = new_framebuffer;
        self.main_target = RenderTarget {
            id: RenderTargetId::from_opaque(new_framebuffer.0.get() as u64),
            color: TextureId(new_texture.0.get() as u64),
            desc: RenderTargetDesc::sampled_rgba8(width, height),
        };
        self.renderer.set_viewport_size(width, height);
        self.renderer.set_stage_size(width, height);
        Ok(())
    }

    fn begin_frame(&mut self, target: FrameTarget) -> Result<(), String> {
        match target {
            FrameTarget::Main => unsafe {
                self.gl
                    .bind_framebuffer(glow::FRAMEBUFFER, Some(self.framebuffer));
            },
            FrameTarget::Offscreen(target) => self.textures.bind_render_target(target)?,
        }
        Ok(())
    }

    fn clear(&mut self, color: [f32; 4]) {
        unsafe {
            self.gl.disable(glow::SCISSOR_TEST);
            self.gl.clear_color(color[0], color[1], color[2], color[3]);
            self.gl.clear(glow::COLOR_BUFFER_BIT);
        }
    }

    fn end_frame(&mut self) {
        unsafe { self.gl.bind_framebuffer(glow::FRAMEBUFFER, None) };
    }

    fn capture_frame(&mut self, name: &str, extent: Extent2D) -> FrameCapture {
        self.textures
            .copy_bound_framebuffer_render_only(name, extent.width, extent.height)
            .map(|(texture, info)| FrameCapture::Texture(texture, info, TextureOrigin::BottomLeft))
            .unwrap_or_else(|| {
                FrameCapture::Pixels(unsafe {
                    platform::read_pixels(&self.gl, extent.width as i32, extent.height as i32)
                })
            })
    }

    fn readback(
        &mut self,
        target: FrameTarget,
        extent: Extent2D,
        out: &mut [u8],
    ) -> Result<usize, String> {
        self.begin_frame(target)?;
        let written = unsafe {
            platform::read_pixels_into(&self.gl, extent.width as i32, extent.height as i32, out)
        };
        self.end_frame();
        Ok(written)
    }

    fn render(&mut self, frame: &DrawList) -> RenderRegion {
        self.renderer.render(frame);
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
        self.texture_descs.clear();
        self.render_targets.clear();
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

    fn video_import_capability(&self) -> VideoImportCapability {
        VideoImportCapability {
            preferred: ExternalImageKind::OpenGlFramebuffer,
            cpu_rgba: true,
            cv_pixel_buffer: false,
            metal_texture: false,
            io_surface: false,
            ahardware_buffer: false,
            opengl_framebuffer: true,
        }
    }

    fn import_external_texture(
        &mut self,
        name: &str,
        image: ExternalImage<'_>,
    ) -> Result<ExternalTextureHandle, String> {
        match image.kind {
            ExternalImageKind::CpuRgba => {
                let rgba = image
                    .rgba
                    .ok_or_else(|| "CPU RGBA import requires pixel bytes".to_string())?;
                if !self.upload_video_rgba(
                    name,
                    image.extent.width,
                    image.extent.height,
                    rgba,
                ) {
                    return Err("CPU RGBA video upload failed".into());
                }
                Ok(self.remember_video_name(name))
            }
            ExternalImageKind::OpenGlFramebuffer => Err(
                "OpenGL framebuffer is acquired through the deprecated video_gl_* lease, not import"
                    .into(),
            ),
            other => Err(format!(
                "GL reference backend cannot import {:?}; use CPU RGBA or the video_gl_* shim",
                other
            )),
        }
    }

    fn release_external_texture(&mut self, handle: ExternalTextureHandle) -> bool {
        let key = VideoSurfaceHandle::from_opaque(handle.opaque());
        let Some((name, _)) = self.video_surfaces.remove(&key) else {
            return false;
        };
        self.textures.evict_prefix(&name);
        true
    }

    fn acquire_video_surface(
        &mut self,
        name: &str,
        extent: Extent2D,
    ) -> Result<VideoSurfaceHandle, String> {
        let framebuffer = self
            .textures
            .ensure_render_target(name, extent.width, extent.height)?;
        if let Some((handle, entry)) = self
            .video_surfaces
            .iter_mut()
            .find(|(_, (existing, _))| existing == name)
        {
            entry.1 = framebuffer;
            return Ok(*handle);
        }
        let handle = VideoSurfaceHandle::from_opaque(self.next_video_handle);
        self.next_video_handle = self.next_video_handle.wrapping_add(1).max(1);
        self.video_surfaces
            .insert(handle, (name.to_owned(), framebuffer));
        Ok(handle)
    }

    fn commit_video_surface(&mut self, handle: VideoSurfaceHandle) -> bool {
        let Some((name, _)) = self.video_surfaces.get(&handle) else {
            return false;
        };
        self.textures.commit_video_render_target(name)
    }

    fn video_surface_consumed(&mut self, handle: VideoSurfaceHandle) -> bool {
        self.external_render_context.is_none() || !self.video_surfaces.contains_key(&handle)
    }

    fn video_surface_gl_framebuffer(&self, handle: VideoSurfaceHandle) -> Option<u32> {
        self.video_surfaces
            .get(&handle)
            .map(|(_, framebuffer)| *framebuffer)
    }

    fn set_native_surface(&mut self, surface: NativeSurface) -> Result<(), String> {
        let width = i32::try_from(surface.extent.width).map_err(|_| "external width overflow")?;
        let height =
            i32::try_from(surface.extent.height).map_err(|_| "external height overflow")?;
        self.native_surface = None;
        self.platform_context.set_external_surface(
            surface.kind.legacy_int(),
            surface.handle,
            width,
            height,
        )?;
        self.native_surface = Some(surface);
        Ok(())
    }

    fn clear_native_surface(&mut self) {
        self.platform_context.clear_external_surface();
        self.native_surface = None;
    }

    fn present(&mut self, damage: Option<[f32; 4]>) -> Result<(), String> {
        let surface = self
            .native_surface
            .ok_or_else(|| "external surface is not configured".to_string())?;
        let width = i32::try_from(surface.extent.width).map_err(|_| "external width overflow")?;
        let height =
            i32::try_from(surface.extent.height).map_err(|_| "external height overflow")?;
        let top_left_memory = matches!(
            surface.kind,
            NativeSurfaceKind::AppleIoSurface | NativeSurfaceKind::AppleMetalTexture
        );
        let damage = if surface.kind == NativeSurfaceKind::AndroidNativeWindow {
            None
        } else {
            damage
        };
        self.platform_context.bind_external_surface()?;
        if let Err(error) = self.renderer.present_texture(
            NonZeroU32::new(self.main_target.color.0 as u32)
                .map(glow::NativeTexture)
                .ok_or_else(|| "invalid main render-target texture".to_string())?,
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

    fn register_hlsl_shader(
        &mut self,
        name: &str,
        source: &[u8],
    ) -> Result<ShaderId, crate::backend::ShaderCompileError> {
        self.renderer
            .register_hlsl_shader(name, source)
            .map_err(|error| crate::backend::ShaderCompileError::new(name, error))?;
        if let Some(id) = self.shader_ids.get(name).copied() {
            return Ok(id);
        }
        let id = ShaderId::from_opaque(self.next_shader_id);
        self.next_shader_id = self.next_shader_id.wrapping_add(1).max(1);
        self.shader_ids.insert(name.to_owned(), id);
        Ok(id)
    }

    fn unregister_shader(&mut self, name: &str) -> bool {
        self.shader_ids.remove(name);
        self.renderer.unregister_hlsl_shader(name)
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
            ..GpuProfileStats::default()
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
        let target =
            self.create_render_target(name, RenderTargetDesc::sampled_rgba8(width, height))?;
        Ok(target.id.opaque())
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

fn validate_block_data(name: &str, extent: Extent2D, bytes: &[u8]) -> Result<(), String> {
    let expected = extent
        .block_4x4_len()
        .ok_or_else(|| format!("texture {name} block size overflow"))?;
    if bytes.len() != expected {
        return Err(format!(
            "texture {name} has {} compressed bytes, expected {expected}",
            bytes.len()
        ));
    }
    Ok(())
}

impl Drop for GlBackend {
    fn drop(&mut self) {
        if self.platform_context.make_current() {
            unsafe {
                self.gl.delete_framebuffer(self.framebuffer);
                if let Some(texture) = NonZeroU32::new(self.main_target.color.0 as u32) {
                    self.gl.delete_texture(glow::NativeTexture(texture));
                }
            }
        } else {
            crate::core_warn!("[GlBackend] context unavailable during destruction");
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn neutral_resources_cover_texture_updates_and_offscreen_targets() {
        let Ok(mut backend) = GlBackend::new(platform::GfxBackend::Cgl, 4, 4) else {
            return;
        };
        backend.begin_access();

        let mut texture_desc = TextureDesc::sampled_rgba8(2, 2);
        texture_desc.usage |= TextureUsage::CPU_READABLE;
        let texture = backend
            .create_texture(
                "resource-api-test",
                texture_desc,
                TextureData::Rgba8(&[0; 16]),
            )
            .unwrap();
        assert_eq!(
            backend.textures.pixels_of("resource-api-test").unwrap().2,
            vec![0; 16]
        );
        backend
            .update_texture(
                texture,
                TextureUpdate {
                    origin: [0, 0],
                    extent: Extent2D::new(2, 2),
                    data: TextureData::Rgba8(&[255; 16]),
                },
            )
            .unwrap();
        assert_eq!(
            backend.textures.pixels_of("resource-api-test").unwrap().2,
            vec![255; 16]
        );
        assert!(backend.texture_descs.contains_key(&texture));
        backend.destroy_texture(texture);
        assert!(!backend.texture_descs.contains_key(&texture));

        let target = backend
            .create_render_target("offscreen-api-test", RenderTargetDesc::sampled_rgba8(2, 2))
            .unwrap();
        backend
            .begin_frame(FrameTarget::Offscreen(target.id))
            .unwrap();
        backend.clear([0.25, 0.5, 0.75, 1.0]);
        backend.end_frame();
        let mut pixels = [0; 16];
        assert_eq!(
            backend
                .readback(
                    FrameTarget::Offscreen(target.id),
                    Extent2D::new(2, 2),
                    &mut pixels,
                )
                .unwrap(),
            pixels.len()
        );
        assert!(
            pixels
                .chunks_exact(4)
                .all(|pixel| pixel == [64, 128, 191, 255])
        );
        backend.destroy_render_target(target.id);
        backend.end_access();
    }
}
