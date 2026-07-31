//! GL 纹理上传、素材解码与程序化占位纹理。
//!
//! [`GlTextureProvider`] 实现合成器的 [`TextureProvider`] trait：把逻辑资源名解析
//! 成 GL 纹理句柄。解析顺序为：缓存 → 可选的[素材字节源](GlTextureProvider::with_source)
//! （解码 PNG 等 → 上传）→ **程序化占位纹理**（棋盘格或纯色）兜底。
//!
//! 素材字节源是一个 `Fn(&str) -> Option<Vec<u8>>` 闭包，把"资源名→原始字节"的来源
//! 与解码/上传解耦：宿主可以接 [`crate::Project::read_file`]（解包后的项目目录），
//! 将来也可以接内存读 `.pfs` 的实现，provider 这边无需改动。样例项目暂无打包图片，
//! 因此默认无字节源、一律回退占位，让整条绘制管线无需素材即可端到端验证。

use crate::render_pipeline::draw::{TextureId, TextureInfo, TextureProvider};
use glow::HasContext;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::rc::Rc;

/// 资源名 → 原始字节的来源。返回 `None` 表示该资源不存在（将回退占位）。
pub type AssetSource = dyn Fn(&str) -> Option<Vec<u8>>;

/// 占位纹理的外观。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceholderKind {
    /// 品红/黑棋盘格——一眼可辨的"缺失素材"标记。
    Checker,
    /// 纯色块（RGBA）。
    Solid([u8; 4]),
}

enum PixelStorage {
    /// Every pixel is fully opaque, so no per-pixel allocation is needed.
    Opaque,
    Alpha(Vec<u8>),
    Rgba(Vec<u8>),
}

struct CpuTexturePixels {
    width: u32,
    height: u32,
    storage: PixelStorage,
}

impl CpuTexturePixels {
    fn alpha_only(width: u32, height: u32, rgba: &[u8]) -> Self {
        let mut alpha = Vec::with_capacity((width as usize).saturating_mul(height as usize));
        let mut opaque = true;
        for pixel in rgba.chunks_exact(4) {
            opaque &= pixel[3] == 255;
            alpha.push(pixel[3]);
        }
        Self {
            width,
            height,
            storage: if opaque {
                PixelStorage::Opaque
            } else {
                PixelStorage::Alpha(alpha)
            },
        }
    }

    fn readable_rgba(width: u32, height: u32, rgba: &[u8]) -> Self {
        Self {
            width,
            height,
            storage: PixelStorage::Rgba(rgba.to_vec()),
        }
    }

    fn alpha_at(&self, x: u32, y: u32) -> Option<u8> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let pixel = (y as usize)
            .checked_mul(self.width as usize)?
            .checked_add(x as usize)?;
        match &self.storage {
            PixelStorage::Opaque => Some(255),
            PixelStorage::Alpha(alpha) => alpha.get(pixel).copied(),
            PixelStorage::Rgba(rgba) => rgba.get(pixel.checked_mul(4)?.checked_add(3)?).copied(),
        }
    }

    fn rgba(&self) -> Option<(u32, u32, Vec<u8>)> {
        match &self.storage {
            PixelStorage::Rgba(rgba) => Some((self.width, self.height, rgba.clone())),
            PixelStorage::Opaque | PixelStorage::Alpha(_) => None,
        }
    }
}

/// 把资源名解析为 GL 纹理并缓存的提供者。
///
/// 与 [`GlRenderer`](super::GlRenderer) 共享同一个 [`glow::Context`]。
pub struct GlTextureProvider {
    gl: Rc<glow::Context>,
    /// 资源名 → (句柄, 尺寸)。
    cache: HashMap<String, (TextureId, TextureInfo)>,
    /// CPU pixels retained only when hit-testing or explicit readback needs them.
    cpu_pixels: HashMap<TextureId, CpuTexturePixels>,
    /// 可选的素材字节源（资源名 → 原始字节）。无则一律用占位。
    source: Option<Box<AssetSource>>,
    /// 缺失资源回退的占位外观与尺寸。
    placeholder: PlaceholderKind,
    placeholder_size: u32,
    /// Monotonic generation for texture pixel content visible to the renderer.
    content_revision: u64,
}

impl GlTextureProvider {
    pub fn new(gl: Rc<glow::Context>) -> Self {
        Self {
            gl,
            cache: HashMap::new(),
            cpu_pixels: HashMap::new(),
            source: None,
            placeholder: PlaceholderKind::Checker,
            placeholder_size: 256,
            content_revision: 0,
        }
    }

