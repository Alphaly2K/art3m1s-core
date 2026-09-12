//! FFmpeg-backed runtime media decoding.
//!
//! `ffmpeg-next` owns the demux/codec safe wrappers and link metadata. A small
//! Rust-side AVIO bridge exposes the runtime `MediaSource` abstraction so PFS
//! resources can be streamed without materializing a temporary file.

use crate::{
    DecodedVideoFrame, MAX_VIDEO_PLANES, MediaSource, MediaTime, VideoPixelFormat, VideoPlane,
};
use ffmpeg::format::{self, Pixel};
use ffmpeg::media::Type;
use ffmpeg::software::scaling;
use ffmpeg::{Error, Packet, Rational};
use ffmpeg_next as ffmpeg;
use std::ffi::{c_int, c_void};
use std::ptr;
use std::sync::{Arc, Once};
use std::time::Duration;

const AVIO_BUFFER_SIZE: usize = 64 * 1024;

static INIT: Once = Once::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoStreamInfo {
    pub width: u32,
    pub height: u32,
    pub frame_rate: Option<(u32, u32)>,
    pub duration: Duration,
}

/// A decoded frame copied into Rust-owned memory.
///
/// This is the software fallback path used to validate container/decoder
/// integration. Production Darwin/Android paths must prefer native frames or
/// YUV textures to avoid a CPU full-frame conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaVideoFrame {
    pub info: DecodedVideoFrame,
    pub pixels: Vec<u8>,
}

/// A tightly packed CPU YUV frame.
///
/// Software decoders keep their native YUV output here. The renderer should
/// upload these planes and convert to RGB in a shader instead of calling
/// `swscale` for every frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YuvVideoFrame {
    pub info: DecodedVideoFrame,
    pub pixels: Vec<u8>,
}

struct SourceIoState {
    source: Arc<dyn MediaSource>,
    offset: i64,
}

impl SourceIoState {
    fn len(&self) -> Result<u64, String> {
        self.source.len()
    }

    fn read(&mut self, output: &mut [u8]) -> Result<usize, String> {
        let read = self.source.read_at(self.offset.max(0) as u64, output)?;
        self.offset = self.offset.saturating_add(read as i64);
        Ok(read)
    }
}

unsafe extern "C" fn read_packet(opaque: *mut c_void, output: *mut u8, capacity: c_int) -> c_int {
    let result = std::panic::catch_unwind(|| {
        if opaque.is_null() || output.is_null() || capacity <= 0 {
            return Err("invalid AVIO read".to_string());
        }
        let state = unsafe { &mut *opaque.cast::<SourceIoState>() };
        let output = unsafe { std::slice::from_raw_parts_mut(output, capacity as usize) };
        state.read(output)
    });
    match result {
        Ok(Ok(0)) => ffmpeg::ffi::AVERROR_EOF,
        Ok(Ok(read)) => read as c_int,
        _ => -libc::EIO,
    }
}

unsafe extern "C" fn seek_packet(opaque: *mut c_void, offset: i64, whence: c_int) -> i64 {
    let result = std::panic::catch_unwind(|| {
        if opaque.is_null() {
            return Err("invalid AVIO seek".to_string());
        }
        let state = unsafe { &mut *opaque.cast::<SourceIoState>() };
        let size = state.len()?;
        if size > i64::MAX as u64 {
            return Err("media source is too large".to_string());
        }
        if whence == ffmpeg::ffi::AVSEEK_SIZE {
            return Ok(size as i64);
        }
        let base = match whence & !ffmpeg::ffi::AVSEEK_FORCE {
            libc::SEEK_SET => 0,
            libc::SEEK_CUR => state.offset,
            libc::SEEK_END => size as i64,
            _ => return Err("unsupported AVIO seek mode".to_string()),
        };
        let target = base.saturating_add(offset).clamp(0, size as i64);
        state.offset = target;
        Ok(target)
    });
    match result {
        Ok(Ok(offset)) => offset,
        _ => (-libc::EIO) as i64,
    }
}

unsafe fn free_avio(avio: *mut ffmpeg::ffi::AVIOContext) {
    if avio.is_null() {
        return;
    }
    let buffer = unsafe { (*avio).buffer };
    if !buffer.is_null() {
        unsafe { ffmpeg::ffi::av_freep(buffer.cast()) };
    }
    let mut avio = avio;
    unsafe { ffmpeg::ffi::avio_context_free(&mut avio) };
}

