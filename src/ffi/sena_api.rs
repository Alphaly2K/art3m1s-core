//! Sena VM sessions. All calls and callbacks use the opening thread.
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Component, Path};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pal_asset::vfs::{HostFiles, with_host_files};
use pal_vm::scene::{FrameScene, rasterize_scene_rgba, rasterize_scene_rgba_2x};
use pal_vm::{Engine, EngineConfig, InputEvent, MouseButton, PalEvent, RuntimeStatus};

const INVALID: i32 = -1;
const SESSION: i32 = -2;
const ENGINE: i32 = -3;
const LIMIT: u32 = 4096;

#[repr(C)]
#[derive(Clone, Copy)]
struct Files {
    size: u32,
    context: *mut c_void,
    stat: Option<unsafe extern "C" fn(*mut c_void, *const c_char, *mut u64, *mut u64) -> i32>,
    read: Option<unsafe extern "C" fn(*mut c_void, *const c_char, u64, *mut u8, usize) -> i32>,
    write: Option<unsafe extern "C" fn(*mut c_void, *const c_char, *const u8, usize) -> i32>,
}

impl Files {
    fn key(path: &Path) -> io::Result<CString> {
        let mut parts = path.components();
        if parts.next() != Some(Component::Normal("project".as_ref())) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let names = parts
            .map(|part| match part {
                Component::Normal(name) => name.to_str().filter(|s| !s.contains('\\')),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()
            .ok_or(io::ErrorKind::InvalidInput)?;
        CString::new(names.join("/")).map_err(io::Error::other)
    }

    fn stat(&self, path: &Path) -> io::Result<(u64, u64)> {
        let key = Self::key(path)?;
        let mut len = 0;
        let mut modified = 0;
        match unsafe { self.stat.unwrap()(self.context, key.as_ptr(), &mut len, &mut modified) } {
            0 => Ok((len, modified)),
            1 => Err(io::ErrorKind::NotFound.into()),
            _ => Err(io::Error::other("host file stat failed")),
        }
    }

    fn write(&self, path: &Path, bytes: Option<&[u8]>) -> io::Result<()> {
        let key = Self::key(path)?;
        if !key.as_bytes().starts_with(b"save/") {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let (data, size) = bytes.map_or((std::ptr::null(), 0), |data| (data.as_ptr(), data.len()));
        if unsafe { self.write.unwrap()(self.context, key.as_ptr(), data, size) } == 0 {
            Ok(())
        } else {
            Err(io::Error::other("host save update failed"))
        }
    }
}

impl HostFiles for Files {
    fn size(&self, path: &Path) -> io::Result<u64> {
        self.stat(path).map(|(size, _)| size)
    }

    fn read(&self, path: &Path, offset: usize, output: &mut [u8]) -> io::Result<()> {
        let key = Self::key(path)?;
        match unsafe {
            self.read.unwrap()(
                self.context,
                key.as_ptr(),
                offset as u64,
                output.as_mut_ptr(),
                output.len(),
            )
        } {
            0 => Ok(()),
            1 => Err(io::ErrorKind::NotFound.into()),
            _ => Err(io::Error::other("host file read failed")),
        }
    }

    fn write_save(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.write(path, Some(bytes))
    }

    fn remove_save(&self, path: &Path) -> io::Result<()> {
        self.write(path, None)
    }

    fn modified(&self, path: &Path) -> io::Result<SystemTime> {
        UNIX_EPOCH
            .checked_add(Duration::from_millis(self.stat(path)?.1))
            .ok_or(io::ErrorKind::InvalidData.into())
    }
}

struct Runtime {
    files: Rc<Files>,
    engine: Engine,
    scene: Option<FrameScene>,
    device: Option<super::pcm_output::Output>,
}

thread_local! {
    static RUNTIMES: RefCell<HashMap<u64, Runtime>> = Default::default();
    static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}
static NEXT: AtomicU64 = AtomicU64::new(1);

fn failure(message: impl ToString) -> i32 {
    LAST_ERROR.with(|last| *last.borrow_mut() = message.to_string());
    ENGINE
}

fn guarded(f: impl FnOnce() -> i32) -> i32 {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| failure("Sena panicked"))
}

fn with_runtime(id: u64, f: impl FnOnce(&mut Runtime) -> i32) -> i32 {
    guarded(|| {
        RUNTIMES.with(|sessions| {
            let Ok(mut table) = sessions.try_borrow_mut() else {
                return SESSION;
            };
            let Some(runtime) = table.get_mut(&id) else {
                return SESSION;
            };
            with_host_files(runtime.files.clone(), || f(runtime))
        })
    })
}

unsafe extern "C" fn open(files: *const Files, output: *mut u64) -> i32 {
    guarded(|| {
        if files.is_null() || !files.is_aligned() || output.is_null() || !output.is_aligned() {
            return INVALID;
        }
        unsafe { output.write(0) };
        if unsafe { files.cast::<u32>().read() } < size_of::<Files>() as u32 {
            return INVALID;
        }
        let files = Rc::new(unsafe { files.read() });
        if files.stat.is_none() || files.read.is_none() || files.write.is_none() {
            return INVALID;
        }
        let engine = with_host_files(files.clone(), || {
            Engine::new(EngineConfig {
                game_root: Some("project".into()),
                ..Default::default()
            })
        });
        let mut engine = match engine {
            Ok(engine) => engine,
            Err(error) => return failure(format!("{error:#}")),
        };
        let (width, height) = engine.logical_size_from_config(1280, 720);
        engine.handle_event(PalEvent::Resized { width, height });
        if !(1..=LIMIT).contains(&width) || !(1..=LIMIT).contains(&height) {
            return failure("Unsupported Sena stage extent");
        }
        let Ok(id) = NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
        else {
            return ENGINE;
        };
        RUNTIMES.with(|sessions| {
            sessions.borrow_mut().insert(
                id,
                Runtime {
                    files,
                    engine,
                    scene: None,
                    device: None,
                },
            )
        });
        unsafe { output.write(id) };
        0
    })
}

extern "C" fn close(id: u64) -> i32 {
    guarded(|| {
        RUNTIMES.with(|sessions| {
            let Ok(mut table) = sessions.try_borrow_mut() else {
                return SESSION;
            };
            let Some(runtime) = table.remove(&id) else {
                return SESSION;
            };
            with_host_files(runtime.files.clone(), || drop(runtime));
            0
        })
    })
}

unsafe extern "C" fn tick(id: u64, delta_ms: u32, width: *mut u32, height: *mut u32) -> i32 {
    if width.is_null() || height.is_null() || !width.is_aligned() || !height.is_aligned() {
        return INVALID;
    }
    with_runtime(id, |rt| {
        rt.engine.handle_event(PalEvent::RedrawRequested);
        let frame = match rt
            .engine
            .update_with_delta(Duration::from_millis(delta_ms.clamp(1, 250).into()))
        {
            Ok(frame) => frame,
            Err(error) => return failure(format!("{error:#}")),
        };
        rt.engine.input_begin_frame();
        if let Some(device) = rt.device.as_ref() {
            let frames = device.capacity().min(delta_ms.clamp(1, 100) as usize * 48);
            if frames > 0 {
                let mut pcm = vec![0; frames * 2];
                rt.engine.audio().host_sink().render_i16(&mut pcm);
                let samples: Vec<_> = pcm
                    .into_iter()
                    .map(|raw| f32::from(raw) / 32768.0)
                    .collect();
                device.append(&samples);
            }
        }
        let w = frame.scene.logical_width;
        let h = frame.scene.logical_height;
        if !(1..=LIMIT).contains(&w) || !(1..=LIMIT).contains(&h) {
            return failure("Unsupported Sena frame extent");
        }
        unsafe {
            width.write(w);
            height.write(h)
        };
        let done = match frame.runtime_status {
            RuntimeStatus::Faulted { .. }
            | RuntimeStatus::UnsupportedCommand { .. }
            | RuntimeStatus::UnsupportedExtCall { .. } => return failure(frame.runtime_status),
            RuntimeStatus::Halted { .. } => true,
            _ => false,
        };
        rt.scene = Some(frame.scene);
        i32::from(done)
    })
}

unsafe extern "C" fn rgba(id: u64, out: *mut u8, capacity: usize) -> i32 {
    copy_rgba(id, out, capacity, false)
}

fn copy_rgba(id: u64, out: *mut u8, capacity: usize, dense: bool) -> i32 {
    if out.is_null() {
        return INVALID;
    }
    with_runtime(id, |rt| {
        let Some(scene) = &rt.scene else { return 1 };
        if dense && (scene.logical_width > LIMIT / 2 || scene.logical_height > LIMIT / 2) {
            return INVALID;
        }
        let required = scene.logical_width as usize
            * scene.logical_height as usize
            * if dense { 16 } else { 4 };
        if capacity < required {
            return INVALID;
        }
        let bytes = if dense {
            rt.engine.enable_dense_text();
            rasterize_scene_rgba_2x(scene)
        } else {
            rasterize_scene_rgba(scene)
        };
        if bytes.len() != required {
            return ENGINE;
        }
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, required) };
        0
    })
}