    /// Changes whenever an upload can alter pixels sampled by a draw command.
    pub fn content_revision(&self) -> u64 {
        self.content_revision
    }

    fn mark_content_changed(&mut self) {
        self.content_revision = self.content_revision.wrapping_add(1);
    }

    /// 设置素材字节源（资源名 → 原始图片字节）。
    ///
    /// 典型用法是接项目文件加载：
    /// `provider.with_source(move |name| project.read_file(name).ok())`。
    pub fn with_source<F>(mut self, source: F) -> Self
    where
        F: Fn(&str) -> Option<Vec<u8>> + 'static,
    {
        self.source = Some(Box::new(source));
        self
    }

    /// Use the FFI-registered file reader as the texture byte source.
    /// All texture loads are routed through the Flutter frontend.
    pub fn with_ffi_source(self) -> Self {
        self.with_source(|name: &str| -> Option<Vec<u8>> { crate::ffi::request_asset(name) })
    }

    /// 设置缺失资源的占位外观。
    pub fn with_placeholder(mut self, kind: PlaceholderKind, size: u32) -> Self {
        self.placeholder = kind;
        self.placeholder_size = size.max(2);
        self
    }

    /// 返回已解析纹理的逻辑尺寸，不触发加载。
    ///
    /// `get_layer_info` 在脚本查询图层尺寸时使用这份缓存；未显式设置
    /// width/height/clip 的图片层应报告素材本身的尺寸。
    pub fn cached_info(&self, name: &str) -> Option<TextureInfo> {
        self.cache.get(name).map(|(_, info)| *info)
    }

    /// 直接用一块 RGBA 像素登记一张命名纹理（测试或预置素材用）。
    /// 返回其句柄与尺寸；GL 纹理创建失败时返回 `None`。
    ///
    /// [`TextureProvider::upload_rgba`] 的 trait 实现直接转发到这里。
    pub fn upload_rgba(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        self.remove_if_cached(name);
        let entry = unsafe { self.try_create_texture(width, height, rgba) }?;
        self.cache.insert(name.to_string(), entry);
        self.cpu_pixels.insert(
            entry.0,
            CpuTexturePixels::readable_rgba(width, height, rgba),
        );
        self.mark_content_changed();
        Some(entry)
    }

    /// Upload pixels without retaining a second CPU-side RGBA allocation.
    pub fn upload_rgba_render_only(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        self.remove_if_cached(name);
        let entry = unsafe { self.try_create_texture(width, height, rgba) }?;
        self.cache.insert(name.to_string(), entry);
        self.mark_content_changed();
        Some(entry)
    }

    fn upload_dxt5_render_only(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        // Mobile GPUs are optimized for ASTC; accepting emulated S3TC there may
        // silently expand the texture. On Apple Silicon, prefer ASTC only when
        // the active backend exposes it, leaving the CGL path unchanged.
        if cfg!(any(target_os = "android", target_os = "ios"))
            || (cfg!(all(target_os = "macos", target_arch = "aarch64")) && self.supports_astc_4x4())
        {
            return None;
        }
        let extensions = self.gl.supported_extensions();
        let supported = [
            "GL_EXT_texture_compression_s3tc",
            "GL_EXT_texture_compression_dxt5",
            "GL_ANGLE_texture_compression_dxt5",
        ]
        .iter()
        .any(|extension| extensions.contains(*extension));
        if !supported {
            return None;
        }
        let expected = (width as usize)
            .checked_add(3)?
            .checked_div(4)?
            .checked_mul((height as usize).checked_add(3)?.checked_div(4)?)?
            .checked_mul(16)?;
        if width == 0 || height == 0 || data.len() != expected {
            return None;
        }

        self.remove_if_cached(name);
        let texture = unsafe { self.gl.create_texture().ok()? };
        unsafe {
            self.gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            self.gl.compressed_tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::COMPRESSED_RGBA_S3TC_DXT5_EXT as i32,
                width as i32,
                height as i32,
                0,
                data.len() as i32,
                data,
            );
            self.gl.bind_texture(glow::TEXTURE_2D, None);
            if self.gl.get_error() != glow::NO_ERROR {
                self.gl.delete_texture(texture);
                return None;
            }
        }