fn initialize() {
    INIT.call_once(|| {
        ffmpeg::util::log::set_level(ffmpeg::util::log::Level::Error);
        if let Err(error) = ffmpeg::init() {
            eprintln!("ffmpeg-next initialization failed: {error}");
        }
    });
}

fn duration_from_stream(stream: &ffmpeg::format::stream::Stream<'_>) -> Duration {
    if stream.duration() == ffmpeg::ffi::AV_NOPTS_VALUE || stream.duration() <= 0 {
        return Duration::ZERO;
    }
    let time_base = stream.time_base();
    if time_base.numerator() <= 0 || time_base.denominator() <= 0 {
        return Duration::ZERO;
    }
    let micros = stream.duration() as i128 * time_base.numerator() as i128 * 1_000_000
        / time_base.denominator() as i128;
    Duration::from_micros(micros.clamp(0, u64::MAX as i128) as u64)
}

fn frame_rate(stream: &ffmpeg::format::stream::Stream<'_>) -> Option<(u32, u32)> {
    let rate = stream.avg_frame_rate();
    if rate.numerator() <= 0 || rate.denominator() <= 0 {
        return None;
    }
    Some((rate.numerator() as u32, rate.denominator() as u32))
}

fn pts_micros(pts: Option<i64>, time_base: Rational) -> Option<i64> {
    let pts = pts?;
    if time_base.numerator() <= 0 || time_base.denominator() <= 0 {
        return None;
    }
    let micros =
        pts as i128 * time_base.numerator() as i128 * 1_000_000 / time_base.denominator() as i128;
    Some(micros.clamp(i64::MIN as i128, i64::MAX as i128) as i64)
}

pub struct FfmpegVideoDecoder {
    input: format::context::Input,
    decoder: ffmpeg::decoder::Video,
    rgba_scaler: scaling::Context,
    yuv_scaler: Option<scaling::Context>,
    stream_index: usize,
    time_base: Rational,
    frame_rate: Option<Rational>,
    info: VideoStreamInfo,
    eof: bool,
    _source: Box<Arc<dyn MediaSource>>,
    _io_state: Box<SourceIoState>,
}

unsafe impl Send for FfmpegVideoDecoder {}

