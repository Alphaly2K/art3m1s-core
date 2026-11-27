//! Siglus session and optional drawing support for Art3m1s hosts.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub use siglus_scene_vm;

use anyhow::{Context, Result, bail};
use art3m1s_render::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawList, DrawMesh, FrameTarget, GpuBackend,
    TextureData, TextureDesc, TextureId, TextureInfo,
};
use siglus_scene_vm::assets::RgbaImage;
pub use siglus_scene_vm::host::{SiglusBootSource, SiglusGameexe, SiglusSaveStore};
use siglus_scene_vm::host::{SiglusHost, SiglusHostConfig};

mod emote;
pub use emote::{SiglusEmoteRenderer, SiglusEmoteTexture};
mod project_io;
pub use project_io::SiglusProjectIoV1;
mod save_directory;
pub use save_directory::DirectorySaves;
use siglus_scene_vm::image_manager::{ImageKey, ImageManager};
use siglus_scene_vm::layer::{
    RenderFrame, RenderSprite, Sprite, SpriteBlend, SpriteFit, SpriteSizeMode,
};
use siglus_scene_vm::platform_time::with_host_time;
use siglus_scene_vm::render_math::sprite_quad_geometry_rect;
pub use siglus_scene_vm::resource::SiglusResourceSource;
use siglus_scene_vm::resource::with_project_files;
use siglus_scene_vm::runtime::input::VmMouseButton;

