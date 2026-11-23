mod difference;
mod lip;

use difference::{Assignments, Differences, Scale, Selection};

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
    slice,
    sync::atomic::{AtomicI32, Ordering},
};

const OK: i32 = 0;
const ABSENT: i32 = 1;
const ERROR: i32 = -5;
const MAX_PSZ: usize = 128 * 1024 * 1024;
const MAX_FRAME_SIDE: i32 = 4096;
const MAX_BODY_OPTIONS_BYTES: usize = 0x2ff;
const DIFFERENCE_EFFECTS_SETTING: u32 = 2;
const LIP_SYNC_SETTING: u32 = 5;
const FADE_SETTING: u32 = 6;

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
type SetMode = unsafe extern "C" fn(u64, u64, u32) -> i32;
type GetSetting = unsafe extern "C" fn(u64, u32, Bytes, *mut i32) -> i32;
type SetSetting = unsafe extern "C" fn(u64, u32, Bytes, i32) -> i32;
type UpdateLipSync = unsafe extern "C" fn(u64, u64, u32, *const u32, usize) -> i32;
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
    set_effects_mode: SetMode,
    set_tone: Unused,
    effects_active: SetMode,
    frame: Frame,
    close: unsafe extern "C" fn(u64, u64),
    default_body_options: GetBytes,
    get_setting: GetSetting,
    set_setting: SetSetting,
    reset_settings: unsafe extern "C" fn(u64) -> i32,
    reset_view: Unused,
    clear_actions: Action,
    advance_action: Unused,
    start_action: SetName,
    action_active: Unused,
    attribute: Unused,
    set_differences: Select,
    current_differences: GetBytes,
    set_difference_scale: unsafe extern "C" fn(u64, u64, u32, Bytes, f32) -> i32,
    clear_effects: Action,
    update_lip_sync: UpdateLipSync,
    start_lip_track: Unused,
    set_feature_mode: unsafe extern "C" fn(u64, u64, u32, u32) -> i32,
    set_waiting_motion: SetName,
    set_inertia_group: Unused,
    refresh_effects: Unused,
    start_fade: SetMode,
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
        set_effects_mode,
        set_tone: None,
        effects_active,
        frame,
        close,
        default_body_options,
        get_setting,
        set_setting,
        reset_settings,
        reset_view: None,
        clear_actions,
        advance_action: None,
        start_action,
        action_active: None,
        attribute: None,
        set_differences,
        current_differences,
        set_difference_scale,
        clear_effects,
        update_lip_sync,
        start_lip_track: None,
        set_feature_mode,
        set_waiting_motion,
        set_inertia_group: None,
        refresh_effects: None,
        start_fade,
        set_color_filter: None,
    }
}

#[repr(C)]
struct SessionHost {
    callbacks: Host,
    encoding: &'static Encoding,
    fade: AtomicI32,
    difference_effects: AtomicI32,
    lip_sync: AtomicI32,
}

/// Creates a model callback table owned by the caller.
///
/// # Safety
/// `label` must point to a NUL-terminated string or be null for CP932.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_katze_model_host_v0(label: *const c_char) -> *const c_void {
    let label = if label.is_null() {
        &[][..]
    } else {
        unsafe { CStr::from_ptr(label) }.to_bytes()
    };
    let session = Box::into_raw(Box::new(SessionHost {
        callbacks: host(0),
        encoding: Encoding::for_label(label).unwrap_or(SHIFT_JIS),
        fade: AtomicI32::new(0),
        difference_effects: AtomicI32::new(1),
        lip_sync: AtomicI32::new(1),
    }));
    unsafe { (*session).callbacks.context = session as u64 };
    session.cast()
}

/// # Safety
/// `callbacks` is the owned table. Release it after the runtime session is destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn art3m1s_katze_model_host_destroy_v0(callbacks: *const c_void) {
    if !callbacks.is_null() {
        drop(unsafe { Box::from_raw(callbacks.cast::<SessionHost>().cast_mut()) });
    }
}

