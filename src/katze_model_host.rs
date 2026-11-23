use crate::emote_playback::EmotePlayback;
use art3m1s_emote::{EmoteModel, PsbDocument};
use art3m1s_render::{
    ALPHA_MASK_SHADER, DrawList, Extent2D, FrameTarget, GpuBackend, TextureData, TextureDesc,
    TextureId, TextureInfo,
};
use asb_interpreter::EmoteLayerCommand;
use encoding_rs::{Encoding, SHIFT_JIS};
use flate2::{Decompress, FlushDecompress, Status};
use glam::{Affine2, Vec2};
use std::{
    collections::BTreeMap,
    ffi::{CStr, c_char, c_void},
    mem::size_of,
    ptr, slice,
};

const OK: i32 = 0;
const ABSENT: i32 = 1;
const ERROR: i32 = -5;
const MAX_PSZ: usize = 128 * 1024 * 1024;
const MAX_FRAME_SIDE: i32 = 4096;

#[repr(C)]
#[derive(Clone, Copy)]
struct Bytes {
    data: *const u8,
    len: usize,
}

impl Bytes {
    unsafe fn view<'a>(self) -> &'a [u8] {
        if self.len == 0 {
            &[]
        } else {
            unsafe { slice::from_raw_parts(self.data, self.len) }
        }
    }

    fn of(bytes: &[u8]) -> Self {
        Self {
            data: bytes.as_ptr(),
            len: bytes.len(),
        }
    }
}

#[repr(C)]
struct SourceReader {
    len: u64,
    source: *const c_void,
    read_at: unsafe extern "C" fn(*const c_void, u64, *mut u8, usize, *mut usize) -> i32,
}

#[repr(C)]
struct Input {
    psz: SourceReader,
    initial_motion: Bytes,
    expression_table: Bytes,
    body_table: Bytes,
    options: Bytes,
    transition: u32,
}

#[repr(C)]
struct Output {
    size: [u32; 2],
    origin: [i32; 2],
    bgra: Bytes,
}

type Open = unsafe extern "C" fn(u64, *const Input, *mut u64) -> i32;
type Frame = unsafe extern "C" fn(u64, u64, u32, *mut Output) -> i32;
type Select = unsafe extern "C" fn(u64, u64, Bytes, u32) -> i32;
type SetName = unsafe extern "C" fn(u64, u64, Bytes) -> i32;
type GetBytes = unsafe extern "C" fn(u64, u64, *mut Bytes) -> i32;
type Action = unsafe extern "C" fn(u64, u64) -> i32;
type Unused = Option<unsafe extern "C" fn()>;

// Field order and signatures follow KatzeModelHost in the development ABI 0 header.
#[repr(C)]
struct Host {
    struct_size: u32,
    reserved: u32,
    context: u64,
    open: Open,
    set_options: Select,
    set_body_options: SetName,
    set_effects_mode: Unused,
    set_tone: Unused,
    effects_active: Unused,
    frame: Frame,
    close: unsafe extern "C" fn(u64, u64),
    default_body_options: Unused,
    get_setting: Unused,
    set_setting: Unused,
    reset_settings: Unused,
    reset_view: Unused,
    clear_actions: Action,
    advance_action: Unused,
    start_action: SetName,
    action_active: Unused,
    attribute: Unused,
    set_differences: Select,
    current_differences: GetBytes,
    set_difference_scale: Unused,
    clear_effects: Unused,
    update_lip_sync: Unused,
    start_lip_track: Unused,
    set_feature_mode: Unused,
    set_waiting_motion: SetName,
    set_inertia_group: Unused,
    refresh_effects: Unused,
    start_fade: Unused,
    set_color_filter: Unused,
}

const fn host(context: u64) -> Host {
    Host {
        struct_size: size_of::<Host>() as u32,
        reserved: 0,
        context,
        open,
        set_options,
        set_body_options,
        set_effects_mode: None,
        set_tone: None,
        effects_active: None,
        frame,
        close,
        default_body_options: None,
        get_setting: None,
        set_setting: None,
        reset_settings: None,
        reset_view: None,
        clear_actions,
        advance_action: None,
        start_action,
        action_active: None,
        attribute: None,
        set_differences: set_options,
        current_differences,
        set_difference_scale: None,
        clear_effects: None,
        update_lip_sync: None,
        start_lip_track: None,
        set_feature_mode: None,
        set_waiting_motion,
        set_inertia_group: None,
        refresh_effects: None,
        start_fade: None,
        set_color_filter: None,
    }
}

