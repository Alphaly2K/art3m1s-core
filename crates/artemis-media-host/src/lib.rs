mod katze_movie;

use art3m1s_media::MediaSource;
use art3m1s_media::ffmpeg::{
    FfmpegVideoDecoder, HostPcmBlock, LegacyHostAudioDecoder, RgbaVideoFrame,
};
use std::ffi::{CStr, c_char};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::{Arc, Mutex};

struct FileSource {
    file: Mutex<File>,
    size: u64,
}

impl MediaSource for FileSource {
    fn len(&self) -> Result<u64, String> {
        Ok(self.size)
    }

    fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, String> {
        let mut file = self.file.lock().map_err(|error| error.to_string())?;
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.read(output))
            .map_err(|error| error.to_string())
    }
}

struct Video {
    decoder: FfmpegVideoDecoder,
    last_frame: Option<RgbaVideoFrame>,
}

struct Audio {
    decoder: LegacyHostAudioDecoder,
    last_block: Option<HostPcmBlock>,
}

#[repr(C)]
#[derive(Default)]
struct Frame {
    rgba: *const u8,
    bytes: u64,
    pts_us: u64,
    duration_us: u64,
    width: u32,
    height: u32,
}

#[repr(C)]
#[derive(Default)]
struct Pcm {
    stereo_i16: *const i16,
    pts_us: u64,
    frames: u32,
}

const INVALID: i32 = -1;
const DECODE_ERROR: i32 = -2;
const END: i32 = 0;
const FRAME: i32 = 1;

fn file_source(path: &CStr) -> Option<Arc<FileSource>> {
    let file = File::open(path.to_str().ok()?).ok()?;
    let meta = file.metadata().ok()?;
    meta.is_file().then(|| {
        Arc::new(FileSource {
            file: Mutex::new(file),
            size: meta.len(),
        })
    })
}

unsafe extern "C" fn open_video(path: *const c_char) -> *mut Video {
    if path.is_null() {
        return ptr::null_mut();
    }
    catch_unwind(|| {
        let source = file_source(unsafe { CStr::from_ptr(path) })?;
        let decoder = FfmpegVideoDecoder::open_legacy_host_video(source).ok()?;
        Some(Box::into_raw(Box::new(Video {
            decoder,
            last_frame: None,
        })))
    })
    .ok()
    .flatten()
    .unwrap_or(ptr::null_mut())
}

unsafe extern "C" fn next_video(video: *mut Video, output: *mut Frame) -> i32 {
    if video.is_null() || output.is_null() {
        return INVALID;
    }
    let video = unsafe { &mut *video };
    let output = unsafe { &mut *output };
    *output = Frame::default();
    video.last_frame = None;
    catch_unwind(AssertUnwindSafe(|| match video.decoder.next_rgba_frame() {
        Ok(Some(frame)) => {
            *output = Frame {
                rgba: frame.pixels.as_ptr(),
                bytes: frame.pixels.len() as u64,
                pts_us: frame.info.pts.micros(),
                duration_us: frame.info.duration.as_micros() as u64,
                width: frame.info.width,
                height: frame.info.height,
            };
            video.last_frame = Some(frame);
            FRAME
        }
        Ok(None) => {
            output.pts_us = video.decoder.implicit_hold_end_us();
            END
        }
        Err(_) => DECODE_ERROR,
    }))
    .unwrap_or(DECODE_ERROR)
}

unsafe extern "C" fn close_video(video: *mut Video) {
    if !video.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(unsafe { Box::from_raw(video) })));
    }
}

unsafe extern "C" fn open_audio(path: *const c_char, output: *mut *mut Audio) -> i32 {
    if path.is_null() || output.is_null() {
        return INVALID;
    }
    unsafe { *output = ptr::null_mut() };
    catch_unwind(AssertUnwindSafe(|| {
        let Some(source) = file_source(unsafe { CStr::from_ptr(path) }) else {
            return DECODE_ERROR;
        };
        match LegacyHostAudioDecoder::open(source) {
            Ok(Some(decoder)) => {
                unsafe {
                    *output = Box::into_raw(Box::new(Audio {
                        decoder,
                        last_block: None,
                    }))
                };
                FRAME
            }
            Ok(None) => END,
            Err(_) => DECODE_ERROR,
        }
    }))
    .unwrap_or(DECODE_ERROR)
}

unsafe extern "C" fn next_audio(audio: *mut Audio, output: *mut Pcm) -> i32 {
    if audio.is_null() || output.is_null() {
        return INVALID;
    }
    let audio = unsafe { &mut *audio };
    let output = unsafe { &mut *output };
    *output = Pcm::default();
    audio.last_block = None;
    catch_unwind(AssertUnwindSafe(|| match audio.decoder.next_pcm() {
        Ok(Some(block)) => {
            *output = Pcm {
                stereo_i16: block.stereo_i16.as_ptr(),
                pts_us: block.pts_us,
                frames: (block.stereo_i16.len() / 2) as u32,
            };
            audio.last_block = Some(block);
            FRAME
        }
        Ok(None) => END,
        Err(_) => DECODE_ERROR,
    }))
    .unwrap_or(DECODE_ERROR)
}

unsafe extern "C" fn close_audio(audio: *mut Audio) {
    if !audio.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(unsafe { Box::from_raw(audio) })));
    }
}

#[repr(C)]
pub struct MediaApiV1 {
    abi_version: u32,
    size: u32,
    open_video: unsafe extern "C" fn(*const c_char) -> *mut Video,
    next_video: unsafe extern "C" fn(*mut Video, *mut Frame) -> i32,
    close_video: unsafe extern "C" fn(*mut Video),
    open_audio: unsafe extern "C" fn(*const c_char, *mut *mut Audio) -> i32,
    next_audio: unsafe extern "C" fn(*mut Audio, *mut Pcm) -> i32,
    close_audio: unsafe extern "C" fn(*mut Audio),
}

static API: MediaApiV1 = MediaApiV1 {
    abi_version: 1,
    size: size_of::<MediaApiV1>() as u32,
    open_video,
    next_video,
    close_video,
    open_audio,
    next_audio,
    close_audio,
};

#[unsafe(no_mangle)]
pub extern "C" fn art3m1s_artemis_media_get_api_v1() -> *const MediaApiV1 {
    &API
}