unsafe fn session(context: u64) -> &'static SessionHost {
    unsafe { &*(context as *const SessionHost) }
}

impl SessionHost {
    fn setting(&self, id: u32) -> Option<&AtomicI32> {
        match id {
            LIP_SYNC_SETTING => Some(&self.lip_sync),
            FADE_SETTING => Some(&self.fade),
            DIFFERENCE_EFFECTS_SETTING => Some(&self.difference_effects),
            _ => None,
        }
    }
}

unsafe extern "C" fn get_setting(context: u64, setting: u32, _: Bytes, out: *mut i32) -> i32 {
    let Some(setting) = (unsafe { session(context) }).setting(setting) else {
        return ABSENT;
    };
    unsafe { out.write(setting.load(Ordering::Relaxed)) };
    OK
}

unsafe extern "C" fn set_setting(context: u64, setting: u32, _: Bytes, value: i32) -> i32 {
    let Some(setting) = (unsafe { session(context) }).setting(setting) else {
        return ABSENT;
    };
    setting.store(value, Ordering::Relaxed);
    OK
}

unsafe extern "C" fn reset_settings(context: u64) -> i32 {
    let session = unsafe { session(context) };
    session.fade.store(0, Ordering::Relaxed);
    session.difference_effects.store(1, Ordering::Relaxed);
    session.lip_sync.store(1, Ordering::Relaxed);
    OK
}