unsafe extern "C" fn rgba_2x(id: u64, out: *mut u8, capacity: usize) -> i32 {
    copy_rgba(id, out, capacity, true)
}

unsafe extern "C" fn pcm(id: u64, out: *mut i16, frames: usize) -> i32 {
    if out.is_null() || !out.is_aligned() || frames > 4800 {
        return INVALID;
    }
    with_runtime(id, |rt| {
        rt.engine
            .audio()
            .host_sink()
            .render_i16(unsafe { std::slice::from_raw_parts_mut(out, frames * 2) });
        0
    })
}

unsafe extern "C" fn input(
    id: u64,
    kind: u32,
    code: i32,
    x: i32,
    y: i32,
    key: *const c_char,
) -> i32 {
    with_runtime(id, |rt| {
        let event = match kind {
            0 => InputEvent::CursorMoved {
                x: x.into(),
                y: y.into(),
            },
            1 | 2 => {
                let button = match code {
                    0 => MouseButton::Left,
                    1 => MouseButton::Right,
                    2 => MouseButton::Middle,
                    _ => return INVALID,
                };
                InputEvent::MouseInput {
                    button,
                    pressed: kind == 1,
                }
            }
            3 | 4 if !key.is_null() => {
                let Ok(name) = (unsafe { CStr::from_ptr(key) }).to_str() else {
                    return INVALID;
                };
                InputEvent::Keyboard {
                    key_name: name.into(),
                    pressed: kind == 3,
                }
            }
            5 => InputEvent::MouseWheel {
                delta_x: x as f32,
                delta_y: y as f32,
            },
            _ => return INVALID,
        };
        rt.engine.handle_event(PalEvent::Input(event));
        0
    })
}

