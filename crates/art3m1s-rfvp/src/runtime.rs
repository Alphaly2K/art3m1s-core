//! Runtime adapter between RFVP's captured frames and an Art3m1s GPU backend.

use std::collections::HashMap;
use std::fmt;

use art3m1s_render::{
    Extent2D, FrameTarget, GpuBackend, RenderRegion, TextureData, TextureDesc, TextureId,
    TextureInfo, TextureUpdate,
};
use rfvp::host_api::{TextureFormat, TextureHandle};
use rfvp::rendering::external::{ExternalFrame, RecordedTextureCreate};

use crate::{AdaptedFrame, AdapterError, DrawListAdapter, HitProxyTable};

#[derive(Debug)]
pub enum ExternalRendererError {
    Adapter(AdapterError),
    Backend(String),
    UnsupportedTextureFormat(TextureFormat),
    TextureSizeOverflow {
        width: u16,
        height: u16,
    },
    InvalidTextureData {
        handle: u32,
        format: TextureFormat,
        expected: usize,
        actual: usize,
    },
}

impl fmt::Display for ExternalRendererError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Adapter(error) => write!(f, "RFVP draw-list adaptation failed: {error}"),
            Self::Backend(error) => write!(f, "Art3m1s GPU backend failed: {error}"),
            Self::UnsupportedTextureFormat(format) => {
                write!(f, "RFVP texture format {format:?} is not supported")
            }
            Self::TextureSizeOverflow { width, height } => {
                write!(
                    f,
                    "RFVP texture size {width}x{height} overflows RGBA byte count"
                )
            }
            Self::InvalidTextureData {
                handle,
                format,
                expected,
                actual,
            } => write!(
                f,
                "RFVP texture handle {handle} has {actual} bytes for {format:?}, expected {expected}"
            ),
        }
    }
}

impl std::error::Error for ExternalRendererError {}

impl From<AdapterError> for ExternalRendererError {
    fn from(error: AdapterError) -> Self {
        Self::Adapter(error)
    }
}

#[derive(Debug, Clone, Copy)]
struct CachedTexture {
    texture: TextureId,
    info: TextureInfo,
    format: TextureFormat,
    generation: u64,
}

#[derive(Debug)]
pub struct RfvpRenderResult {
    pub hit_proxies: HitProxyTable,
    pub region: RenderRegion,
}

/// Owns RFVP textures and submits captured frames to an Art3m1s backend.
pub struct ExternalRenderer {
    backend: Box<dyn GpuBackend>,
    adapter: DrawListAdapter,
    textures: HashMap<TextureHandle, CachedTexture>,
    clear_color: [f32; 4],
}

impl ExternalRenderer {
    pub fn new(backend: Box<dyn GpuBackend>, clear_color: [f32; 4]) -> Self {
        Self {
            backend,
            adapter: DrawListAdapter::new(),
            textures: HashMap::new(),
            clear_color,
        }
    }

    pub fn backend(&self) -> &dyn GpuBackend {
        self.backend.as_ref()
    }

    pub fn backend_mut(&mut self) -> &mut dyn GpuBackend {
        self.backend.as_mut()
    }

    pub fn adapter(&self) -> &DrawListAdapter {
        &self.adapter
    }

    pub fn cached_texture_count(&self) -> usize {
        self.textures.len()
    }

    pub fn render_frame(
        &mut self,
        frame: &ExternalFrame,
    ) -> Result<RfvpRenderResult, ExternalRendererError> {
        self.sync_textures(&frame.textures)?;

        let adapted: AdaptedFrame = self.adapter.convert_rfvp_frame(&frame.frame)?;
        let hit_proxies = adapted.hit_proxies.clone();

        self.backend
            .begin_frame(FrameTarget::Main)
            .map_err(ExternalRendererError::Backend)?;
        self.backend.clear(self.clear_color);
        let region = self.backend.render(&adapted.draw_list);
        self.backend.end_frame();

        Ok(RfvpRenderResult {
            hit_proxies,
            region,
        })
    }

