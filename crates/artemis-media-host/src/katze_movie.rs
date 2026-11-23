use art3m1s_media::{
    MediaSource,
    ffmpeg::{FfmpegVideoDecoder, HostPcmBlock, LegacyHostAudioDecoder, RgbaVideoFrame},
};
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
    sync::Arc,
    thread::{self, ThreadId},
};

const OK: i32 = 0;
const ABSENT: i32 = 1;
const INVALID: i32 = -1;
const ERROR: i32 = -5;
const SAMPLE_RATE: u32 = 48_000;
const MICROS_PER_SECOND: f64 = 1_000_000.;

// These types match KatzeMovieHost and its arguments in the CatSystem2 ABI 0 header.
#[repr(C)]
struct MovieInput {
    len: u64,
    context: *const c_void,
    read_at: Option<unsafe extern "C" fn(*const c_void, u64, *mut u8, usize, *mut usize) -> i32>,
}

#[repr(C)]
struct Metadata {
    width: u32,
    height: u32,
    sample_rate: u32,
    frame_rate: f64,
    duration: f64,
}

#[repr(C)]
#[derive(Default)]
struct Bytes {
    data: *const u8,
    len: usize,
}

#[repr(C)]
#[derive(Default)]
struct Output {
    decoded_until: f64,
    video_time: f64,
    audio_time: f64,
    video: Bytes,
    pcm: *const f32,
    pcm_samples: usize,
    ended: u32,
}

#[repr(C)]
struct MovieHost {
    size: u32,
    reserved: u32,
    context: u64,
    open: unsafe extern "C" fn(u64, *const MovieInput, *mut u64, *mut Metadata) -> i32,
    advance: unsafe extern "C" fn(u64, u64, f64, *mut Output) -> i32,
    rewind: unsafe extern "C" fn(u64, u64) -> i32,
    close: unsafe extern "C" fn(u64, u64),
}

struct Source {
    len: u64,
    context: usize,
    read_at: unsafe extern "C" fn(*const c_void, u64, *mut u8, usize, *mut usize) -> i32,
    session: ThreadId,
}

impl MediaSource for Source {
    fn len(&self) -> Result<u64, String> {
        Ok(self.len)
    }

    fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, String> {
        if thread::current().id() != self.session {
            return Err("Katze movie source requires its session thread".into());
        }
        let limit = output.len().min(
            self.len
                .saturating_sub(offset)
                .try_into()
                .unwrap_or(usize::MAX),
        );
        if limit == 0 {
            return Ok(0);
        }
        let mut count = 0;
        // The movie source belongs to the session and remains valid until close returns.
        let status = unsafe {
            (self.read_at)(
                self.context as *const c_void,
                offset,
                output.as_mut_ptr(),
                limit,
                &mut count,
            )
        };
        if status == OK && count != 0 && count <= limit {
            Ok(count)
        } else {
            Err("Katze movie source read failed".into())
        }
    }
}

struct Movie {
    source: Arc<Source>,
    video: FfmpegVideoDecoder,
    audio: Option<LegacyHostAudioDecoder>,
    time: f64,
    next_video: Option<RgbaVideoFrame>,
    next_audio: Option<HostPcmBlock>,
    video_ended: bool,
    pixels: Vec<u8>,
    pcm: Vec<f32>,
}

impl Movie {
    fn open(source: Arc<Source>) -> Result<Self, String> {
        let video = FfmpegVideoDecoder::open_webm(source.clone())?;
        let audio = LegacyHostAudioDecoder::open_webm(source.clone())?;
        Ok(Self {
            source,
            video,
            audio,
            time: 0.,
            next_video: None,
            next_audio: None,
            video_ended: false,
            pixels: Vec::new(),
            pcm: Vec::new(),
        })
    }

    fn metadata(&self) -> Metadata {
        let info = self.video.info();
        Metadata {
            width: info.width,
            height: info.height,
            sample_rate: if self.audio.is_some() { SAMPLE_RATE } else { 0 },
            frame_rate: info
                .frame_rate
                .map_or(30., |(num, den)| f64::from(num) / f64::from(den)),
            duration: info.duration.as_secs_f64(),
        }
    }

