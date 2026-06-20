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

use crate::compositor::renderer::{TextureId, TextureInfo, TextureProvider};
use glow::HasContext;
use std::collections::HashMap;
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

/// 把资源名解析为 GL 纹理并缓存的提供者。
///
/// 与 [`GlRenderer`](super::GlRenderer) 共享同一个 [`glow::Context`]。
pub struct GlTextureProvider {
    gl: Rc<glow::Context>,
    /// 资源名 → (句柄, 尺寸)。
    cache: HashMap<String, (TextureId, TextureInfo)>,
    /// 纹理句柄 → CPU 侧 RGBA 缓存（用于 hit-test 像素采样）。
    pixels: HashMap<TextureId, (u32, u32, Vec<u8>)>,
    /// 可选的素材字节源（资源名 → 原始字节）。无则一律用占位。
    source: Option<Box<AssetSource>>,
    /// 缺失资源回退的占位外观与尺寸。
    placeholder: PlaceholderKind,
    placeholder_size: u32,
}

impl GlTextureProvider {
    pub fn new(gl: Rc<glow::Context>) -> Self {
        Self {
            gl,
            cache: HashMap::new(),
            pixels: HashMap::new(),
            source: None,
            placeholder: PlaceholderKind::Checker,
            placeholder_size: 256,
        }
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
        self.with_source(|name: &str| -> Option<Vec<u8>> {
            crate::ffi::request_asset(name)
        })
    }

    /// 设置缺失资源的占位外观。
    pub fn with_placeholder(mut self, kind: PlaceholderKind, size: u32) -> Self {
        self.placeholder = kind;
        self.placeholder_size = size.max(2);
        self
    }

    /// 直接用一块 RGBA 像素登记一张命名纹理（测试或预置素材用）。
    /// 返回其句柄与尺寸。
    pub fn upload_rgba(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> (TextureId, TextureInfo) {
        let entry = unsafe { self.create_texture(width, height, rgba) };
        self.cache.insert(name.to_string(), entry);
        self.pixels
            .insert(entry.0, (width, height, rgba.to_vec()));
        entry
    }

    /// 在 GL 上创建一张 RGBA8 纹理并上传像素。
    ///
    /// # Safety
    /// 需在当前 GL 上下文下调用。
    unsafe fn create_texture(
        &self,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> (TextureId, TextureInfo) {
        let gl = &self.gl;
        unsafe {
            let tex = gl.create_texture().expect("create_texture");
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
            (
                TextureId(raw as u64),
                TextureInfo { width, height },
            )
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

impl TextureProvider for GlTextureProvider {
    fn resolve(&mut self, name: &str) -> Option<(TextureId, TextureInfo)> {
        if let Some(entry) = self.cache.get(name) {
            return Some(*entry);
        }

        // 1) 有字节源且能取到字节并解码成功 → 上传真实纹理。
        if let Some(source) = &self.source
            && let Some(bytes) = source(name)
            && let Some((w, h, rgba)) = decode_rgba(&bytes)
        {
            let entry = unsafe { self.create_texture(w, h, &rgba) };
            self.cache.insert(name.to_string(), entry);
            self.pixels.insert(entry.0, (w, h, rgba));
            return Some(entry);
        }

        // 2) 取不到或解码失败 → 回退占位纹理（按名缓存，保证句柄稳定）。
        let (size, pixels) = self.placeholder_pixels();
        let entry = unsafe { self.create_texture(size, size, &pixels) };
        self.cache.insert(name.to_string(), entry);
        self.pixels.insert(entry.0, (size, size, pixels));
        Some(entry)
    }

    fn upload_rgba(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Option<(TextureId, TextureInfo)> {
        let entry = unsafe { self.create_texture(width, height, data) };
        self.cache.insert(name.to_string(), entry);
        self.pixels
            .insert(entry.0, (width, height, data.to_vec()));
        Some(entry)
    }

    fn pixel_alpha(&self, texture: TextureId, x: u32, y: u32) -> Option<u8> {
        let (w, h, rgba) = self.pixels.get(&texture)?;
        if x >= *w || y >= *h {
            return None;
        }
        let idx = ((y * *w + x) * 4 + 3) as usize; // +3 = alpha channel
        rgba.get(idx).copied()
    }
}

/// 把图片字节解码成 `(宽, 高, RGBA8)`。无法识别/解码失败返回 `None`。
fn decode_rgba(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some((w, h, rgba.into_raw()))
}