/// Returns a model callback table with the corresponding script encoding.
///
/// # Safety
/// `label` must point to a NUL-terminated string or be null for CP932.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_katze_model_host_v0(label: *const c_char) -> *const c_void {
    static JAPANESE: Host = host(0);
    static SIMPLIFIED_CHINESE: Host = host(1);
    static UTF8: Host = host(2);
    static TRADITIONAL_CHINESE: Host = host(3);
    let label = if label.is_null() {
        &[][..]
    } else {
        unsafe { CStr::from_ptr(label) }.to_bytes()
    };
    let encoding = Encoding::for_label(label).unwrap_or(SHIFT_JIS);
    let table = if [encoding_rs::GBK, encoding_rs::GB18030].contains(&encoding) {
        &SIMPLIFIED_CHINESE
    } else if encoding == encoding_rs::UTF_8 {
        &UTF8
    } else if encoding == encoding_rs::BIG5 {
        &TRADITIONAL_CHINESE
    } else {
        &JAPANESE
    };
    ptr::from_ref(table).cast()
}

fn encoding(context: u64) -> &'static Encoding {
    match context {
        1 => encoding_rs::GB18030,
        2 => encoding_rs::UTF_8,
        3 => encoding_rs::BIG5,
        _ => SHIFT_JIS,
    }
}

fn guarded(call: impl FnOnce() -> Result<i32, String>) -> i32 {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(call)) {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            eprintln!("CatSystem2 model: {error}");
            ERROR
        }
        Err(_) => ERROR,
    }
}

unsafe extern "C" fn open(context: u64, input: *const Input, token: *mut u64) -> i32 {
    guarded(|| {
        let input = unsafe { &*input };
        let bytes = read_psz(&input.psz)?;
        let model =
            EmoteModel::from_document(PsbDocument::from_bytes(bytes).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let mut model = Model::load(model)?;
        let encoding = encoding(context);
        let decode = |bytes: Bytes| encoding.decode(unsafe { bytes.view() }).0;
        model.expression = Differences::parse(&decode(input.expression_table));
        model.body = Differences::parse(&decode(input.body_table));
        let initial = decode(input.initial_motion);
        model.play(&initial);
        model.select(unsafe { input.options.view() }, encoding);
        unsafe { token.write(Box::into_raw(Box::new(model)) as u64) };
        Ok(OK)
    })
}

unsafe extern "C" fn close(_: u64, token: u64) {
    guarded(|| {
        drop(unsafe { Box::from_raw(token as *mut Model) });
        Ok(OK)
    });
}

unsafe extern "C" fn set_options(context: u64, token: u64, options: Bytes, _: u32) -> i32 {
    guarded(|| {
        let model = unsafe { &mut *(token as *mut Model) };
        model.select(unsafe { options.view() }, encoding(context));
        Ok(OK)
    })
}

unsafe extern "C" fn set_body_options(context: u64, token: u64, options: Bytes) -> i32 {
    guarded(|| {
        let model = unsafe { &mut *(token as *mut Model) };
        let keys = encoding(context).decode(unsafe { options.view() }).0;
        model.body_values = model.body.select(&keys);
        model.apply();
        Ok(OK)
    })
}

unsafe extern "C" fn current_differences(_: u64, token: u64, out: *mut Bytes) -> i32 {
    guarded(|| {
        let model = unsafe { &*(token as *const Model) };
        if model.keys.is_empty() {
            return Ok(ABSENT);
        }
        unsafe { out.write(Bytes::of(&model.keys)) };
        Ok(OK)
    })
}

unsafe extern "C" fn start_action(context: u64, token: u64, name: Bytes) -> i32 {
    guarded(|| {
        let model = unsafe { &mut *(token as *mut Model) };
        let label = encoding(context).decode(unsafe { name.view() }).0;
        if model.play(&label) {
            Ok(OK)
        } else {
            Ok(ABSENT)
        }
    })
}

unsafe extern "C" fn clear_actions(_: u64, token: u64) -> i32 {
    guarded(|| {
        let model = unsafe { &mut *(token as *mut Model) };
        if let Some(action) = model.action.take() {
            model
                .playback
                .command(EmoteLayerCommand::StopTimeline { label: action });
            model.dirty = true;
        }
        Ok(OK)
    })
}

unsafe extern "C" fn set_waiting_motion(context: u64, token: u64, name: Bytes) -> i32 {
    if name.data.is_null() {
        unsafe { clear_actions(context, token) }
    } else {
        unsafe { start_action(context, token, name) }
    }
}

unsafe extern "C" fn frame(_: u64, token: u64, time: u32, out: *mut Output) -> i32 {
    guarded(|| {
        let model = unsafe { &mut *(token as *mut Model) };
        model.render(time)?;
        if model.pixels.is_empty() {
            return Ok(ABSENT);
        }
        unsafe {
            out.write(Output {
                size: model.size,
                origin: model.origin,
                bgra: Bytes::of(&model.pixels),
            });
        }
        Ok(OK)
    })
}

fn read_psz(reader: &SourceReader) -> Result<Vec<u8>, String> {
    let len = usize::try_from(reader.len).map_err(|_| "Invalid PSZ size")?;
    if !(16..=MAX_PSZ).contains(&len) {
        return Err("Invalid PSZ size".into());
    }
    let mut bytes = vec![0; len];
    let mut offset = 0;
    while offset < bytes.len() {
        let remaining = &mut bytes[offset..];
        let mut count = 0;
        let result = unsafe {
            (reader.read_at)(
                reader.source,
                offset as u64,
                remaining.as_mut_ptr(),
                remaining.len(),
                &mut count,
            )
        };
        if result != OK || count == 0 || count > remaining.len() {
            return Err("Cannot read PSZ source".into());
        }
        offset += count;
    }
    let packed_len = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let size = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    if &bytes[..4] != b"PSZ\0" || packed_len != len - 16 || !(4..=MAX_PSZ).contains(&size) {
        return Err("Invalid PSZ header".into());
    }
    let mut result = vec![0; size];
    let mut inflate = Decompress::new(true);
    let status = inflate
        .decompress(&bytes[16..], &mut result, FlushDecompress::Finish)
        .map_err(|e| e.to_string())?;
    if status != Status::StreamEnd
        || inflate.total_out() != size as u64
        || inflate.total_in() != packed_len as u64
        || !result.starts_with(b"PSB\0")
    {
        return Err("Invalid PSZ payload".into());
    }
    Ok(result)
}

type Assignments = BTreeMap<String, f32>;

#[derive(Default)]
struct Differences(BTreeMap<usize, BTreeMap<String, Assignments>>);

impl Differences {
    fn parse(text: &str) -> Self {
        let mut patterns = Self::default();
        let mut section = false;
        let mut current = None;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                section = line.eq_ignore_ascii_case("#pattern");
                current = None;
            } else if section && !line.starts_with([';', '/']) {
                let mut fields = line.split_ascii_whitespace();
                let (Some(name), Some(value)) = (fields.next(), fields.next()) else {
                    continue;
                };
                if let Some(group) = name.strip_prefix('%') {
                    current = group
                        .parse()
                        .ok()
                        .map(|group| (group, value.trim_matches('"').to_ascii_lowercase()));
                } else if let Some((group, key)) = &current
                    && let Ok(value) = value.parse::<f32>()
                    && value.is_finite()
                    && value != 99.0
                {
                    patterns
                        .0
                        .entry(*group)
                        .or_default()
                        .entry(key.clone())
                        .or_default()
                        .insert(name.trim_matches('"').to_owned(), value);
                }
            }
        }
        patterns
    }

    fn select(&self, keys: &str) -> Assignments {
        keys.split([',', ':'])
            .enumerate()
            .filter_map(|(index, key)| {
                self.0
                    .get(&(index + 1))?
                    .get(&key.trim().to_ascii_lowercase())
            })
            .flat_map(|values| values.iter().map(|(label, value)| (label.clone(), *value)))
            .collect()
    }
}

