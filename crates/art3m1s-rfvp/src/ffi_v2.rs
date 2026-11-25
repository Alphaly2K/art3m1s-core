//! C ABI for RFVP sessions that publish replayable frames.

use std::collections::HashMap;
use std::mem::{align_of, size_of};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{LazyLock, Mutex, TryLockError};
use std::thread::{self, JoinHandle};

use rfvp::host_api::{
    CommandBlendMode, RectI16, RenderCommand, Rgba8, TextureFilter, TextureFormat,
};

use crate::{RfvpFrameRuntime, RfvpHostRuntimeError, RfvpNls, RfvpReplayError, RfvpReplayFrame};

pub mod input;
use input::{Art3m1sRfvpInputEventV1, convert_input_event};

const MAGIC: u64 = 0x3256_4652_4d33_4152;
const VERSION: u32 = 2;
const OK: i32 = 0;
const NO_FRAME: i32 = 1;
const ARGUMENT: i32 = -1;
const HANDLE: i32 = -2;
const ENGINE: i32 = -3;
const VERSION_ERROR: i32 = -4;
const BUSY: i32 = -5;
const LIMIT: i32 = -6;
const MAX_FRAMES: usize = 8;
const MAX_INPUT: u32 = 256;
const MAX_PATH: u64 = 32 * 1024;

#[repr(C)]
pub struct Bytes {
    data: *const u8,
    len: u64,
}