unsafe extern "C" fn last_error(out: *mut u8, capacity: usize) -> usize {
    LAST_ERROR.with(|last| {
        let last = last.borrow();
        if out.is_null() {
            return last.len();
        }
        let len = capacity.min(last.len());
        unsafe { std::ptr::copy_nonoverlapping(last.as_ptr(), out, len) };
        len
    })
}

extern "C" fn device_audio(id: u64, action: u32) -> i32 {
    with_runtime(id, |rt| {
        match action {
            0 if rt.device.is_none() => match super::pcm_output::Output::start() {
                Ok(device) => rt.device = Some(device),
                Err(error) => return failure(error),
            },
            0 => {}
            1 => rt.device = None,
            2 | 3 => {
                if let Some(device) = &rt.device {
                    device.suspend(action == 2);
                }
            }
            _ => return INVALID,
        }
        0
    })
}

#[repr(C)]
pub struct SenaApi {
    size: u32,
    version: u32,
    open: unsafe extern "C" fn(*const Files, *mut u64) -> i32,
    close: extern "C" fn(u64) -> i32,
    tick: unsafe extern "C" fn(u64, u32, *mut u32, *mut u32) -> i32,
    rgba: unsafe extern "C" fn(u64, *mut u8, usize) -> i32,
    pcm: unsafe extern "C" fn(u64, *mut i16, usize) -> i32,
    input: unsafe extern "C" fn(u64, u32, i32, i32, i32, *const c_char) -> i32,
    last_error: unsafe extern "C" fn(*mut u8, usize) -> usize,
    device_audio: extern "C" fn(u64, u32) -> i32,
    rgba_2x: unsafe extern "C" fn(u64, *mut u8, usize) -> i32,
}

static API: SenaApi = SenaApi {
    size: size_of::<SenaApi>() as u32,
    version: 1,
    open,
    close,
    tick,
    rgba,
    pcm,
    input,
    last_error,
    device_audio,
    rgba_2x,
};

#[unsafe(no_mangle)]
pub extern "C" fn art3m1s_sena_get_api_v1() -> *const SenaApi {
    &API
}

#[cfg(test)]
mod tests {
    use super::*;

    type Project = HashMap<String, Vec<u8>>;