struct Model {
    playback: EmotePlayback,
    gpu: Box<dyn GpuBackend>,
    textures: BTreeMap<String, (TextureId, TextureInfo)>,
    expression: Differences,
    body: Differences,
    expression_values: Assignments,
    body_values: Assignments,
    applied: Assignments,
    keys: Vec<u8>,
    action: Option<String>,
    origin: [i32; 2],
    size: [u32; 2],
    pixels: Vec<u8>,
    last_tick: Option<u32>,
    dirty: bool,
}

impl Model {
    fn load(mut model: EmoteModel) -> Result<Self, String> {
        let mut gpu = create_backend()?;
        let (_, sources) = model.take_texture_data().map_err(|e| e.to_string())?;
        let mut textures = BTreeMap::new();
        for (name, texture) in model.atlas().textures() {
            let info = TextureInfo {
                width: texture.width,
                height: texture.height,
            };
            let rgba = model
                .atlas()
                .decode_texture_data_rgba8(name, sources[name].as_bytes())
                .map_err(|e| e.to_string())?;
            let id = gpu.create_texture(
                name,
                TextureDesc::sampled_rgba8(info.width, info.height),
                TextureData::Rgba8(&rgba),
            )?;
            textures.insert(name.clone(), (id, info));
        }
        Ok(Self {
            playback: EmotePlayback::new(model, 1, 0, 0),
            gpu,
            textures,
            expression: Differences::default(),
            body: Differences::default(),
            expression_values: Assignments::new(),
            body_values: Assignments::new(),
            applied: Assignments::new(),
            keys: Vec::new(),
            action: None,
            origin: [0; 2],
            size: [0; 2],
            pixels: Vec::new(),
            last_tick: None,
            dirty: true,
        })
    }