    fn advance(&mut self, seconds: f64) -> Result<Output, String> {
        self.time += seconds;
        let due_us = (self.time * MICROS_PER_SECOND).round() as u64;
        let mut output = Output {
            decoded_until: self.time,
            ..Output::default()
        };
        let mut video = None;
        while !self.video_ended {
            if self.next_video.is_none() {
                self.next_video = self.video.next_rgba_frame()?;
            }
            let Some(next) = &self.next_video else {
                self.video_ended = true;
                break;
            };
            if next.info.pts.micros() > due_us {
                break;
            }
            video = self.next_video.take();
        }
        if let Some(mut video) = video {
            for pixel in video.pixels.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            self.pixels = video.pixels;
            output.video_time = video.info.pts.micros() as f64 / MICROS_PER_SECOND;
            output.video = Bytes {
                data: self.pixels.as_ptr(),
                len: self.pixels.len(),
            };
        }

        self.pcm.clear();
        let mut start_frame = None;
        while let Some(audio) = &mut self.audio {
            if self.next_audio.is_none() {
                self.next_audio = audio.next_pcm()?;
            }
            let Some(next) = &self.next_audio else {
                self.audio = None;
                break;
            };
            if next.pts_us > due_us {
                break;
            }
            let block = self.next_audio.take().unwrap();
            let frame = (u128::from(block.pts_us) * u128::from(SAMPLE_RATE) + 500_000) / 1_000_000;
            let start = *start_frame.get_or_insert(frame);
            let offset = usize::try_from(frame.saturating_sub(start) * 2)
                .map_err(|_| "Katze movie audio timestamp is out of range")?;
            if offset.saturating_add(block.stereo_i16.len()) > 16_777_216 {
                return Err("Katze movie audio step is too large".into());
            }
            let overlap = self
                .pcm
                .len()
                .saturating_sub(offset)
                .min(block.stereo_i16.len());
            if self.pcm.len() < offset {
                self.pcm.resize(offset, 0.);
            }
            self.pcm.extend(
                block.stereo_i16[overlap..]
                    .iter()
                    .map(|&sample| f32::from(sample) / 32768.),
            );
        }
        if let Some(start) = start_frame {
            output.audio_time = start as f64 / f64::from(SAMPLE_RATE);
            output.pcm = self.pcm.as_ptr();
            output.pcm_samples = self.pcm.len();
        }
        output.ended = u32::from(self.video_ended && self.audio.is_none());
        Ok(output)
    }
}

fn report(operation: impl FnOnce() -> Result<i32, String>) -> i32 {
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            eprintln!("Katze movie: {error}");
            ERROR
        }
        Err(_) => ERROR,
    }
}

unsafe extern "C" fn open(
    _: u64,
    input: *const MovieInput,
    token: *mut u64,
    metadata: *mut Metadata,
) -> i32 {
    if input.is_null() || token.is_null() || metadata.is_null() {
        return INVALID;
    }
    let input = unsafe { &*input };
    let Some(read_at) = input.read_at else {
        return INVALID;
    };
    report(|| {
        let source = Arc::new(Source {
            len: input.len,
            context: input.context as usize,
            read_at,
            session: thread::current().id(),
        });
        let mut prefix = [0; 4];
        if source.len < prefix.len() as u64 {
            return Ok(ABSENT);
        }
        let mut offset = 0;
        while offset < prefix.len() {
            offset += source.read_at(offset as u64, &mut prefix[offset..])?;
        }
        if prefix != [0x1a, 0x45, 0xdf, 0xa3] {
            return Ok(ABSENT);
        }
        let movie = Box::new(Movie::open(source)?);
        unsafe {
            metadata.write(movie.metadata());
            token.write(Box::into_raw(movie) as u64);
        }
        Ok(OK)
    })
}

unsafe extern "C" fn advance(_: u64, token: u64, seconds: f64, output: *mut Output) -> i32 {
    if token == 0 || output.is_null() || !seconds.is_finite() || !(0. ..=1.).contains(&seconds) {
        return INVALID;
    }
    report(|| {
        let movie = unsafe { &mut *(token as *mut Movie) };
        unsafe { output.write(movie.advance(seconds)?) };
        Ok(OK)
    })
}

unsafe extern "C" fn rewind(_: u64, token: u64) -> i32 {
    if token == 0 {
        return INVALID;
    }
    report(|| {
        let movie = unsafe { &mut *(token as *mut Movie) };
        *movie = Movie::open(movie.source.clone())?;
        Ok(OK)
    })
}

unsafe extern "C" fn close(_: u64, token: u64) {
    if token != 0 {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            drop(unsafe { Box::from_raw(token as *mut Movie) })
        }));
    }
}

static HOST: MovieHost = MovieHost {
    size: size_of::<MovieHost>() as u32,
    reserved: 0,
    context: 0,
    open,
    advance,
    rewind,
    close,
};

#[unsafe(no_mangle)]
pub extern "C" fn art3m1s_katze_movie_host_v0() -> *const c_void {
    ptr::from_ref(&HOST).cast()
}