        let entry = (
            TextureId(texture.0.get() as u64),
            TextureInfo { width, height },
        );
        self.cache.insert(name.to_string(), entry);
        self.mark_content_changed();
        Some(entry)
    }

    fn supports_astc_4x4(&self) -> bool {
        self.gl
            .supported_extensions()
            .contains("GL_KHR_texture_compression_astc_ldr")
    }

    fn upload_astc_4x4_render_only(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        if !self.supports_astc_4x4() {
            return None;
        }
        let expected = (width as usize)
            .checked_add(3)?
            .checked_div(4)?
            .checked_mul((height as usize).checked_add(3)?.checked_div(4)?)?
            .checked_mul(16)?;
        if width == 0 || height == 0 || data.len() != expected {
            return None;
        }

        self.remove_if_cached(name);
        let texture = unsafe { self.gl.create_texture().ok()? };
        unsafe {
            self.gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            self.gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            self.gl.compressed_tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::COMPRESSED_RGBA_ASTC_4x4_KHR as i32,
                width as i32,
                height as i32,
                0,
                data.len() as i32,
                data,
            );
            self.gl.bind_texture(glow::TEXTURE_2D, None);
            if self.gl.get_error() != glow::NO_ERROR {
                self.gl.delete_texture(texture);
                return None;
            }
        }

        let entry = (
            TextureId(texture.0.get() as u64),
            TextureInfo { width, height },
        );
        self.cache.insert(name.to_string(), entry);
        self.mark_content_changed();
        Some(entry)
    }

    /// Upload one host-decoded RGBA video frame without retaining a CPU copy.
    /// Reuses the GL texture while the frame dimensions stay unchanged.
    pub fn upload_video_rgba(&mut self, name: &str, width: u32, height: u32, rgba: &[u8]) -> bool {
        let Some(expected_len) = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
        else {
            return false;
        };
        if width == 0 || height == 0 || rgba.len() < expected_len {
            return false;
        }
        let rgba = &rgba[..expected_len];

        if let Some((texture, info)) = self.cache.get(name).copied()
            && info.width == width
            && info.height == height
            && let Some(raw) = NonZeroU32::new(texture.0 as u32)
        {
            unsafe {
                self.gl
                    .bind_texture(glow::TEXTURE_2D, Some(glow::NativeTexture(raw)));
                self.gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    0,
                    0,
                    width as i32,
                    height as i32,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(rgba)),
                );
                self.gl.bind_texture(glow::TEXTURE_2D, None);
            }
            self.cpu_pixels.remove(&texture);
            self.mark_content_changed();
            return true;
        }

        self.remove_if_cached(name);
        let Some(entry) = (unsafe { self.try_create_texture(width, height, rgba) }) else {
            return false;
        };
        // mpv's software renderer supplies `rgb0`: byte four is padding, not
        // alpha. Limit this swizzle to video textures so ordinary RGBA assets
        // keep their authored alpha channel.
        if let Some(raw) = NonZeroU32::new(entry.0.0 as u32) {
            unsafe {
                self.gl
                    .bind_texture(glow::TEXTURE_2D, Some(glow::NativeTexture(raw)));
                self.gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_SWIZZLE_A,
                    glow::ONE as i32,
                );
                self.gl.bind_texture(glow::TEXTURE_2D, None);
            }
        }
        self.cache.insert(name.to_string(), entry);
        self.cpu_pixels.remove(&entry.0);
        self.mark_content_changed();
        true
    }

    fn remove_if_cached(&mut self, name: &str) {
        if let Some((id, _)) = self.cache.remove(name) {
            self.cpu_pixels.remove(&id);
            if let Some(nz) = NonZeroU32::new(id.0 as u32) {
                unsafe {
                    self.gl.delete_texture(glow::NativeTexture(nz));
                }
            }
        }
    }

    /// 在 GL 上创建一张 RGBA8 纹理并上传像素。创建失败（如上下文丢失）时
    /// 记日志并返回 `None`，不 panic——本函数在生产渲染路径上。
    ///
    /// # Safety
    /// 需在当前 GL 上下文下调用。
    unsafe fn try_create_texture(
        &self,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        let gl = &self.gl;
        unsafe {
            let tex = match gl.create_texture() {
                Ok(tex) => tex,
                Err(e) => {
                    crate::core_warn!("create_texture 失败 ({width}x{height}): {e}");
                    return None;
                }
            };
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
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
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA as i32,
                width as i32,
                height as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(rgba)),
            );
            gl.bind_texture(glow::TEXTURE_2D, None);

            // glow 的 NativeTexture 内部是 NonZeroU32；取出原始 id 存进句柄。
            let raw = tex.0.get();
            Some((TextureId(raw as u64), TextureInfo { width, height }))
        }
    }

    /// 生成占位纹理的 RGBA 像素。
    fn placeholder_pixels(&self) -> (u32, Vec<u8>) {
        let size = self.placeholder_size;
        let mut buf = vec![0u8; (size * size * 4) as usize];
        match self.placeholder {
            PlaceholderKind::Solid(color) => {
                for px in buf.chunks_exact_mut(4) {
                    px.copy_from_slice(&color);
                }
            }
            PlaceholderKind::Checker => {
                let cell = (size / 8).max(1);
                for y in 0..size {
                    for x in 0..size {
                        let on = ((x / cell) + (y / cell)).is_multiple_of(2);
                        let idx = ((y * size + x) * 4) as usize;
                        let color: [u8; 4] = if on {
                            [255, 0, 255, 255] // 品红
                        } else {
                            [0, 0, 0, 255] // 黑
                        };
                        buf[idx..idx + 4].copy_from_slice(&color);
                    }
                }
            }
        }
        (size, buf)
    }
}