    fn play(&mut self, name: &str) -> bool {
        if !self.playback.model.timelines().contains_key(name) {
            return false;
        }
        if let Some(previous) = self.action.replace(name.to_owned()) {
            self.playback
                .command(EmoteLayerCommand::StopTimeline { label: previous });
        }
        self.playback.command(EmoteLayerCommand::PlayTimeline {
            label: name.to_owned(),
            flags: 0,
        });
        self.dirty = true;
        true
    }

    fn select(&mut self, options: &[u8], encoding: &'static Encoding) {
        self.keys.clear();
        self.keys.extend_from_slice(options);
        self.expression_values = self.expression.select(&encoding.decode(options).0);
        self.apply();
    }

    fn apply(&mut self) {
        let values: Assignments = self
            .expression_values
            .iter()
            .chain(&self.body_values)
            .map(|(name, value)| (name.clone(), *value))
            .collect();
        if values == self.applied {
            return;
        }
        for (label, value) in &values {
            self.playback.command(EmoteLayerCommand::SetVariable {
                label: label.clone(),
                value: *value,
                frames: 0.,
                easing: 0,
            });
        }
        for label in self
            .applied
            .keys()
            .filter(|label| !values.contains_key(*label))
        {
            self.playback.clear_variable(label);
        }
        self.applied = values;
        self.dirty = true;
    }

    fn render(&mut self, time: u32) -> Result<(), String> {
        if let Some(last) = self.last_tick.replace(time) {
            if last != time {
                self.dirty |= self.playback.advance(time.wrapping_sub(last) as f32 * 0.06);
            }
        }
        if !self.dirty {
            return Ok(());
        }
        let mut list = DrawList::new();
        let mut min = Vec2::splat(f32::INFINITY);
        let mut max = Vec2::splat(f32::NEG_INFINITY);
        for item in self.playback.evaluate()? {
            let Some(&(id, info)) = self.textures.get(&item.texture_id) else {
                continue;
            };
            let Some(mut command) = self.playback.draw_command(item, id, info) else {
                continue;
            };
            let mut include = |point| {
                let p = command.transform.transform_point2(point);
                min = min.min(p);
                max = max.max(p);
            };
            if let Some(mesh) = &command.mesh {
                for &[x, y, _, _] in mesh.vertices.iter() {
                    include(Vec2::new(x, y));
                }
            } else {
                let [w, h] = command.clip.quad_size;
                for point in [
                    Vec2::ZERO,
                    Vec2::new(w, 0.),
                    Vec2::new(0., h),
                    Vec2::new(w, h),
                ] {
                    include(point);
                }
            }
            command.clip_bounds = None;
            list.push(command);
        }
        if list.is_empty() {
            self.pixels.clear();
            self.dirty = false;
            return Ok(());
        }
        let min = min.floor();
        let max = max.ceil();
        let size = max - min;
        if !min.is_finite()
            || !size.is_finite()
            || size.min_element() < 1.0
            || size.max_element() > MAX_FRAME_SIDE as f32
        {
            return Err("Invalid model bounds".into());
        }
        let origin = [min.x as i32, min.y as i32];
        let size = [size.x as u32, size.y as u32];
        let extent = Extent2D::new(size[0], size[1]);
        let translation = Affine2::from_translation(-min);
        for cmd in &mut list.commands {
            cmd.transform = translation * cmd.transform;
        }
        list.materialize_stencil_groups(ALPHA_MASK_SHADER);
        if self.size != size {
            self.gpu.resize(extent)?;
        }
        self.gpu.begin_frame(FrameTarget::Main)?;
        self.gpu.render_with_clear_color(&list, [0.; 4]);
        self.gpu.end_frame();
        self.pixels
            .resize(extent.rgba8_len().ok_or("Invalid frame size")?, 0);
        let written = self
            .gpu
            .readback(FrameTarget::Main, extent, &mut self.pixels)?;
        if written != self.pixels.len() {
            return Err("Incomplete model readback".into());
        }
        for pixel in self.pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
            let alpha = u32::from(pixel[3]);
            if alpha > 0 && alpha < 255 {
                for c in &mut pixel[..3] {
                    *c = ((u32::from(*c) * 255 + alpha / 2) / alpha).min(255) as u8;
                }
            }
        }
        self.size = size;
        self.origin = origin;
        self.dirty = false;
        Ok(())
    }
}

#[cfg(all(feature = "metal-backend", any(target_os = "macos", target_os = "ios")))]
fn create_backend() -> Result<Box<dyn GpuBackend>, String> {
    Ok(Box::new(art3m1s_render::backend::metal::MetalBackend::new(
        1, 1,
    )?))
}

#[cfg(all(
    feature = "vulkan-backend",
    any(target_os = "linux", target_os = "windows", target_os = "android")
))]
fn create_backend() -> Result<Box<dyn GpuBackend>, String> {
    Ok(Box::new(
        art3m1s_render::backend::vulkan::VulkanBackend::new(1, 1)?,
    ))
}