    unsafe extern "C" fn stat(
        context: *mut c_void,
        key: *const c_char,
        size: *mut u64,
        modified: *mut u64,
    ) -> i32 {
        let project = unsafe { &*context.cast::<Project>() };
        let key = unsafe { CStr::from_ptr(key) }.to_str().unwrap();
        let Some(bytes) = project.get(key) else {
            return 1;
        };
        unsafe {
            size.write(bytes.len() as u64);
            modified.write(0)
        };
        0
    }

    unsafe extern "C" fn read(
        context: *mut c_void,
        key: *const c_char,
        offset: u64,
        out: *mut u8,
        len: usize,
    ) -> i32 {
        let project = unsafe { &*context.cast::<Project>() };
        let key = unsafe { CStr::from_ptr(key) }.to_str().unwrap();
        let Some(bytes) = project.get(key) else {
            return 1;
        };
        let offset = offset as usize;
        let Some(range) = bytes.get(offset..offset.saturating_add(len)) else {
            return -1;
        };
        unsafe { std::ptr::copy_nonoverlapping(range.as_ptr(), out, len) };
        0
    }

    unsafe extern "C" fn write(
        context: *mut c_void,
        key: *const c_char,
        bytes: *const u8,
        len: usize,
    ) -> i32 {
        let project = unsafe { &mut *context.cast::<Project>() };
        let key = unsafe { CStr::from_ptr(key) }.to_str().unwrap();
        if bytes.is_null() {
            project.remove(key);
        } else {
            project.insert(
                key.into(),
                unsafe { std::slice::from_raw_parts(bytes, len) }.to_vec(),
            );
        }
        0
    }

    #[test]
    fn callback_project_delivers_pixels_and_uses_session_owned_save_storage() {
        let mut script = b"Sv20".to_vec();
        script.extend([0u32, 12, 0x10015].into_iter().flat_map(u32::to_le_bytes));
        let mut project = Project::from([
            ("Script.src".into(), script),
            ("archive.dat".into(), b"data".to_vec()),
            (
                "SYSTEM.INI".into(),
                b"[GRAPHICS]\nDEF_CG_WIDTH=320\nDEF_CG_HEIGHT=240\n".to_vec(),
            ),
            ("File.dat".into(), Vec::new()),
            ("Text.dat".into(), Vec::new()),
            ("Mem.dat".into(), Vec::new()),
            ("Point.dat".into(), Vec::new()),
        ]);
        let files = Files {
            size: size_of::<Files>() as u32,
            context: (&mut project as *mut Project).cast(),
            stat: Some(stat),
            read: Some(read),
            write: Some(write),
        };
        let mut id = 0;
        assert_eq!(
            unsafe { (API.open)(&files, &mut id) },
            0,
            "{:?}",
            LAST_ERROR.with(|s| s.borrow().clone())
        );
        assert_eq!(
            std::thread::spawn(move || close(id)).join().unwrap(),
            SESSION
        );
        let (mut width, mut height) = (0, 0);
        assert_eq!(unsafe { (API.tick)(id, 16, &mut width, &mut height) }, 1);
        assert_eq!((width, height), (320, 240));
        let mut pixels = vec![0; width as usize * height as usize * 4];
        assert_eq!(
            unsafe { (API.rgba)(id, pixels.as_mut_ptr(), pixels.len()) },
            0
        );
        assert!(pixels.chunks_exact(4).all(|p| p[3] == 255));
        assert_eq!(
            unsafe { (API.rgba_2x)(id, pixels.as_mut_ptr(), pixels.len()) },
            INVALID
        );
        let mut dense = vec![0; pixels.len() * 4];
        assert_eq!(
            unsafe { (API.rgba_2x)(id, dense.as_mut_ptr(), dense.len()) },
            0
        );
        assert!(dense.chunks_exact(4).all(|p| p[3] == 255));
        let mut silence = [1i16; 32];
        assert_eq!(
            unsafe { (API.pcm)(id, silence.as_mut_ptr(), silence.len() / 2) },
            0
        );
        assert!(silence.iter().all(|pcm| *pcm == 0));
        assert_eq!(
            with_runtime(id, |_| {
                let key = Path::new("project/save/checkpoint");
                pal_asset::vfs::write_save(key, b"session save").unwrap();
                assert_eq!(pal_asset::vfs::read_all(key).unwrap(), b"session save");
                assert!(
                    pal_asset::vfs::write_save(Path::new("project/Script.src"), b"ignored")
                        .is_err()
                );
                0
            }),
            0
        );
        assert_eq!(close(id), 0);
        assert_eq!(close(id), SESSION);
        assert_eq!(project["save/checkpoint"], b"session save");
    }
}