impl Drop for GlTextureProvider {
    fn drop(&mut self) {
        for (id, _) in self.cache.drain().map(|(_, entry)| entry) {
            if let Some(raw) = NonZeroU32::new(id.0 as u32) {
                unsafe {
                    self.gl.delete_texture(glow::NativeTexture(raw));
                }
            }
        }
        self.cpu_pixels.clear();
    }
}

impl TextureProvider for GlTextureProvider {
    fn resolve(&mut self, name: &str) -> Option<(TextureId, TextureInfo)> {
        if let Some(entry) = self.cache.get(name) {
            return Some(*entry);
        }

        // Dynamic video textures do not exist until the host uploads frame 0.
        if crate::video::is_video_layer_texture_name(name) {
            return None;
        }

        // 1) 有字节源且能取到字节并解码成功 → 上传真实纹理。
        if let Some(source) = &self.source {
            match source(name) {
                Some(bytes) => match decode_rgba(&bytes) {
                    Some((w, h, rgba)) => {
                        let entry = unsafe { self.try_create_texture(w, h, &rgba) }?;
                        self.cache.insert(name.to_string(), entry);
                        self.cpu_pixels
                            .insert(entry.0, CpuTexturePixels::alpha_only(w, h, &rgba));
                        self.mark_content_changed();
                        return Some(entry);
                    }
                    // 只在首次失败时到达（结果按名缓存），不会刷屏。
                    None => {
                        crate::core_warn!("纹理解码失败，回退占位: {name}");
                    }
                },
                None => {
                    crate::core_warn!("素材不存在，回退占位: {name}");
                }
            }
        }

        // 2) 取不到或解码失败 → 回退占位纹理（按名缓存，保证句柄稳定）。
        let (size, pixels) = self.placeholder_pixels();
        let entry = unsafe { self.try_create_texture(size, size, &pixels) }?;
        self.cache.insert(name.to_string(), entry);
        self.cpu_pixels
            .insert(entry.0, CpuTexturePixels::alpha_only(size, size, &pixels));
        self.mark_content_changed();
        Some(entry)
    }

    fn upload_rgba(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        GlTextureProvider::upload_rgba(self, name, width, height, data)
    }

    fn upload_rgba_render_only(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        GlTextureProvider::upload_rgba_render_only(self, name, width, height, data)
    }

    fn upload_dxt5_render_only(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        GlTextureProvider::upload_dxt5_render_only(self, name, width, height, data)
    }

    fn supports_astc_4x4(&self) -> bool {
        GlTextureProvider::supports_astc_4x4(self)
    }

    fn upload_astc_4x4_render_only(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        GlTextureProvider::upload_astc_4x4_render_only(self, name, width, height, data)
    }

    fn pixel_alpha(&self, texture: TextureId, x: u32, y: u32) -> Option<u8> {
        self.cpu_pixels.get(&texture)?.alpha_at(x, y)
    }

    /// 1x1 纯色纹理（`lyc` 单色图层）：按稳定名缓存，避免每帧重建。
    fn solid_texture(&mut self, rgba: [u8; 4]) -> Option<(TextureId, TextureInfo)> {
        let name = crate::render_pipeline::draw::solid_texture_name(rgba);
        if let Some(entry) = self.cache.get(&name) {
            return Some(*entry);
        }
        GlTextureProvider::upload_rgba(self, &name, 1, 1, &rgba)
    }