fn guarded(call: impl FnOnce() -> Result<i32, String>) -> i32 {
    let run = || std::panic::catch_unwind(std::panic::AssertUnwindSafe(call));
    // Session callbacks can run on a Rust worker without an AppKit pool.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let result = objc2::rc::autoreleasepool(|_| run());
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    let result = run();
    match result {
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
        let encoding = unsafe { session(context) }.encoding;
        let decode = |bytes: Bytes| encoding.decode(unsafe { bytes.view() }).0;
        let expression = decode(input.expression_table);
        model.expression = Differences::parse(&expression);
        model.lip = lip::LipSync::parse(&expression);
        model.body = Differences::parse(&decode(input.body_table));
        if let Some(keys) = model.body.default_keys() {
            let (encoded, _, had_errors) = encoding.encode(&keys);
            if !had_errors && encoded.len() <= MAX_BODY_OPTIONS_BYTES {
                model.default_body_keys = encoded.into_owned();
                model.body_selection.select(&model.body, &keys, false);
            }
        }
        let initial = decode(input.initial_motion);
        model.play(&initial);
        model.select(unsafe { input.options.view() }, encoding, false);
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

unsafe extern "C" fn set_options(context: u64, token: u64, options: Bytes, transition: u32) -> i32 {
    guarded(|| {
        let model = unsafe { &mut *(token as *mut Model) };
        let settings = unsafe { session(context) };
        model.select(
            unsafe { options.view() },
            settings.encoding,
            transition != 0 && model.effects_enabled(settings),
        );
        Ok(OK)
    })
}

unsafe extern "C" fn set_differences(context: u64, token: u64, keys: Bytes, mode: u32) -> i32 {
    unsafe { set_options(context, token, keys, u32::from(mode != 0)) }
}

unsafe extern "C" fn set_effects_mode(_: u64, token: u64, mode: u32) -> i32 {
    let model = unsafe { &mut *(token as *mut Model) };
    model.effects_mode = mode;
    OK
}

unsafe extern "C" fn set_difference_scale(
    context: u64,
    token: u64,
    scale: u32,
    parameter: Bytes,
    multiplier: f32,
) -> i32 {
    if !multiplier.is_finite() {
        return ABSENT;
    }
    guarded(|| {
        let model = unsafe { &mut *(token as *mut Model) };
        let scale = match scale {
            0 => &mut model.difference_speed,
            1 => &mut model.difference_value,
            _ => return Ok(ABSENT),
        };
        let name = (!parameter.data.is_null()).then(|| {
            unsafe { session(context) }
                .encoding
                .decode(unsafe { parameter.view() })
                .0
                .into_owned()
        });
        scale.set(name, multiplier);
        model.differences_dirty = true;
        Ok(OK)
    })
}

unsafe extern "C" fn set_body_options(context: u64, token: u64, options: Bytes) -> i32 {
    guarded(|| {
        let model = unsafe { &mut *(token as *mut Model) };
        let settings = unsafe { session(context) };
        let options = if options.len == 0 {
            &model.default_body_keys
        } else {
            unsafe { options.view() }
        };
        let keys = settings.encoding.decode(options).0;
        let fade = model
            .body_selection
            .select(&model.body, &keys, model.effects_enabled(settings));
        if fade.is_some() {
            model.pending_chain_fade = fade;
        }
        model.differences_dirty = true;
        Ok(OK)
    })
}

unsafe extern "C" fn default_body_options(_: u64, token: u64, out: *mut Bytes) -> i32 {
    let model = unsafe { &*(token as *const Model) };
    if model.default_body_keys.is_empty() {
        return ABSENT;
    }
    unsafe { out.write(Bytes::of(&model.default_body_keys)) };
    OK
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
        let label = unsafe { session(context) }
            .encoding
            .decode(unsafe { name.view() })
            .0;
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

unsafe extern "C" fn start_fade(_: u64, token: u64, duration: u32) -> i32 {
    unsafe { &mut *(token as *mut Model) }.pending_fade = Some(duration);
    OK
}

unsafe extern "C" fn clear_effects(_: u64, token: u64) -> i32 {
    let model = unsafe { &mut *(token as *mut Model) };
    model.fade = None;
    model.finish_differences();
    OK
}

unsafe extern "C" fn effects_active(context: u64, token: u64, time: u32) -> i32 {
    guarded(|| {
        let model = unsafe { &mut *(token as *mut Model) };
        model.advance_differences(time, unsafe { session(context) });
        Ok(
            if model.fade.is_some()
                || model.expression_selection.active()
                || model.body_selection.active()
            {
                OK
            } else {
                ABSENT
            },
        )
    })
}

unsafe extern "C" fn set_feature_mode(_: u64, token: u64, feature: u32, mode: u32) -> i32 {
    if feature != LIP_SYNC_SETTING {
        return ABSENT;
    }
    let model = unsafe { &mut *(token as *mut Model) };
    model.lip_enabled = mode != 0;
    if !model.lip_enabled {
        model.lip.observe(None);
    }
    OK
}

unsafe extern "C" fn update_lip_sync(
    context: u64,
    token: u64,
    playing: u32,
    levels: *const u32,
    count: usize,
) -> i32 {
    guarded(|| {
        let model = unsafe { &mut *(token as *mut Model) };
        let enabled =
            model.lip_enabled && unsafe { session(context) }.lip_sync.load(Ordering::Relaxed) != 0;
        let levels = if playing == 0 || !enabled {
            None
        } else if count == 0 {
            Some(&[][..])
        } else {
            Some(unsafe { slice::from_raw_parts(levels, count) })
        };
        model.lip.observe(levels);
        Ok(OK)
    })
}

unsafe extern "C" fn frame(context: u64, token: u64, time: u32, out: *mut Output) -> i32 {
    guarded(|| {
        let model = unsafe { &mut *(token as *mut Model) };
        let settings = unsafe { session(context) };
        if !model.lip_enabled || settings.lip_sync.load(Ordering::Relaxed) == 0 {
            model.lip.observe(None);
        }
        model.advance_differences(time, settings);
        model.advance_frame(time, settings.fade.load(Ordering::Relaxed) != 0)?;
        let pixels = model.output();
        if pixels.bgra.is_empty() {
            return Ok(ABSENT);
        }
        unsafe {
            out.write(Output {
                size: pixels.size,
                origin: pixels.origin,
                bgra: Bytes::of(&pixels.bgra),
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

#[derive(Clone, Default)]
struct Raster {
    origin: [i32; 2],
    size: [u32; 2],
    bgra: Vec<u8>,
}

impl Raster {
    fn pixel(&self, x: i64, y: i64) -> [u8; 4] {
        let x = x - i64::from(self.origin[0]);
        let y = y - i64::from(self.origin[1]);
        if x < 0
            || y < 0
            || x >= i64::from(self.size[0])
            || y >= i64::from(self.size[1])
            || self.bgra.is_empty()
        {
            return [0; 4];
        }
        let index = 4 * (y as usize * self.size[0] as usize + x as usize);
        self.bgra[index..index + 4].try_into().unwrap()
    }

    fn blend(&mut self, from: &Self, to: &Self, weight: u32) -> bool {
        let bounds = |raster: &Self, axis: usize| {
            let min = i64::from(raster.origin[axis]);
            (min, min + i64::from(raster.size[axis]))
        };
        for axis in 0..2 {
            let (from_min, from_max) = bounds(from, axis);
            let (to_min, to_max) = bounds(to, axis);
            let (min, max) = if from.bgra.is_empty() {
                (to_min, to_max)
            } else if to.bgra.is_empty() {
                (from_min, from_max)
            } else {
                (from_min.min(to_min), from_max.max(to_max))
            };
            let side = max - min;
            if !(1..=i64::from(MAX_FRAME_SIDE)).contains(&side) {
                return false;
            }
            self.origin[axis] = min as i32;
            self.size[axis] = side as u32;
        }
        let [width, height] = self.size.map(|n| n as usize);
        self.bgra.resize(4 * width * height, 0);
        for (i, dst) in self.bgra.chunks_exact_mut(4).enumerate() {
            let x = i64::from(self.origin[0]) + (i % width) as i64;
            let y = i64::from(self.origin[1]) + (i / width) as i64;
            let (mut a, mut b) = (from.pixel(x, y), to.pixel(x, y));
            if a[3] == 0 {
                a[..3].copy_from_slice(&b[..3]);
            }
            if b[3] == 0 {
                b[..3].copy_from_slice(&a[..3]);
            }
            for channel in 0..4 {
                dst[channel] = ((u32::from(a[channel]) * (256 - weight)
                    + u32::from(b[channel]) * weight)
                    >> 8) as u8;
            }
        }
        true
    }
}

struct Fade {
    from: Raster,
    started: u32,
    duration: u32,
}

struct Model {
    playback: EmotePlayback,
    gpu: Box<dyn GpuBackend>,
    textures: BTreeMap<String, (TextureId, TextureInfo)>,
    expression: Differences,
    body: Differences,
    expression_selection: Selection,
    body_selection: Selection,
    difference_speed: Scale,
    difference_value: Scale,
    effects_mode: u32,
    differences_dirty: bool,
    difference_tick: Option<u32>,
    difference_values: Assignments,
    lip: lip::LipSync,
    lip_enabled: bool,
    lip_values: Assignments,
    applied: Assignments,
    keys: Vec<u8>,
    default_body_keys: Vec<u8>,
    action: Option<String>,
    raster: Raster,
    blended: Raster,
    fade: Option<Fade>,
    pending_fade: Option<u32>,
    pending_chain_fade: Option<u32>,
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
            expression_selection: Selection::default(),
            body_selection: Selection::default(),
            difference_speed: Scale::default(),
            difference_value: Scale::default(),
            effects_mode: 1,
            differences_dirty: true,
            difference_tick: None,
            difference_values: Assignments::new(),
            lip: lip::LipSync::default(),
            lip_enabled: true,
            lip_values: Assignments::new(),
            applied: Assignments::new(),
            keys: Vec::new(),
            default_body_keys: Vec::new(),
            action: None,
            raster: Raster::default(),
            blended: Raster::default(),
            fade: None,
            pending_fade: None,
            pending_chain_fade: None,
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

    fn effects_enabled(&self, settings: &SessionHost) -> bool {
        self.effects_mode != 0 && settings.difference_effects.load(Ordering::Relaxed) != 0
    }

    fn finish_differences(&mut self) {
        self.differences_dirty |=
            self.expression_selection.active() || self.body_selection.active();
        self.expression_selection.finish();
        self.body_selection.finish();
    }

    fn select(&mut self, options: &[u8], encoding: &'static Encoding, animate: bool) {
        self.keys.clear();
        self.keys.extend_from_slice(options);
        let options = encoding.decode(options).0;
        if let Some(duration) =
            self.expression_selection
                .select(&self.expression, &options, animate)
        {
            self.pending_chain_fade = Some(duration);
        }
        self.lip.select(&self.expression, &options);
        self.differences_dirty = true;
    }

    fn advance_differences(&mut self, time: u32, settings: &SessionHost) {
        if !self.effects_enabled(settings) {
            self.finish_differences();
            self.pending_chain_fade = None;
        }
        if !self.differences_dirty
            && (self.difference_tick == Some(time)
                || !(self.expression_selection.active() || self.body_selection.active()))
        {
            return;
        }
        self.differences_dirty = false;
        self.difference_tick = Some(time);
        self.difference_values = self
            .expression_selection
            .sample(time, &self.difference_speed);
        self.difference_values
            .extend(self.body_selection.sample(time, &self.difference_speed));
        for (label, value) in &mut self.difference_values {
            *value *= self.difference_value.get(label);
        }
        self.apply();
    }

    fn apply(&mut self) {
        let values: Assignments = self
            .difference_values
            .iter()
            .chain(&self.lip_values)
            .map(|(label, value)| (label.clone(), *value))
            .collect();
        if values == self.applied {
            return;
        }
        for (label, value) in &values {
            if self.applied.get(label) == Some(value) {
                continue;
            }
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

    fn output(&self) -> &Raster {
        if self.fade.is_some() {
            &self.blended
        } else {
            &self.raster
        }
    }

    fn advance_frame(&mut self, time: u32, fade_enabled: bool) -> Result<(), String> {
        if !fade_enabled {
            self.fade = None;
        }
        let chain_fade = self.pending_chain_fade.take();
        if let Some(duration) = self.pending_fade.take().or(chain_fade) {
            self.fade = if duration != 0 && fade_enabled && self.last_tick.is_some() {
                Some(Fade {
                    from: self.output().clone(),
                    started: time,
                    duration,
                })
            } else {
                None
            };
        }
        let lip_values = self.lip.sample(time);
        if lip_values != self.lip_values {
            self.lip_values = lip_values;
            self.apply();
        }
        self.render(time)?;
        if let Some(fade) = &self.fade {
            let elapsed = time.wrapping_sub(fade.started);
            if elapsed >= fade.duration
                || !self.blended.blend(
                    &fade.from,
                    &self.raster,
                    (elapsed as f32 / fade.duration as f32 * 256.0) as u32,
                )
            {
                self.fade = None;
            }
        }
        Ok(())
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
            self.raster.bgra.clear();
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
        if self.raster.size != size {
            self.gpu.resize(extent)?;
        }
        self.gpu.begin_frame(FrameTarget::Main)?;
        self.gpu.render_with_clear_color(&list, [0.; 4]);
        self.gpu.end_frame();
        self.raster
            .bgra
            .resize(extent.rgba8_len().ok_or("Invalid frame size")?, 0);
        let written = self
            .gpu
            .readback(FrameTarget::Main, extent, &mut self.raster.bgra)?;
        if written != self.raster.bgra.len() {
            return Err("Incomplete model readback".into());
        }
        for pixel in self.raster.bgra.chunks_exact_mut(4) {
            pixel.swap(0, 2);
            let alpha = u32::from(pixel[3]);
            if alpha > 0 && alpha < 255 {
                for c in &mut pixel[..3] {
                    *c = ((u32::from(*c) * 255 + alpha / 2) / alpha).min(255) as u8;
                }
            }
        }
        self.raster.size = size;
        self.raster.origin = origin;
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