/// Probe only; constructing the host performs full game initialization.
pub fn is_siglus_project(path: &Path) -> bool {
    project_io::probe_directory(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ColorEffects {
    mono: u8,
    reverse: u8,
    bright: u8,
    dark: u8,
    color_rate: u8,
    color_add: [u8; 3],
    color_target: [u8; 3],
}

impl ColorEffects {
    fn from_sprite(s: &Sprite) -> Self {
        Self {
            mono: s.mono,
            reverse: s.reverse,
            bright: s.bright,
            dark: s.dark,
            color_rate: s.color_rate,
            color_add: [s.color_add_r, s.color_add_g, s.color_add_b],
            color_target: [s.color_r, s.color_g, s.color_b],
        }
    }

    fn is_identity(self) -> bool {
        self.mono == 0
            && self.reverse == 0
            && self.bright == 0
            && self.dark == 0
            && self.color_rate == 0
            && self.color_add == [0; 3]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum TextureKey {
    Image(ImageKey, ColorEffects),
    Solid(ColorEffects),
}

pub struct SiglusAdapter {
    host: SiglusHost,
    elapsed: Duration,
    files: Option<(PathBuf, Arc<dyn SiglusResourceSource>)>,
}

/// A frame and its image lookup, valid until the session is mutated again.
pub struct SiglusFrame<'session> {
    pub size: (u32, u32),
    pub plan: RenderFrame,
    pub images: &'session ImageManager,
}

pub struct SiglusStep<'session> {
    pub exiting: bool,
    pub frame: Option<SiglusFrame<'session>>,
}

impl SiglusAdapter {
    pub fn open(config: SiglusHostConfig) -> Result<Self> {
        Self::open_directory(config, false)
    }

    pub fn open_with_boot_source(
        config: SiglusHostConfig,
        source: Arc<dyn SiglusBootSource>,
    ) -> Result<Self> {
        Self::open_with(None, || {
            SiglusHost::new_external_with_boot_source(config, source)
        })
    }

    /// Mount logical project files for named image and script resource lookups.
    pub fn open_with_project_files(
        config: SiglusHostConfig,
        boot: Arc<dyn SiglusBootSource>,
        files: Arc<dyn SiglusResourceSource>,
    ) -> Result<Self> {
        Self::open_with(Some((config.project_dir.clone(), files)), || {
            SiglusHost::new_external_with_boot_source(config, boot)
        })
    }

    fn open_with(
        files: Option<(PathBuf, Arc<dyn SiglusResourceSource>)>,
        build_host: impl FnOnce() -> Result<SiglusHost>,
    ) -> Result<Self> {
        #[cfg(feature = "uci-ffmpeg")]
        siglus_scene_vm::assets::install_in_process_decoder(decode_uci_h264);
        Ok(Self {
            host: with_project_files(files.clone(), || with_host_time(Duration::ZERO, build_host))?,
            elapsed: Duration::ZERO,
            files,
        })
    }

    /// Run an input or service call at the session's current time.
    pub fn with_host<T>(&mut self, run: impl FnOnce(&mut SiglusHost) -> T) -> T {
        with_project_files(self.files.clone(), || {
            with_host_time(self.elapsed, || run(&mut self.host))
        })
    }

    pub fn logical_size(&self) -> (u32, u32) {
        self.host.logical_size()
    }

    /// Host input coordinates are expressed in logical stage pixels.
    pub fn pointer_move(&mut self, x: i32, y: i32) {
        self.with_host(|host| host.mouse_move(x as f64, y as f64));
    }

    pub fn pointer_button(&mut self, button: i32, pressed: bool) {
        let button = match button {
            2 => VmMouseButton::Right,
            3 => VmMouseButton::Middle,
            _ => VmMouseButton::Left,
        };
        self.with_host(|host| {
            if pressed {
                host.mouse_down(button);
            } else {
                host.mouse_up(button);
            }
        });
    }

    pub fn touch(&mut self, phase: i32, x: i32, y: i32) {
        self.with_host(|host| host.touch(phase, x as f64, y as f64));
    }

    pub fn wheel(&mut self, delta_y: i32) {
        self.with_host(|host| host.mouse_wheel(delta_y));
    }

    pub fn key(&mut self, code: i32, pressed: bool) {
        self.with_host(|host| {
            if pressed {
                host.key_down_code(code);
            } else {
                host.key_up_code(code);
            }
        });
    }

    /// Fill interleaved stereo f32 samples at 48 kHz. The Host owns the
    /// output device and determines how many frames to request.
    #[cfg(feature = "host-audio")]
    pub fn render_audio(&mut self, output: &mut [f32]) {
        self.with_host(|host| host.render_host_audio(output));
    }

    /// Advance the VM by Host time. No frame means the stage did not redraw.
    pub fn step(&mut self, dt_ms: u32) -> Result<SiglusStep<'_>> {
        self.elapsed += Duration::from_millis(u64::from(dt_ms));
        let exiting = self.with_host(|host| host.step(dt_ms))?;
        let frame = self.host.take_external_frame().map(|plan| SiglusFrame {
            size: self.host.logical_size(),
            plan,
            images: self.host.external_images(),
        });
        Ok(SiglusStep { exiting, frame })
    }
}

/// An owned frame with logical texture IDs and no device resources.
/// It retains the exact CPU images and object packets used by its commands.
pub struct SiglusFrameSnapshot {
    size: (u32, u32),
    draw: DrawList,
    textures: Vec<SiglusFrameTexture>,
    emote_textures: Vec<SiglusEmoteTexture>,
    sprite_count: usize,
}

impl SiglusFrameSnapshot {
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn draw_list(&self) -> &DrawList {
        &self.draw
    }

    /// CPU textures already available for upload. Object textures are listed separately.
    pub fn textures(&self) -> &[SiglusFrameTexture] {
        &self.textures
    }

    pub fn emote_textures(&self) -> &[SiglusEmoteTexture] {
        &self.emote_textures
    }
}

#[derive(Clone)]
pub struct SiglusFrameTexture {
    pub id: TextureId,
    pub image: Arc<RgbaImage>,
}

/// Converts the current VM frame into a replayable Host frame.
#[derive(Default)]
pub struct SiglusFrameCompiler {
    textures: HashMap<TextureKey, (u64, SiglusFrameTexture)>,
    last_texture_id: u64,
}

impl SiglusFrameCompiler {
    pub fn compile(&mut self, frame: &SiglusFrame<'_>) -> Result<SiglusFrameSnapshot> {
        if frame.plan.wipe.is_some() {
            bail!("Siglus stage wipe requires a two-target compositor");
        }
        let mut snapshot = SiglusFrameSnapshot {
            size: frame.size,
            draw: DrawList::new(),
            textures: Vec::new(),
            emote_textures: Vec::new(),
            sprite_count: frame.plan.submitted_sprite_count(),
        };
        let mut used = HashSet::new();
        for entry in &frame.plan.sprites {
            let sprite = &entry.sprite;
            if !sprite.visible || sprite.alpha == 0 {
                continue;
            }
            validate_sprite(sprite)?;
            let effects = ColorEffects::from_sprite(sprite);
            if let Some(packet) = &sprite.emote_render {
                let size = TextureInfo {
                    width: packet.width,
                    height: packet.height,
                };
                if let Some(mut command) = map_sprite(entry, TextureId(0), size, frame.size)? {
                    self.last_texture_id = self
                        .last_texture_id
                        .checked_add(1)
                        .context("Siglus texture IDs exhausted")?;
                    command.texture = TextureId(self.last_texture_id);
                    snapshot.emote_textures.push(SiglusEmoteTexture::new(
                        command.texture,
                        packet.clone(),
                        effects,
                    ));
                    snapshot.draw.push(command);
                }
                continue;
            }
            let (key, version, image, dense) = match &sprite.image_id {
                Some(handle) => {
                    let (image, version) = frame
                        .images
                        .get_entry(handle)
                        .context("Siglus frame references an unavailable image")?;
                    (
                        TextureKey::Image(handle.key(), effects),
                        version,
                        Some(image),
                        frame.images.dense_text_image(handle),
                    )
                }
                None => (TextureKey::Solid(effects), 0, None, None),
            };
            let size = image.as_ref().map_or(
                TextureInfo {
                    width: 1,
                    height: 1,
                },
                |image| TextureInfo {
                    width: image.width,
                    height: image.height,
                },
            );
            let Some(mut command) = map_sprite(entry, TextureId(0), size, frame.size)? else {
                continue;
            };
            let image = dense.or(image);
            if let Some(image) = &image {
                command.size = TextureInfo {
                    width: image.width,
                    height: image.height,
                };
                command.clip = ClipRect::full(command.size);
            }
            if self
                .textures
                .get(&key)
                .is_none_or(|(revision, _)| *revision != version)
            {
                let image = match image {
                    Some(image) if effects.is_identity() => image,
                    image => Arc::new(RgbaImage {
                        width: command.size.width,
                        height: command.size.height,
                        center_x: 0,
                        center_y: 0,
                        rgba: apply_color_effects(
                            image
                                .as_ref()
                                .map_or(&[255, 255, 255, 255], |i| i.rgba.as_slice()),
                            effects,
                        ),
                    }),
                };
                self.last_texture_id = self
                    .last_texture_id
                    .checked_add(1)
                    .context("Siglus texture IDs exhausted")?;
                self.textures.insert(
                    key,
                    (
                        version,
                        SiglusFrameTexture {
                            id: TextureId(self.last_texture_id),
                            image,
                        },
                    ),
                );
            }
            let (_, texture) = &self.textures[&key];
            if used.insert(key) {
                snapshot.textures.push(texture.clone());
            }
            command.texture = texture.id;
            snapshot.draw.push(command);
        }
        self.textures.retain(|key, _| used.contains(key));
        Ok(snapshot)
    }
}

/// Drawing adapter for Hosts using the shared Art3m1s GPU backend.
#[derive(Default)]
pub struct SiglusRenderer {
    compiler: SiglusFrameCompiler,
    emote: SiglusEmoteRenderer,
    textures: HashMap<TextureId, (Arc<RgbaImage>, TextureId)>,
    last_sprite_count: usize,
    last_command_count: usize,
}

impl SiglusRenderer {
    pub fn last_frame_counts(&self) -> (usize, usize) {
        (self.last_sprite_count, self.last_command_count)
    }

    pub fn draw(
        &mut self,
        frame: &SiglusFrame<'_>,
        gpu: &mut dyn GpuBackend,
        present: bool,
    ) -> Result<()> {
        let snapshot = self.compiler.compile(frame)?;
        self.draw_snapshot(&snapshot, gpu, present)
    }

    pub fn draw_snapshot(
        &mut self,
        frame: &SiglusFrameSnapshot,
        gpu: &mut dyn GpuBackend,
        present: bool,
    ) -> Result<()> {
        let textures = self.resolve_textures(frame)?;
        gpu.begin_access();
        let rendered = (|| {
            let mut used = HashSet::new();
            for texture in &textures {
                used.insert(texture.id);
                if self
                    .textures
                    .get(&texture.id)
                    .is_some_and(|(image, _)| Arc::ptr_eq(image, &texture.image))
                {
                    continue;
                }
                let image = &texture.image;
                let id = gpu
                    .create_texture(
                        &format!("siglus frame texture {}", texture.id.0),
                        TextureDesc::sampled_rgba8(image.width, image.height),
                        TextureData::Rgba8(&image.rgba),
                    )
                    .map_err(anyhow::Error::msg)?;
                if let Some((_, old)) = self.textures.insert(texture.id, (image.clone(), id)) {
                    gpu.destroy_texture(old);
                }
            }
            let mut draw = frame.draw_list().clone();
            for command in &mut draw.commands {
                command.texture = self.textures[&command.texture].1;
            }
            self.last_sprite_count = frame.sprite_count;
            self.last_command_count = draw.commands.len();
            gpu.begin_frame(FrameTarget::Main)
                .map_err(anyhow::Error::msg)?;
            gpu.clear([0.0, 0.0, 0.0, 1.0]);
            let damage = gpu.render(&draw).damage();
            gpu.end_frame();
            if present {
                gpu.present(damage).map_err(anyhow::Error::msg)?;
            }
            self.textures.retain(|key, (_, texture)| {
                if used.contains(key) {
                    true
                } else {
                    gpu.destroy_texture(*texture);
                    false
                }
            });
            Ok(())
        })();
        gpu.end_access();
        rendered
    }

    /// Resolve object targets while preserving the ordinary stage texture IDs.
    pub fn resolve_textures(
        &mut self,
        frame: &SiglusFrameSnapshot,
    ) -> Result<Vec<SiglusFrameTexture>> {
        self.emote.resolve_textures(frame)
    }

    /// Release resources before replacing or reusing the GPU backend.
    pub fn release_textures(&mut self, gpu: &mut dyn GpuBackend) {
        gpu.begin_access();
        for (_, (_, texture)) in self.textures.drain() {
            gpu.destroy_texture(texture);
        }
        gpu.end_access();
    }
}

#[cfg(feature = "uci-ffmpeg")]
struct UciStream(Vec<u8>);

#[cfg(feature = "uci-ffmpeg")]
impl art3m1s_media::MediaSource for UciStream {
    fn len(&self) -> std::result::Result<u64, String> {
        Ok(self.0.len() as u64)
    }

    fn read_at(&self, offset: u64, output: &mut [u8]) -> std::result::Result<usize, String> {
        let offset = usize::try_from(offset).map_err(|error| error.to_string())?;
        if offset >= self.0.len() {
            return Ok(0);
        }
        let count = output.len().min(self.0.len() - offset);
        output[..count].copy_from_slice(&self.0[offset..offset + count]);
        Ok(count)
    }
}

#[cfg(feature = "uci-ffmpeg")]
fn decode_uci_h264(stream: &[u8], pixel_format: &str, expected_len: usize) -> Result<Vec<u8>> {
    use art3m1s_media::ffmpeg::FfmpegVideoDecoder;

    let mut decoder = FfmpegVideoDecoder::open(Arc::new(UciStream(stream.to_vec())))
        .map_err(anyhow::Error::msg)
        .context("open in-process UCI H.264 decoder")?;
    let frame = decoder
        .next_rgba_frame()
        .map_err(anyhow::Error::msg)?
        .context("UCI H.264 stream has no frame")?;
    let pixels = match pixel_format {
        "rgba" => frame.pixels,
        "gray" => frame.pixels.chunks_exact(4).map(|pixel| pixel[0]).collect(),
        _ => bail!("unsupported UCI pixel format {pixel_format}"),
    };
    if pixels.len() != expected_len {
        bail!(
            "UCI decoded frame size mismatch: got {}, expected {expected_len}",
            pixels.len()
        );
    }
    Ok(pixels)
}

fn apply_color_effects(rgba: &[u8], effects: ColorEffects) -> Vec<u8> {
    let mut pixels = rgba.to_vec();
    let mono = effects.mono as f32 / 255.0;
    let reverse = effects.reverse as f32 / 255.0;
    let bright = effects.bright as f32 / 255.0;
    let dark = effects.dark as f32 / 255.0;
    let color_rate = effects.color_rate as f32 / 255.0;
    for pixel in pixels.chunks_exact_mut(4) {
        let original = [
            pixel[0] as f32 / 255.0,
            pixel[1] as f32 / 255.0,
            pixel[2] as f32 / 255.0,
        ];
        let gray = original[0] * 0.2989 + original[1] * 0.5886 + original[2] * 0.1145;
        for channel in 0..3 {
            let mut value = original[channel];
            value = value * (1.0 - reverse) + (1.0 - value) * reverse;
            value = value * (1.0 - mono) + gray * mono;
            value += bright - dark;
            value = value * (1.0 - color_rate)
                + (effects.color_target[channel] as f32 / 255.0) * color_rate;
            value += effects.color_add[channel] as f32 / 255.0;
            pixel[channel] = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    pixels
}

fn validate_sprite(s: &Sprite) -> Result<()> {
    // Siglus's ordinary 2D alpha test discards alpha < 1/255. With this
    // adapter's color-only composition, zero-alpha texels have the same
    // visible result under alpha blending; depth/shadow paths stay rejected.
    if s.mask_image_id.is_some()
        || s.tonecurve_image_id.is_some()
        || s.wipe_src_image_id.is_some()
        || s.fog_texture_image_id.is_some()
        || s.mesh_kind != 0
        || s.billboard
        || s.camera_enabled
        || s.z.abs() > f32::EPSILON
        || s.pivot_z.abs() > f32::EPSILON
        || (s.scale_z - 1.0).abs() > 1e-6
        || s.rotate_x.abs() > f32::EPSILON
        || s.rotate_y.abs() > f32::EPSILON
        || s.light_enabled
        || s.fog_enabled
        || s.wipe_fx_mode != 0
        || s.mask_mode != 0
        || !s.alpha_blend
    {
        bail!(
            "Siglus unsupported sprite effect: mask={} tonecurve={} wipe_source={} fog_texture={} mesh_kind={} billboard={} camera={} z={} pivot_z={} scale_z={} rotate_x={} rotate_y={} light={} fog={} wipe_mode={} mask_mode={} alpha_test={} alpha_blend={}",
            s.mask_image_id.is_some(),
            s.tonecurve_image_id.is_some(),
            s.wipe_src_image_id.is_some(),
            s.fog_texture_image_id.is_some(),
            s.mesh_kind,
            s.billboard,
            s.camera_enabled,
            s.z,
            s.pivot_z,
            s.scale_z,
            s.rotate_x,
            s.rotate_y,
            s.light_enabled,
            s.fog_enabled,
            s.wipe_fx_mode,
            s.mask_mode,
            s.alpha_test,
            s.alpha_blend,
        );
    }
    if !matches!(
        s.blend,
        SpriteBlend::Normal | SpriteBlend::Add | SpriteBlend::Mul | SpriteBlend::Screen
    ) {
        bail!("Siglus sprite blend mode is unsupported");
    }
    Ok(())
}

fn map_sprite(
    entry: &RenderSprite,
    texture: TextureId,
    texture_size: TextureInfo,
    viewport: (u32, u32),
) -> Result<Option<DrawCommand>> {
    let s = &entry.sprite;
    let sw = texture_size.width.max(1) as f32;
    let sh = texture_size.height.max(1) as f32;
    let vw = viewport.0.max(1) as f32;
    let vh = viewport.1.max(1) as f32;
    let (dx, dy, l, t, r, b, u0, v0, u1, v1) = match s.fit {
        SpriteFit::FullScreen => {
            let (l, t, r, b) = source_clip(s, sw, sh);
            (0.0, 0.0, 0.0, 0.0, vw, vh, l / sw, t / sh, r / sw, b / sh)
        }
        SpriteFit::PixelRect => {
            let (lw, lh) = match s.size_mode {
                SpriteSizeMode::Intrinsic => (sw, sh),
                SpriteSizeMode::Explicit { width, height } => {
                    (width.max(1) as f32, height.max(1) as f32)
                }
            };
            let Some((l, t, r, b)) = local_clip(s, lw, lh) else {
                return Ok(None);
            };
            (
                s.x as f32,
                s.y as f32,
                l,
                t,
                r,
                b,
                l / lw,
                t / lh,
                r / lw,
                b / lh,
            )
        }
    };
    let geometry = sprite_quad_geometry_rect(s, dx, dy, l, t, r, b, vw, vh)
        .context("Siglus sprite projection failed")?;
    let p = geometry.projected;
    let vertices = [
        [p[0].x, p[0].y, u0, v0],
        [p[1].x, p[1].y, u1, v0],
        [p[2].x, p[2].y, u1, v1],
        [p[0].x, p[0].y, u0, v0],
        [p[2].x, p[2].y, u1, v1],
        [p[3].x, p[3].y, u0, v1],
    ];
    let blend = match s.blend {
        SpriteBlend::Normal => BlendMode::Alpha,
        SpriteBlend::Add => BlendMode::Add,
        SpriteBlend::Mul => BlendMode::Multiply,
        SpriteBlend::Screen => BlendMode::Screen,
        _ => unreachable!("validated above"),
    };
    Ok(Some(DrawCommand {
        texture,
        size: texture_size,
        transform: glam::Affine2::IDENTITY,
        opacity: (s.alpha as f32 / 255.0) * (s.tr as f32 / 255.0),
        blend,
        color: ColorFilter::default(),
        clip: ClipRect::full(texture_size),
        clip_bounds: s.dst_clip.map(|c| {
            [
                c.left as f32,
                c.top as f32,
                (c.right - c.left) as f32,
                (c.bottom - c.top) as f32,
            ]
        }),
        shader: None,
        mesh: Some(DrawMesh {
            vertices: Arc::new(vertices),
        }),
        stencil: None,
        native_emote: None,
    }))
}

fn source_clip(s: &Sprite, w: f32, h: f32) -> (f32, f32, f32, f32) {
    if let Some(c) = s.src_clip {
        let l = (c.left as f32).clamp(0.0, w);
        let t = (c.top as f32).clamp(0.0, h);
        let r = (c.right as f32).clamp(0.0, w);
        let b = (c.bottom as f32).clamp(0.0, h);
        if r > l && b > t {
            return (l, t, r, b);
        }
    }
    (0.0, 0.0, w, h)
}

fn local_clip(s: &Sprite, w: f32, h: f32) -> Option<(f32, f32, f32, f32)> {
    let cx = s.pivot_x
        + if s.object_anchor {
            s.texture_center_x
        } else {
            0.0
        };
    let cy = s.pivot_y
        + if s.object_anchor {
            s.texture_center_y
        } else {
            0.0
        };
    let mut l = -cx;
    let mut t = -cy;
    let mut r = w - cx;
    let mut b = h - cy;
    if let Some(c) = s.src_clip {
        l = l.max(c.left as f32);
        t = t.max(c.top as f32);
        r = r.min(c.right as f32);
        b = b.min(c.bottom as f32);
    }
    (r > l && b > t).then_some((l + cx, t + cy, r + cx, b + cy))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_sprite_maps_to_direct_mesh() {
        let sprite = Sprite {
            visible: true,
            x: 12,
            y: 34,
            ..Sprite::default()
        };
        let entry = RenderSprite::new(None, None, sprite);
        let command = map_sprite(
            &entry,
            TextureId(7),
            TextureInfo {
                width: 20,
                height: 10,
            },
            (1280, 720),
        )
        .unwrap()
        .unwrap();
        let vertices = &command.mesh.unwrap().vertices;
        assert_eq!(vertices[0], [12.0, 34.0, 0.0, 0.0]);
        assert_eq!(vertices[2], [32.0, 44.0, 1.0, 1.0]);
    }

    #[test]
    fn siglus_emote_objects_defer_texture_rendering_in_stage_order() {
        use siglus_scene_vm::emote::SiglusEmoteRuntime;

        let psb = include_bytes!("../../../src/host/script_io/fixtures/emote-bgra-red.psb");
        let player = SiglusEmoteRuntime::from_psb_bytes(psb, None).unwrap();
        let packet = player.packet(8, 8, 0, 0, true);
        let images = ImageManager::new(PathBuf::from("siglus-emote-frame-test"));
        let sprite = |x, emote_render| {
            RenderSprite::new(
                None,
                None,
                Sprite {
                    visible: true,
                    x,
                    emote_render,
                    ..Sprite::default()
                },
            )
        };
        let plan = RenderFrame::ordinary(vec![
            sprite(2, None),
            sprite(6, Some(packet.clone())),
            sprite(10, None),
        ]);
        let frame = SiglusFrame {
            size: (16, 16),
            plan,
            images: &images,
        };
        let snapshot = SiglusFrameCompiler::default().compile(&frame).unwrap();
        let [model] = snapshot.emote_textures() else {
            panic!("expected one deferred object texture");
        };
        assert!(Arc::ptr_eq(&model.packet, &packet));
        let commands = &snapshot.draw_list().commands;
        assert_eq!(commands.len(), 3);
        assert_eq!(commands[1].texture, model.id);
        assert_eq!(commands[0].texture, commands[2].texture);
        assert_eq!(snapshot.textures().len(), 1);
        assert_eq!(
            commands[1].mesh.as_ref().unwrap().vertices[2],
            [14., 8., 1., 1.]
        );
    }

    #[test]
    fn retained_snapshot_keeps_texture_revision() {
        let mut images = ImageManager::new(PathBuf::from("siglus-frame-test"));
        let image_id = images.solid_rgba((10, 20, 30, 255));
        let render = || {
            RenderFrame::ordinary(vec![RenderSprite::new(
                None,
                None,
                Sprite {
                    visible: true,
                    image_id: Some(image_id.clone()),
                    ..Sprite::default()
                },
            )])
        };
        let mut compiler = SiglusFrameCompiler::default();
        let frame = |compiler: &mut SiglusFrameCompiler, images: &ImageManager| {
            compiler
                .compile(&SiglusFrame {
                    size: (320, 240),
                    plan: render(),
                    images,
                })
                .unwrap()
        };
        let first = frame(&mut compiler, &images);
        let same = frame(&mut compiler, &images);
        assert_eq!(first.textures()[0].id, same.textures()[0].id);
        assert!(Arc::ptr_eq(
            &first.textures()[0].image,
            &same.textures()[0].image
        ));
        images
            .replace_image(
                &image_id,
                RgbaImage {
                    width: 1,
                    height: 1,
                    center_x: 0,
                    center_y: 0,
                    rgba: vec![40, 50, 60, 255],
                },
            )
            .unwrap();
        let second = frame(&mut compiler, &images);
        assert_ne!(first.textures()[0].id, second.textures()[0].id);
        assert_eq!(first.textures()[0].image.rgba, [10, 20, 30, 255]);
        assert_eq!(second.textures()[0].image.rgba, [40, 50, 60, 255]);
        assert_eq!(
            second.draw_list().commands[0].texture,
            second.textures()[0].id
        );
    }

    #[test]
    fn unsupported_effect_fails_explicitly() {
        let sprite = Sprite {
            wipe_fx_mode: 1,
            ..Sprite::default()
        };
        assert!(validate_sprite(&sprite).is_err());
    }
}