    pub fn readback_rgba(&mut self, extent: Extent2D) -> Result<Vec<u8>, ExternalRendererError> {
        self.backend
            .readback_owned(FrameTarget::Main, extent)
            .map_err(ExternalRendererError::Backend)
    }

    fn sync_textures(
        &mut self,
        textures: &[RecordedTextureCreate],
    ) -> Result<(), ExternalRendererError> {
        for upload in textures {
            let cached = self.textures.get(&upload.handle).copied();
            if cached.is_some_and(|cached| {
                cached.format == upload.desc.format
                    && cached.info.width == u32::from(upload.desc.width)
                    && cached.info.height == u32::from(upload.desc.height)
                    && cached.generation == upload.generation
            }) {
                continue;
            }

            let (width, height, rgba) = texture_rgba(upload)?;
            let same_layout = cached.is_some_and(|cached| {
                cached.format == upload.desc.format
                    && cached.info.width == width
                    && cached.info.height == height
            });

            let texture = if let Some(cached) = cached.filter(|_| same_layout) {
                self.backend
                    .update_texture(
                        cached.texture,
                        TextureUpdate {
                            origin: [0, 0],
                            extent: Extent2D::new(width, height),
                            data: TextureData::Rgba8(&rgba),
                        },
                    )
                    .map_err(ExternalRendererError::Backend)?;
                cached.texture
            } else {
                if let Some(cached) = cached {
                    self.backend.destroy_texture(cached.texture);
                }
                self.backend
                    .create_texture(
                        &format!("rfvp-texture-{}", upload.handle.0),
                        TextureDesc::sampled_rgba8(width, height),
                        TextureData::Rgba8(&rgba),
                    )
                    .map_err(ExternalRendererError::Backend)?
            };

            let info = TextureInfo { width, height };
            self.textures.insert(
                upload.handle,
                CachedTexture {
                    texture,
                    info,
                    format: upload.desc.format,
                    generation: upload.generation,
                },
            );
            self.adapter.bindings_mut().insert(
                crate::TextureHandle(upload.handle.0),
                texture,
                info,
            );
        }

        Ok(())
    }
}

fn texture_rgba(
    upload: &RecordedTextureCreate,
) -> Result<(u32, u32, Vec<u8>), ExternalRendererError> {
    let width = u32::from(upload.desc.width);
    let height = u32::from(upload.desc.height);

    match upload.desc.format {
        TextureFormat::Rgba8 => {
            let expected = rgba_len(width, height, upload.desc.width, upload.desc.height)?;
            if upload.pixels.len() != expected {
                return Err(ExternalRendererError::InvalidTextureData {
                    handle: upload.handle.0,
                    format: upload.desc.format,
                    expected,
                    actual: upload.pixels.len(),
                });
            }
            Ok((width, height, upload.pixels.clone()))
        }
        TextureFormat::LumaA8 => {
            let expected = usize::try_from(width)
                .ok()
                .and_then(|width| width.checked_mul(usize::try_from(height).ok()?))
                .and_then(|pixels| pixels.checked_mul(2))
                .ok_or(ExternalRendererError::TextureSizeOverflow {
                    width: upload.desc.width,
                    height: upload.desc.height,
                })?;
            if upload.pixels.len() != expected {
                return Err(ExternalRendererError::InvalidTextureData {
                    handle: upload.handle.0,
                    format: upload.desc.format,
                    expected,
                    actual: upload.pixels.len(),
                });
            }

            let mut rgba = Vec::with_capacity(expected.saturating_mul(2));
            for pixel in upload.pixels.chunks_exact(2) {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]);
            }
            Ok((width, height, rgba))
        }
        format => Err(ExternalRendererError::UnsupportedTextureFormat(format)),
    }
}

fn rgba_len(
    width: u32,
    height: u32,
    source_width: u16,
    source_height: u16,
) -> Result<usize, ExternalRendererError> {
    usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(usize::try_from(height).ok()?))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(ExternalRendererError::TextureSizeOverflow {
            width: source_width,
            height: source_height,
        })
}
