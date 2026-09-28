//! Same-thread KRKR window-texture adapter for `art3m1s-render`.

use std::collections::HashMap;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};

use art3m1s_krkr::protocol::{
    ART3M1S_KRKR_STATUS_ENGINE, ART3M1S_KRKR_STATUS_INVALID_ARGUMENT, ART3M1S_KRKR_STATUS_OK,
};
use art3m1s_krkr::render_host::Art3m1sKrkrRenderHostV1;
use art3m1s_render::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawList, Extent2D, FrameTarget, GpuBackend,
    NativeSurface, RenderRegion, TextureData, TextureDesc, TextureId, TextureInfo, TextureUpdate,
};
use glam::{Affine2, Vec2};

#[derive(Clone, Copy)]
struct WindowTexture {
    texture: TextureId,
    info: TextureInfo,
}

pub(super) struct KrkrRenderer {
    backend: Box<dyn GpuBackend>,
    textures: HashMap<u64, WindowTexture>,
    next_texture: u64,
    draw_list: DrawList,
    extent: Extent2D,
    generation: u64,
    frame_in_progress: bool,
    surface_attached: bool,
    last_region: RenderRegion,
}

// The public KRKR handle table serializes access, while callbacks additionally
// take this renderer's mutex. The backend never escapes either boundary.
unsafe impl Send for KrkrRenderer {}

impl KrkrRenderer {
    pub(super) fn new(backend: Box<dyn GpuBackend>, width: u32, height: u32) -> Self {
        Self {
            backend,
            textures: HashMap::new(),
            next_texture: 1,
            draw_list: DrawList::new(),
            extent: Extent2D::new(width, height),
            generation: 0,
            frame_in_progress: false,
            surface_attached: false,
            last_region: RenderRegion::Full,
        }
    }

    pub(super) fn host_v1(renderer: &Arc<Mutex<Self>>) -> Art3m1sKrkrRenderHostV1 {
        let mut host =
            Art3m1sKrkrRenderHostV1::new(Arc::as_ptr(renderer).cast_mut().cast::<c_void>());
        host.begin_frame = Some(begin_frame);
        host.create_texture = Some(create_texture);
        host.update_texture = Some(update_texture);
        host.destroy_texture = Some(destroy_texture);
        host.draw_texture = Some(draw_texture);
        host.end_frame = Some(end_frame);
        host
    }

    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    pub(super) fn frame_in_progress(&self) -> bool {
        self.frame_in_progress
    }

    pub(super) fn extent(&self) -> Extent2D {
        self.extent
    }

    pub(super) fn readback_rgba(&mut self) -> Result<Vec<u8>, String> {
        self.backend.readback_owned(FrameTarget::Main, self.extent)
    }

    pub(super) fn set_native_surface(
        &mut self,
        kind: i32,
        handle: *mut c_void,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        let surface = NativeSurface::from_legacy_parts(kind, handle, width, height)?;
        self.backend.set_native_surface(surface)?;
        self.surface_attached = true;
        Ok(())
    }

    pub(super) fn clear_native_surface(&mut self) {
        self.backend.clear_native_surface();
        self.surface_attached = false;
    }

    fn begin_frame(&mut self, width: u32, height: u32) -> Result<(), String> {
        let extent = Extent2D::new(width, height);
        if extent.is_empty() {
            return Err("KRKR frame extent is empty".into());
        }
        if extent != self.extent {
            self.backend.resize(extent)?;
            self.extent = extent;
        }
        self.draw_list = DrawList::new();
        self.backend.begin_frame(FrameTarget::Main)?;
        self.frame_in_progress = true;
        self.backend.clear([0.0, 0.0, 0.0, 0.0]);
        Ok(())
    }

    fn create_texture(&mut self, width: u32, height: u32) -> Result<u64, String> {
        let info = TextureInfo { width, height };
        if width == 0 || height == 0 {
            return Err("KRKR window texture extent is empty".into());
        }
        let handle = self.next_texture;
        self.next_texture = self.next_texture.checked_add(1).unwrap_or(1);
        let texture = self.backend.create_texture(
            &format!("krkr-window-{handle}"),
            TextureDesc::sampled_rgba8(width, height),
            TextureData::Uninitialized,
        )?;
        self.textures
            .insert(handle, WindowTexture { texture, info });
        Ok(handle)
    }

    fn update_texture(
        &mut self,
        handle: u64,
        pixels: &[u8],
        width: u32,
        height: u32,
        pitch: u32,
    ) -> Result<(), String> {
        let cached = self
            .textures
            .get(&handle)
            .copied()
            .ok_or_else(|| format!("unknown KRKR window texture {handle}"))?;
        if cached.info.width != width || cached.info.height != height {
            return Err(format!(
                "KRKR texture {handle} changed from {}x{} to {width}x{height} without recreation",
                cached.info.width, cached.info.height
            ));
        }
        let row_bytes = (width as usize)
            .checked_mul(4)
            .ok_or_else(|| "KRKR texture row size overflow".to_owned())?;
        let pitch = pitch as usize;
        if pitch < row_bytes {
            return Err("KRKR texture pitch is smaller than one RGBA row".into());
        }
        let required = pitch
            .checked_mul(height as usize)
            .ok_or_else(|| "KRKR texture byte size overflow".to_owned())?;
        if pixels.len() < required {
            return Err("KRKR texture payload is truncated".into());
        }

        let packed;
        let data = if pitch == row_bytes {
            &pixels[..row_bytes * height as usize]
        } else {
            packed = pixels
                .chunks_exact(pitch)
                .take(height as usize)
                .flat_map(|row| row[..row_bytes].iter().copied())
                .collect::<Vec<_>>();
            packed.as_slice()
        };
        self.backend.update_texture(
            cached.texture,
            TextureUpdate {
                origin: [0, 0],
                extent: Extent2D::new(width, height),
                data: TextureData::Rgba8(data),
            },
        )
    }