impl std::fmt::Debug for FfmpegVideoDecoder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FfmpegVideoDecoder")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl FfmpegVideoDecoder {
    pub fn open(source: Arc<dyn MediaSource>) -> Result<Self, String> {
        initialize();
        let source_box = Box::new(Arc::clone(&source));
        let mut io_state = Box::new(SourceIoState { source, offset: 0 });
        let io_pointer = (&mut *io_state as *mut SourceIoState).cast::<c_void>();

        let avio_buffer = unsafe { ffmpeg::ffi::av_malloc(AVIO_BUFFER_SIZE) }.cast::<u8>();
        if avio_buffer.is_null() {
            return Err("unable to allocate AVIO buffer".to_string());
        }
        let avio = unsafe {
            ffmpeg::ffi::avio_alloc_context(
                avio_buffer,
                AVIO_BUFFER_SIZE as c_int,
                0,
                io_pointer,
                Some(read_packet),
                None,
                Some(seek_packet),
            )
        };
        if avio.is_null() {
            unsafe { ffmpeg::ffi::av_free(avio_buffer.cast()) };
            return Err("unable to allocate AVIO context".to_string());
        }

        let mut format = unsafe { ffmpeg::ffi::avformat_alloc_context() };
        if format.is_null() {
            unsafe { free_avio(avio) };
            return Err("unable to allocate format context".to_string());
        }
        unsafe {
            (*format).pb = avio;
            (*format).flags |= ffmpeg::ffi::AVFMT_FLAG_CUSTOM_IO;
        }

        let open_result = unsafe {
            ffmpeg::ffi::avformat_open_input(
                &mut format,
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if open_result < 0 {
            unsafe { free_avio(avio) };
            return Err(Error::from(open_result).to_string());
        }

        let stream_info_result =
            unsafe { ffmpeg::ffi::avformat_find_stream_info(format, ptr::null_mut()) };
        if stream_info_result < 0 {
            unsafe { ffmpeg::ffi::avformat_close_input(&mut format) };
            return Err(Error::from(stream_info_result).to_string());
        }

        let input = unsafe { format::context::Input::wrap(format) };
        let stream = input
            .streams()
            .best(Type::Video)
            .ok_or_else(|| "no video stream".to_string())?;
        let stream_index = stream.index();
        let time_base = stream.time_base();
        let rate = stream.avg_frame_rate();
        let decoder_context = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
            .map_err(|error| error.to_string())?;
        let decoder = decoder_context
            .decoder()
            .video()
            .map_err(|error| error.to_string())?;
        let info = VideoStreamInfo {
            width: decoder.width(),
            height: decoder.height(),
            frame_rate: frame_rate(&stream),
            duration: duration_from_stream(&stream),
        };
        let rgba_scaler = scaling::Context::get(
            decoder.format(),
            decoder.width(),
            decoder.height(),
            Pixel::RGBA,
            decoder.width(),
            decoder.height(),
            scaling::Flags::BILINEAR,
        )
        .map_err(|error| error.to_string())?;

        Ok(Self {
            input,
            decoder,
            rgba_scaler,
            yuv_scaler: None,
            stream_index,
            time_base,
            frame_rate: (rate.numerator() > 0 && rate.denominator() > 0).then_some(rate),
            info,
            eof: false,
            _source: source_box,
            _io_state: io_state,
        })
    }

    pub fn info(&self) -> VideoStreamInfo {
        self.info
    }

    pub fn next_rgba_frame(&mut self) -> Result<Option<RgbaVideoFrame>, String> {
        match self.next_decoded_frame()? {
            Some(frame) => self.convert_frame(&frame).map(Some),
            None => Ok(None),
        }
    }

    pub fn next_yuv_frame(&mut self) -> Result<Option<YuvVideoFrame>, String> {
        match self.next_decoded_frame()? {
            Some(frame) => self.convert_yuv_frame(&frame).map(Some),
            None => Ok(None),
        }
    }

    pub fn seek_start(&mut self) -> Result<(), String> {
        self.input.seek(0, ..).map_err(|error| error.to_string())?;
        self.decoder.flush();
        self.eof = false;
        Ok(())
    }

    fn next_decoded_frame(&mut self) -> Result<Option<ffmpeg::util::frame::Video>, String> {
        loop {
            let mut frame = ffmpeg::util::frame::Video::empty();
            match self.decoder.receive_frame(&mut frame) {
                Ok(()) => return Ok(Some(frame)),
                Err(Error::Eof) => return Ok(None),
                Err(Error::Other { errno }) if errno == libc::EAGAIN => {
                    if self.eof {
                        return Ok(None);
                    }
                    let mut packet = Packet::empty();
                    match packet.read(&mut self.input) {
                        Ok(()) if packet.stream() != self.stream_index => continue,
                        Ok(()) => {
                            self.decoder
                                .send_packet(&packet)
                                .map_err(|error| error.to_string())?;
                        }
                        Err(Error::Eof) => {
                            self.eof = true;
                            self.decoder.send_eof().map_err(|error| error.to_string())?;
                        }
                        Err(error) => return Err(error.to_string()),
                    }
                }
                Err(error) => return Err(error.to_string()),
            }
        }
    }

    fn convert_frame(
        &mut self,
        frame: &ffmpeg::util::frame::Video,
    ) -> Result<RgbaVideoFrame, String> {
        if frame.width() != self.rgba_scaler.input().width
            || frame.height() != self.rgba_scaler.input().height
            || frame.format() != self.rgba_scaler.input().format
        {
            self.rgba_scaler = scaling::Context::get(
                frame.format(),
                frame.width(),
                frame.height(),
                Pixel::RGBA,
                frame.width(),
                frame.height(),
                scaling::Flags::BILINEAR,
            )
            .map_err(|error| error.to_string())?;
        }

        let mut rgba = ffmpeg::util::frame::Video::empty();
        self.rgba_scaler
            .run(frame, &mut rgba)
            .map_err(|error| error.to_string())?;
        let width = rgba.width();
        let height = rgba.height();
        let stride = rgba.stride(0);
        let tight_stride = (width as usize)
            .checked_mul(4)
            .ok_or_else(|| "video width overflow".to_string())?;
        let pixels = if stride == tight_stride {
            rgba.data(0)[..tight_stride * height as usize].to_vec()
        } else {
            let source = rgba.data(0);
            let mut output = Vec::with_capacity(tight_stride * height as usize);
            for row in 0..height as usize {
                let start = row * stride;
                output.extend_from_slice(&source[start..start + tight_stride]);
            }
            output
        };

        let plane = VideoPlane {
            offset: 0,
            stride: tight_stride,
            width,
            height,
        };
        Ok(RgbaVideoFrame {
            info: self.decoded_frame_info(
                frame,
                VideoPixelFormat::Rgba8,
                tight_stride,
                1,
                [plane, VideoPlane::default(), VideoPlane::default()],
            ),
            pixels,
        })
    }

    fn convert_yuv_frame(
        &mut self,
        frame: &ffmpeg::util::frame::Video,
    ) -> Result<YuvVideoFrame, String> {
        let yuv = if frame.format() == Pixel::YUV420P {
            None
        } else {
            if self.yuv_scaler.as_ref().is_none_or(|scaler| {
                frame.width() != scaler.input().width
                    || frame.height() != scaler.input().height
                    || frame.format() != scaler.input().format
            }) {
                self.yuv_scaler = Some(
                    scaling::Context::get(
                        frame.format(),
                        frame.width(),
                        frame.height(),
                        Pixel::YUV420P,
                        frame.width(),
                        frame.height(),
                        scaling::Flags::BILINEAR,
                    )
                    .map_err(|error| error.to_string())?,
                );
            }
            let mut converted = ffmpeg::util::frame::Video::empty();
            self.yuv_scaler
                .as_mut()
                .ok_or_else(|| "YUV scaler is unavailable".to_string())?
                .run(frame, &mut converted)
                .map_err(|error| error.to_string())?;
            Some(converted)
        };
        let yuv = yuv.as_ref().unwrap_or(frame);
        let (pixels, planes) = copy_yuv420p(yuv)?;
        Ok(YuvVideoFrame {
            info: self.decoded_frame_info(
                frame,
                VideoPixelFormat::Yuv420p,
                planes[0].stride,
                MAX_VIDEO_PLANES as u8,
                planes,
            ),
            pixels,
        })
    }

    fn decoded_frame_info(
        &self,
        frame: &ffmpeg::util::frame::Video,
        format: VideoPixelFormat,
        stride: usize,
        plane_count: u8,
        planes: [VideoPlane; MAX_VIDEO_PLANES],
    ) -> DecodedVideoFrame {
        let pts = pts_micros(frame.pts().or_else(|| frame.timestamp()), self.time_base);
        let duration = self
            .frame_rate
            .and_then(|rate| {
                (rate.numerator() > 0 && rate.denominator() > 0).then(|| {
                    Duration::from_micros(
                        (1_000_000u128 * rate.denominator() as u128 / rate.numerator() as u128)
                            .min(u64::MAX as u128) as u64,
                    )
                })
            })
            .unwrap_or_default();
        DecodedVideoFrame {
            pts: MediaTime::from_micros(pts.unwrap_or(0).max(0) as u64),
            duration,
            width: frame.width(),
            height: frame.height(),
            stride,
            format,
            plane_count,
            planes,
        }
    }
}

fn copy_yuv420p(
    frame: &ffmpeg::util::frame::Video,
) -> Result<(Vec<u8>, [VideoPlane; MAX_VIDEO_PLANES]), String> {
    let width = frame.width();
    let height = frame.height();
    let chroma_width = width.div_ceil(2);
    let chroma_height = height.div_ceil(2);
    let y_len = checked_plane_len(width, height)?;
    let chroma_len = checked_plane_len(chroma_width, chroma_height)?;
    let total = y_len
        .checked_add(
            chroma_len
                .checked_mul(2)
                .ok_or_else(|| "video frame size overflow".to_string())?,
        )
        .ok_or_else(|| "video frame size overflow".to_string())?;
    let mut pixels = Vec::with_capacity(total);

    let y = append_plane(&mut pixels, frame, 0, width, height)?;
    let u = append_plane(&mut pixels, frame, 1, chroma_width, chroma_height)?;
    let v = append_plane(&mut pixels, frame, 2, chroma_width, chroma_height)?;
    Ok((pixels, [y, u, v]))
}

fn append_plane(
    output: &mut Vec<u8>,
    frame: &ffmpeg::util::frame::Video,
    plane: usize,
    width: u32,
    height: u32,
) -> Result<VideoPlane, String> {
    let stride = frame.stride(plane);
    let row_len = width as usize;
    if row_len == 0 || height == 0 || stride < row_len {
        return Err(format!("invalid YUV plane {plane} geometry"));
    }
    let source = frame.data(plane);
    let required = (height as usize)
        .saturating_sub(1)
        .saturating_mul(stride)
        .saturating_add(row_len);
    if source.len() < required {
        return Err(format!(
            "short YUV plane {plane}: {} bytes, need {required}",
            source.len()
        ));
    }

    let offset = output.len();
    output.reserve(row_len.saturating_mul(height as usize));
    for row in 0..height as usize {
        let start = row * stride;
        output.extend_from_slice(&source[start..start + row_len]);
    }
    Ok(VideoPlane {
        offset,
        stride: row_len,
        width,
        height,
    })
}

fn checked_plane_len(width: u32, height: u32) -> Result<usize, String> {
    (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| "video plane size overflow".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MediaSource;

    const ONE_FRAME_MPEG_BASE64: &str = concat!(
        "AAABuiEAAQABwzNnAAABuwAJwzNnACH/4ODmAAAB4AAuIQADX5EAAAGzAQAQE///4BgAAAG4AAgAQAAAAQAAD//4AAABARPyFKUvmb9wgAAAAb4Hqw////",
        "////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////8="
    );

    struct BytesSource(Vec<u8>);

    impl MediaSource for BytesSource {
        fn len(&self) -> Result<u64, String> {
            Ok(self.0.len() as u64)
        }

        fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, String> {
            let offset = usize::try_from(offset).map_err(|error| error.to_string())?;
            if offset >= self.0.len() {
                return Ok(0);
            }
            let count = output.len().min(self.0.len() - offset);
            output[..count].copy_from_slice(&self.0[offset..offset + count]);
            Ok(count)
        }
    }

    fn base64_decode(value: &str) -> Vec<u8> {
        let mut output = Vec::with_capacity(value.len() * 3 / 4);
        let mut accumulator = 0u32;
        let mut bits = 0u32;
        for byte in value.bytes() {
            let value = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' => break,
                _ => continue,
            } as u32;
            accumulator = (accumulator << 6) | value;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                output.push((accumulator >> bits) as u8);
            }
        }
        output
    }

    #[test]
    fn decodes_mpeg_frame_through_ffmpeg_next_and_custom_media_source() {
        let source = Arc::new(BytesSource(base64_decode(ONE_FRAME_MPEG_BASE64)));
        let mut decoder = FfmpegVideoDecoder::open(source).unwrap();
        assert_eq!(decoder.info().width, 16);
        assert_eq!(decoder.info().height, 16);
        let frame = decoder.next_rgba_frame().unwrap().unwrap();
        assert_eq!(frame.info.width, 16);
        assert_eq!(frame.info.height, 16);
        assert_eq!(frame.info.format, VideoPixelFormat::Rgba8);
        assert_eq!(frame.pixels.len(), 16 * 16 * 4);
    }

    #[test]
    fn decodes_mpeg_frame_to_packed_yuv420p() {
        let source = Arc::new(BytesSource(base64_decode(ONE_FRAME_MPEG_BASE64)));
        let mut decoder = FfmpegVideoDecoder::open(source).unwrap();
        let frame = decoder.next_yuv_frame().unwrap().unwrap();

        assert_eq!(frame.info.format, VideoPixelFormat::Yuv420p);
        assert_eq!(frame.info.width, 16);
        assert_eq!(frame.info.height, 16);
        assert_eq!(frame.info.plane_count, 3);
        assert_eq!(
            frame.info.planes[0],
            VideoPlane {
                offset: 0,
                stride: 16,
                width: 16,
                height: 16,
            }
        );
        assert_eq!(
            frame.info.planes[1],
            VideoPlane {
                offset: 16 * 16,
                stride: 8,
                width: 8,
                height: 8,
            }
        );
        assert_eq!(frame.info.planes[2].offset, 16 * 16 + 8 * 8);
        assert_eq!(frame.pixels.len(), 16 * 16 + 2 * 8 * 8);
    }
}