    /// `lyc` file+mask 双图合成：out.rgb = file.rgb，out.a = file.a × mask 灰度。
    ///
    /// 结果按组合名缓存。任一图取不到 / 解码失败 / 尺寸不一致（文档要求同尺寸）
    /// 时退化为普通 resolve(file)。
    fn resolve_with_mask(&mut self, file: &str, mask: &str) -> Option<(TextureId, TextureInfo)> {
        let name = crate::render_pipeline::draw::masked_texture_name(file, mask);
        if let Some(entry) = self.cache.get(&name) {
            return Some(*entry);
        }

        let combined = self.source.as_ref().and_then(|source| {
            let file_bytes = source(file)?;
            let mask_bytes = source(mask)?;
            let (fw, fh, mut fpx) = decode_rgba(&file_bytes)?;
            let (mw, mh, mpx) = decode_rgba(&mask_bytes)?;
            if (fw, fh) != (mw, mh) {
                crate::core_warn!(
                    "[lyc] mask 尺寸不一致: {file}({fw}x{fh}) vs {mask}({mw}x{mh})，忽略蒙版"
                );
                return None;
            }
            // 灰度蒙版：白=不透明。取 R 通道（灰度图 R=G=B）乘进 file 的 alpha。
            for (dst, m) in fpx.chunks_exact_mut(4).zip(mpx.chunks_exact(4)) {
                dst[3] = ((dst[3] as u16 * m[0] as u16) / 255) as u8;
            }
            Some((fw, fh, fpx))
        });

        match combined {
            Some((w, h, pixels)) => {
                GlTextureProvider::upload_rgba_render_only(self, &name, w, h, &pixels)
            }
            None => self.resolve(file),
        }
    }

    /// 读取逻辑资源的 CPU 侧像素副本（`lyedit` 用）。
    fn pixels_of(&mut self, name: &str) -> Option<(u32, u32, Vec<u8>)> {
        if let Some((texture, _)) = self.cache.get(name)
            && let Some(rgba) = self
                .cpu_pixels
                .get(texture)
                .and_then(CpuTexturePixels::rgba)
        {
            return Some(rgba);
        }
        if let Some(source) = &self.source
            && let Some(decoded) = source(name).as_deref().and_then(decode_rgba)
        {
            return Some(decoded);
        }
        let (texture, _) = self.resolve(name)?;
        self.cpu_pixels
            .get(&texture)
            .and_then(CpuTexturePixels::rgba)
    }

    fn retain(&mut self, names: &std::collections::HashSet<String>) {
        let stale: Vec<String> = self
            .cache
            .keys()
            .filter(|k| !names.contains(*k))
            .cloned()
            .collect();
        for name in &stale {
            if let Some((id, _)) = self.cache.remove(name) {
                self.cpu_pixels.remove(&id);
                if let Some(nz) = NonZeroU32::new(id.0 as u32) {
                    unsafe {
                        self.gl.delete_texture(glow::NativeTexture(nz));
                    }
                }
            }
        }
    }
}

/// 把图片字节解码成 `(宽, 高, RGBA8)`。无法识别/解码失败返回 `None`。
fn decode_rgba(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some((w, h, rgba.into_raw()))
}

#[cfg(test)]
mod tests {
    use super::{CpuTexturePixels, PixelStorage};

    #[test]
    fn opaque_texture_needs_no_per_pixel_cpu_storage() {
        let pixels = CpuTexturePixels::alpha_only(2, 1, &[1, 2, 3, 255, 4, 5, 6, 255]);
        assert!(matches!(pixels.storage, PixelStorage::Opaque));
        assert_eq!(pixels.alpha_at(1, 0), Some(255));
    }

    #[test]
    fn translucent_texture_retains_only_one_alpha_byte_per_pixel() {
        let pixels = CpuTexturePixels::alpha_only(2, 1, &[1, 2, 3, 64, 4, 5, 6, 128]);
        let PixelStorage::Alpha(alpha) = &pixels.storage else {
            panic!("expected alpha-only storage");
        };
        assert_eq!(alpha, &[64, 128]);
        assert_eq!(pixels.alpha_at(0, 0), Some(64));
        assert_eq!(pixels.alpha_at(2, 0), None);
    }
}