    fn destroy_texture(&mut self, handle: u64) {
        if let Some(texture) = self.textures.remove(&handle) {
            self.backend.destroy_texture(texture.texture);
        }
    }

    fn draw_texture(
        &mut self,
        handle: u64,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) -> Result<(), String> {
        let cached = self
            .textures
            .get(&handle)
            .copied()
            .ok_or_else(|| format!("unknown KRKR window texture {handle}"))?;
        if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
            return Err("KRKR draw extent is invalid".into());
        }
        let scale = Vec2::new(
            width / cached.info.width as f32,
            height / cached.info.height as f32,
        );
        self.draw_list.push(DrawCommand {
            texture: cached.texture,
            size: cached.info,
            transform: Affine2::from_scale_angle_translation(scale, 0.0, Vec2::new(x, y)),
            opacity: 1.0,
            blend: BlendMode::Alpha,
            color: ColorFilter::default(),
            clip: ClipRect::full(cached.info),
            clip_bounds: None,
            shader: None,
            mesh: None,
            stencil: None,
            native_emote: None,
        });
        Ok(())
    }

    fn end_frame(&mut self) -> Result<(), String> {
        self.last_region = self.backend.render(&self.draw_list);
        self.backend.end_frame();
        self.frame_in_progress = false;
        self.generation = self.generation.wrapping_add(1).max(1);
        if self.surface_attached {
            self.backend.present(self.last_region.damage())?;
        }
        Ok(())
    }
}

fn renderer(user_data: *mut c_void) -> Result<&'static Mutex<KrkrRenderer>, i32> {
    if user_data.is_null() {
        return Err(ART3M1S_KRKR_STATUS_INVALID_ARGUMENT);
    }
    Ok(unsafe { &*user_data.cast::<Mutex<KrkrRenderer>>() })
}

fn with_renderer(
    user_data: *mut c_void,
    callback: impl FnOnce(&mut KrkrRenderer) -> Result<(), String>,
) -> i32 {
    catch_unwind(AssertUnwindSafe(|| {
        let renderer = renderer(user_data)?;
        let mut renderer = renderer.lock().map_err(|_| ART3M1S_KRKR_STATUS_ENGINE)?;
        callback(&mut renderer).map_err(|error| {
            eprintln!("[KRKR] render host callback failed: {error}");
            ART3M1S_KRKR_STATUS_ENGINE
        })?;
        Ok(ART3M1S_KRKR_STATUS_OK)
    }))
    .unwrap_or(Err(ART3M1S_KRKR_STATUS_ENGINE))
    .unwrap_or_else(|status| status)
}

unsafe extern "C" fn begin_frame(user_data: *mut c_void, width: u32, height: u32) -> i32 {
    with_renderer(user_data, |renderer| renderer.begin_frame(width, height))
}

unsafe extern "C" fn create_texture(user_data: *mut c_void, width: u32, height: u32) -> u64 {
    catch_unwind(AssertUnwindSafe(|| {
        let renderer = renderer(user_data).ok()?;
        let mut renderer = renderer.lock().ok()?;
        renderer.create_texture(width, height).ok()
    }))
    .ok()
    .flatten()
    .unwrap_or(0)
}

unsafe extern "C" fn update_texture(
    user_data: *mut c_void,
    texture: u64,
    pixels: *const u8,
    width: u32,
    height: u32,
    pitch: u32,
) -> i32 {
    if pixels.is_null() {
        return ART3M1S_KRKR_STATUS_INVALID_ARGUMENT;
    }
    let Some(length) = (pitch as usize).checked_mul(height as usize) else {
        return ART3M1S_KRKR_STATUS_INVALID_ARGUMENT;
    };
    let pixels = unsafe { std::slice::from_raw_parts(pixels, length) };
    with_renderer(user_data, |renderer| {
        renderer.update_texture(texture, pixels, width, height, pitch)
    })
}

unsafe extern "C" fn destroy_texture(user_data: *mut c_void, texture: u64) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Ok(renderer) = renderer(user_data)
            && let Ok(mut renderer) = renderer.lock()
        {
            renderer.destroy_texture(texture);
        }
    }));
}

unsafe extern "C" fn draw_texture(
    user_data: *mut c_void,
    texture: u64,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> i32 {
    with_renderer(user_data, |renderer| {
        renderer.draw_texture(texture, x, y, width, height)
    })
}

unsafe extern "C" fn end_frame(user_data: *mut c_void) -> i32 {
    with_renderer(user_data, KrkrRenderer::end_frame)
}

#[cfg(test)]
mod tests {
    #[test]
    fn padded_rows_pack_without_leaking_padding() {
        let pixels = [1, 2, 3, 4, 99, 99, 5, 6, 7, 8, 99, 99];
        let packed = pixels
            .chunks_exact(6)
            .take(2)
            .flat_map(|row| row[..4].iter().copied())
            .collect::<Vec<_>>();
        assert_eq!(packed, [1, 2, 3, 4, 5, 6, 7, 8]);
    }
}