#[repr(C)]
pub struct OpenInfo {
    struct_size: u32,
    nls: u32,
    project: Bytes,
    saves: Bytes,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Rect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl From<RectI16> for Rect {
    fn from(rect: RectI16) -> Self {
        Self {
            x: rect.x.into(),
            y: rect.y.into(),
            width: rect.w.into(),
            height: rect.h.into(),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Vertex {
    x: f32,
    y: f32,
    u: f32,
    v: f32,
    rgba: [f32; 4],
}

#[repr(C)]
#[derive(Default)]
pub struct Draw {
    kind: u32,
    texture: u32,
    blend: u32,
    filter: u32,
    has_clip: u32,
    effect: u32,
    source: Rect,
    destination: Rect,
    clip: Rect,
    rgba: [u8; 4],
    vertices: [Vertex; 4],
}

#[repr(C)]
pub struct HitProxy {
    prim_id: u32,
    flags: u32,
    bounds: Rect,
    order: u32,
}

#[repr(C)]
pub struct FrameView {
    width: u32,
    height: u32,
    draw_count: u32,
    hit_count: u32,
    texture_count: u32,
    reserved: u32,
    draws: *const Draw,
    hits: *const HitProxy,
}

#[repr(C)]
pub struct TextureView {
    texture: u32,
    format: u32,
    width: u32,
    height: u32,
    generation: u64,
    pixels: Bytes,
}

#[repr(C)]
pub struct Api {
    pub struct_size: u32,
    pub abi_major: u32,
    pub magic: u64,
    pub open_directory: unsafe extern "C" fn(*const OpenInfo, *mut u64) -> i32,
    pub close: extern "C" fn(u64) -> i32,
    pub push_input: unsafe extern "C" fn(u64, *const Art3m1sRfvpInputEventV1, u32) -> i32,
    pub step: unsafe extern "C" fn(u64, u32, *mut u32) -> i32,
    pub acquire_frame: unsafe extern "C" fn(u64, *mut u64) -> i32,
    pub frame_view: unsafe extern "C" fn(u64, *mut FrameView) -> i32,
    pub frame_texture: unsafe extern "C" fn(u64, u32, *mut TextureView) -> i32,
    pub release_frame: extern "C" fn(u64) -> i32,
}

static API: Api = Api {
    struct_size: size_of::<Api>() as u32,
    abi_major: VERSION,
    magic: MAGIC,
    open_directory,
    close,
    push_input,
    step,
    acquire_frame,
    frame_view,
    frame_texture,
    release_frame,
};

/// # Safety
/// `out` must point to writable storage for a table pointer.
pub unsafe fn get_api(major: u32, min_size: u32, out: *mut *const Api) -> i32 {
    status(|| {
        unsafe { write(out, ptr::null())? };
        if major != VERSION || min_size > API.struct_size {
            return Err(VERSION_ERROR);
        }
        unsafe { write(out, &API) }
    })
}

type Job = Box<dyn FnOnce(&mut RfvpFrameRuntime) + Send>;

struct Session {
    handle: u64,
    jobs: SyncSender<Job>,
    worker: JoinHandle<()>,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);
static FRAMES: LazyLock<Mutex<HashMap<u64, Frame>>> = LazyLock::new(Default::default);
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

fn next_handle(frame: bool) -> Result<u64, i32> {
    let value = NEXT_HANDLE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| {
            (id < u64::MAX / 2).then(|| id + 1)
        })
        .map_err(|_| LIMIT)?;
    Ok((value << 1) | u64::from(frame))
}

impl Session {
    fn run<R: Send + 'static>(
        &self,
        job: impl FnOnce(&mut RfvpFrameRuntime) -> Result<R, i32> + Send + 'static,
    ) -> Result<R, i32> {
        let (reply, result) = mpsc::sync_channel(1);
        self.jobs
            .send(Box::new(move |runtime| {
                let _ = reply.send(job(runtime));
            }))
            .map_err(|_| ENGINE)?;
        result.recv().map_err(|_| ENGINE)?
    }
}

fn session_lock() -> Result<std::sync::MutexGuard<'static, Option<Session>>, i32> {
    match SESSION.try_lock() {
        Ok(session) => Ok(session),
        Err(TryLockError::WouldBlock) => Err(BUSY),
        Err(TryLockError::Poisoned(_)) => Err(ENGINE),
    }
}

fn with_session<R>(handle: u64, call: impl FnOnce(&Session) -> Result<R, i32>) -> Result<R, i32> {
    let session = session_lock()?;
    call(
        session
            .as_ref()
            .filter(|session| session.handle == handle)
            .ok_or(HANDLE)?,
    )
}

unsafe extern "C" fn open_directory(info: *const OpenInfo, out: *mut u64) -> i32 {
    status(|| {
        unsafe { write(out, 0)? };
        check_pointer(info)?;
        if unsafe { info.cast::<u32>().read() } < size_of::<OpenInfo>() as u32 {
            return Err(ARGUMENT);
        }
        let info = unsafe { &*info };
        let project = unsafe { path(&info.project)? }.ok_or(ARGUMENT)?;
        let saves = unsafe { path(&info.saves)? };
        let nls = match info.nls {
            1 => RfvpNls::ShiftJis,
            2 => RfvpNls::Gbk,
            3 => RfvpNls::Utf8,
            _ => return Err(ARGUMENT),
        };
        let mut session = session_lock()?;
        if session.is_some() {
            return Err(BUSY);
        }
        let handle = next_handle(false)?;
        let (jobs, queue) = mpsc::sync_channel::<Job>(0);
        let (ready, start) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("rfvp-session".into())
            .spawn(move || {
                match RfvpFrameRuntime::new_directory(
                    &project,
                    saves.as_deref().map(Path::new),
                    nls,
                ) {
                    Ok(mut runtime) => {
                        if ready.send(OK).is_ok() {
                            for job in queue {
                                job(&mut runtime);
                            }
                        }
                    }
                    Err(error) => {
                        let _ = ready.send(runtime_error(error));
                    }
                }
            })
            .map_err(|_| ENGINE)?;
        let start = start.recv().unwrap_or(ENGINE);
        if start != OK {
            let _ = worker.join();
            return Err(start);
        }
        *session = Some(Session {
            handle,
            jobs,
            worker,
        });
        unsafe { write(out, handle) }
    })
}

extern "C" fn close(handle: u64) -> i32 {
    status(|| {
        let mut current = session_lock()?;
        if current
            .as_ref()
            .is_none_or(|session| session.handle != handle)
        {
            return Err(HANDLE);
        }
        let Session { jobs, worker, .. } = current.take().unwrap();
        drop(jobs);
        let _ = worker.join();
        Ok(())
    })
}

unsafe extern "C" fn push_input(
    handle: u64,
    input: *const Art3m1sRfvpInputEventV1,
    count: u32,
) -> i32 {
    status(|| {
        if count > MAX_INPUT {
            return Err(LIMIT);
        }
        let inputs = if count == 0 {
            Vec::new()
        } else {
            check_pointer(input)?;
            unsafe { std::slice::from_raw_parts(input, count as usize) }
                .iter()
                .map(convert_input_event)
                .collect::<Result<Vec<_>, _>>()?
        };
        with_session(handle, |session| {
            session.run(move |runtime| runtime.push_input(&inputs).map_err(runtime_error))
        })
    })
}

unsafe extern "C" fn step(handle: u64, delta_ms: u32, flags: *mut u32) -> i32 {
    status(|| {
        unsafe { write(flags, 0)? };
        let result = with_session(handle, |session| {
            session.run(move |runtime| {
                runtime.step(delta_ms).map_err(runtime_error)?;
                Ok(u32::from(runtime.is_exit_requested()))
            })
        })?;
        unsafe { write(flags, result) }
    })
}

struct Frame {
    source: std::sync::Arc<RfvpReplayFrame>,
    draws: Box<[Draw]>,
    hits: Box<[HitProxy]>,
}

impl Frame {
    fn new(source: std::sync::Arc<RfvpReplayFrame>) -> Result<Self, i32> {
        let render = &source.frame;
        for count in [
            render.commands.len(),
            render.hit_proxies.proxies.len(),
            source.textures.len(),
        ] {
            u32::try_from(count).map_err(|_| LIMIT)?;
        }
        let draws = render.commands.iter().map(draw).collect();
        let hits = render
            .hit_proxies
            .proxies
            .iter()
            .map(|hit| HitProxy {
                prim_id: u32::from(hit.prim_id.0),
                flags: u32::from(hit.enabled) | (u32::from(hit.visible) << 1),
                bounds: hit.rect.into(),
                order: hit.order,
            })
            .collect();
        Ok(Self {
            source,
            draws,
            hits,
        })
    }
}

unsafe extern "C" fn acquire_frame(handle: u64, out: *mut u64) -> i32 {
    status(|| {
        unsafe { write(out, 0)? };
        let source = with_session(handle, |session| {
            session.run(|runtime| runtime.acquire_frame().ok_or(NO_FRAME))
        })?;
        let mut frames = FRAMES.lock().map_err(|_| ENGINE)?;
        if frames.len() >= MAX_FRAMES {
            return Err(LIMIT);
        }
        let lease = next_handle(true)?;
        frames.insert(lease, Frame::new(source)?);
        unsafe { write(out, lease) }
    })
}

unsafe extern "C" fn frame_view(lease: u64, out: *mut FrameView) -> i32 {
    status(|| {
        check_pointer(out)?;
        let frames = FRAMES.lock().map_err(|_| ENGINE)?;
        let frame = frames.get(&lease).ok_or(HANDLE)?;
        unsafe {
            write(
                out,
                FrameView {
                    width: frame.source.width,
                    height: frame.source.height,
                    draw_count: frame.draws.len() as u32,
                    hit_count: frame.hits.len() as u32,
                    texture_count: frame.source.textures.len() as u32,
                    reserved: 0,
                    draws: slice_ptr(&frame.draws),
                    hits: slice_ptr(&frame.hits),
                },
            )
        }
    })
}

unsafe extern "C" fn frame_texture(lease: u64, index: u32, out: *mut TextureView) -> i32 {
    status(|| {
        check_pointer(out)?;
        let frames = FRAMES.lock().map_err(|_| ENGINE)?;
        let frame = frames.get(&lease).ok_or(HANDLE)?;
        let texture = frame.source.textures.get(index as usize).ok_or(ARGUMENT)?;
        let format = match texture.desc.format {
            TextureFormat::Rgba8 => 1,
            TextureFormat::LumaA8 => 2,
            _ => return Err(ENGINE),
        };
        unsafe {
            write(
                out,
                TextureView {
                    texture: texture.handle.0,
                    format,
                    width: texture.desc.width.into(),
                    height: texture.desc.height.into(),
                    generation: texture.generation,
                    pixels: Bytes {
                        data: slice_ptr(&texture.pixels),
                        len: texture.pixels.len() as u64,
                    },
                },
            )
        }
    })
}

extern "C" fn release_frame(lease: u64) -> i32 {
    status(|| {
        let frame = FRAMES.lock().map_err(|_| ENGINE)?.remove(&lease);
        frame.map(drop).ok_or(HANDLE)
    })
}

fn draw(command: &RenderCommand) -> Draw {
    let rgba = |value: Rgba8| [value.r, value.g, value.b, value.a];
    let source = |rect: rfvp::host_api::RectU16| Rect {
        x: rect.x.into(),
        y: rect.y.into(),
        width: rect.w.into(),
        height: rect.h.into(),
    };
    match command {
        RenderCommand::DrawImage(image) => Draw {
            kind: 1,
            texture: image.texture.0,
            blend: match image.blend {
                CommandBlendMode::Normal => 0,
                CommandBlendMode::Add => 1,
                CommandBlendMode::Sub => 2,
                CommandBlendMode::Mul => 3,
            },
            filter: u32::from(image.filter == TextureFilter::Linear),
            has_clip: u32::from(image.clip.is_some()),
            effect: image.effect_id.into(),
            source: source(image.src),
            destination: image.dst.into(),
            clip: image.clip.unwrap_or_default().into(),
            rgba: rgba(image.color),
            vertices: image.vertices.map(|v| Vertex {
                x: v.position[0],
                y: v.position[1],
                u: v.tex_coord[0],
                v: v.tex_coord[1],
                rgba: [v.color.r, v.color.g, v.color.b, v.color.a],
            }),
        },
        RenderCommand::DrawGlyph(glyph) => Draw {
            kind: 2,
            texture: glyph.texture.0,
            filter: 1,
            has_clip: u32::from(glyph.clip.is_some()),
            source: source(glyph.src),
            destination: glyph.dst.into(),
            clip: glyph.clip.unwrap_or_default().into(),
            rgba: rgba(glyph.color),
            ..Draw::default()
        },
        RenderCommand::SetClip(clip) => Draw {
            kind: 3,
            clip: (*clip).into(),
            ..Draw::default()
        },
        RenderCommand::ClearClip => Draw {
            kind: 4,
            ..Draw::default()
        },
    }
}

fn status(call: impl FnOnce() -> Result<(), i32>) -> i32 {
    catch_unwind(AssertUnwindSafe(call))
        .unwrap_or(Err(ENGINE))
        .map_or_else(|code| code, |()| OK)
}

fn runtime_error(error: RfvpReplayError) -> i32 {
    match error {
        RfvpReplayError::Runtime(RfvpHostRuntimeError::SessionBusy) => BUSY,
        _ => ENGINE,
    }
}

fn check_pointer<T>(value: *const T) -> Result<(), i32> {
    if value.is_null() || !value.addr().is_multiple_of(align_of::<T>()) {
        return Err(ARGUMENT);
    }
    Ok(())
}

unsafe fn write<T>(out: *mut T, value: T) -> Result<(), i32> {
    check_pointer(out)?;
    unsafe { out.write(value) };
    Ok(())
}

unsafe fn path(bytes: &Bytes) -> Result<Option<String>, i32> {
    if bytes.len == 0 {
        return Ok(None);
    }
    if bytes.len > MAX_PATH {
        return Err(LIMIT);
    }
    check_pointer(bytes.data)?;
    let bytes = unsafe { std::slice::from_raw_parts(bytes.data, bytes.len as usize) };
    let value = std::str::from_utf8(bytes).map_err(|_| ARGUMENT)?;
    if value.contains('\0') {
        return Err(ARGUMENT);
    }
    Ok(Some(value.to_owned()))
}

fn slice_ptr<T>(items: &[T]) -> *const T {
    if items.is_empty() {
        ptr::null()
    } else {
        items.as_ptr()
    }
}
