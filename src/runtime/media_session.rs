//! Runtime-owned software video playback.
//!
//! The session owns demux/decode state and PTS scheduling. It uploads decoded
//! frames to the active GPU backend under the same names used by the existing
//! layer-video path. Audio output remains host-owned.

use crate::backend::{GpuBackend, Yuv420pPlanes};
use crate::media::ffmpeg::{FfmpegVideoDecoder, RgbaVideoFrame, YuvVideoFrame};
use crate::media::{FrameQueue, MediaTime, VideoPixelFormat};
use crate::render_pipeline::draw::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawList, TextureProvider,
};
use crate::video::video_layer_texture_name;
use glam::{Affine2, Vec2};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub const FULLSCREEN_VIDEO_TEXTURE: &str = "__video_layer__:__fullscreen__";
const VIDEO_FRAME_QUEUE_CAPACITY: usize = 3;
const MAX_DECODE_STEPS_PER_TICK: usize = 4;

enum DecodedFrame {
    Yuv(YuvVideoFrame),
    Rgba(RgbaVideoFrame),
}

impl DecodedFrame {
    fn pts(&self) -> MediaTime {
        match self {
            Self::Yuv(frame) => frame.info.pts,
            Self::Rgba(frame) => frame.info.pts,
        }
    }

    fn upload(&self, gpu: &mut dyn GpuBackend, name: &str) -> bool {
        match self {
            Self::Yuv(frame) if frame.info.format == VideoPixelFormat::Yuv420p => {
                let planes = &frame.info.planes;
                if frame.info.plane_count < 3 {
                    return false;
                }
                let Some(y) = plane_slice(
                    &frame.pixels,
                    planes[0].offset,
                    planes[0].stride,
                    planes[0].height,
                    planes[0].width,
                ) else {
                    return false;
                };
                let Some(u) = plane_slice(
                    &frame.pixels,
                    planes[1].offset,
                    planes[1].stride,
                    planes[1].height,
                    planes[1].width,
                ) else {
                    return false;
                };
                let Some(v) = plane_slice(
                    &frame.pixels,
                    planes[2].offset,
                    planes[2].stride,
                    planes[2].height,
                    planes[2].width,
                ) else {
                    return false;
                };
                gpu.upload_video_yuv420p(
                    name,
                    frame.info.width,
                    frame.info.height,
                    Yuv420pPlanes {
                        y,
                        y_stride: planes[0].stride,
                        u,
                        u_stride: planes[1].stride,
                        v,
                        v_stride: planes[2].stride,
                    },
                )
            }
            Self::Yuv(_) => false,
            Self::Rgba(frame) => {
                gpu.upload_video_rgba(name, frame.info.width, frame.info.height, &frame.pixels)
            }
        }
    }
}

fn plane_slice(
    pixels: &[u8],
    offset: usize,
    stride: usize,
    height: u32,
    width: u32,
) -> Option<&[u8]> {
    let required = (height as usize)
        .checked_sub(1)?
        .checked_mul(stride)?
        .checked_add(width as usize)?;
    pixels.get(offset..offset.checked_add(required)?)
}

struct VideoPlayback {
    id: Option<String>,
    texture_name: String,
    decoder: FfmpegVideoDecoder,
    queue: FrameQueue<DecodedFrame>,
    clock: MediaTime,
    loop_play: bool,
    decoder_eof: bool,
    uploaded_any: bool,
}

impl VideoPlayback {
    fn new(
        id: Option<String>,
        texture_name: String,
        decoder: FfmpegVideoDecoder,
        loop_play: bool,
    ) -> Self {
        Self {
            id,
            texture_name,
            decoder,
            queue: FrameQueue::new(VIDEO_FRAME_QUEUE_CAPACITY),
            clock: MediaTime::ZERO,
            loop_play,
            decoder_eof: false,
            uploaded_any: false,
        }
    }

    fn advance(&mut self, delta_ms: u64, gpu: &mut dyn GpuBackend) -> bool {
        self.clock = self.clock.saturating_add(Duration::from_millis(delta_ms));
        self.fill_queue(gpu);

        let mut latest_due = None;
        while self
            .queue
            .front()
            .is_some_and(|frame| !self.uploaded_any || frame.pts() <= self.clock)
        {
            latest_due = self.queue.pop();
        }
        if let Some(frame) = latest_due {
            if !frame.upload(gpu, &self.texture_name) {
                crate::core_warn!(
                    "[media] video frame upload failed: id={:?} name={}",
                    self.id,
                    self.texture_name
                );
            }
            self.uploaded_any = true;
        }
        self.fill_queue(gpu);
        self.decoder_eof && self.queue.is_empty()
    }

    fn fill_queue(&mut self, gpu: &mut dyn GpuBackend) {
        for _ in 0..MAX_DECODE_STEPS_PER_TICK {
            if self.queue.len() >= VIDEO_FRAME_QUEUE_CAPACITY {
                break;
            }
            let frame = if gpu.supports_video_yuv420p() {
                self.decoder
                    .next_yuv_frame()
                    .map(|frame| frame.map(DecodedFrame::Yuv))
            } else {
                self.decoder
                    .next_rgba_frame()
                    .map(|frame| frame.map(DecodedFrame::Rgba))
            };
            match frame {
                Ok(Some(frame)) => {
                    self.queue.push(frame);
                }
                Ok(None) => {
                    self.decoder_eof = true;
                    break;
                }
                Err(error) => {
                    crate::core_warn!(
                        "[media] video decode failed: id={:?} name={} error={error}",
                        self.id,
                        self.texture_name
                    );
                    self.decoder_eof = true;
                    break;
                }
            }
        }
    }

    fn rewind(&mut self) -> bool {
        let last_duration = self.decoder.info().duration;
        if last_duration.is_zero() {
            return false;
        }
        if self.decoder.seek_start().is_err() {
            return false;
        }
        self.queue.clear();
        self.decoder_eof = false;
        self.uploaded_any = false;
        self.clock = self.clock.saturating_sub(last_duration);
        true
    }
}

pub struct RuntimeMediaSession {
    host: crate::host::HostSession,
    enabled: bool,
    fullscreen: Option<VideoPlayback>,
    layers: HashMap<String, VideoPlayback>,
    finished: Vec<Option<String>>,
    audio_generation: Arc<AtomicU64>,
}

impl Drop for RuntimeMediaSession {
    fn drop(&mut self) {
        self.invalidate_audio();
    }
}

impl RuntimeMediaSession {
    pub fn new(host: crate::host::HostSession) -> Self {
        Self {
            host,
            enabled: false,
            fullscreen: None,
            layers: HashMap::new(),
            finished: Vec::new(),
            audio_generation: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl RuntimeMediaSession {
    pub fn set_enabled(&mut self, enabled: bool) {
        if !enabled {
            self.stop_all();
        }
        self.enabled = enabled;
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn play_fullscreen(
        &mut self,
        resources: &crate::host_files::HostResources,
        file: &str,
        loop_play: bool,
    ) -> Result<(), String> {
        let decoder = FfmpegVideoDecoder::open(resources.open_media_source(file)?)?;
        self.fullscreen = Some(VideoPlayback::new(
            None,
            FULLSCREEN_VIDEO_TEXTURE.to_string(),
            decoder,
            loop_play,
        ));
        self.start_audio_extraction(resources, file, None, loop_play);
        Ok(())
    }

    pub fn play_layer(
        &mut self,
        resources: &crate::host_files::HostResources,
        id: &str,
        file: &str,
        loop_play: bool,
    ) -> Result<(), String> {
        let decoder = FfmpegVideoDecoder::open(resources.open_media_source(file)?)?;
        self.layers.insert(
            id.to_string(),
            VideoPlayback::new(
                Some(id.to_string()),
                video_layer_texture_name(id),
                decoder,
                loop_play,
            ),
        );
        self.start_audio_extraction(resources, file, Some(id.to_string()), loop_play);
        Ok(())
    }

    pub fn stop_fullscreen(&mut self) {
        self.invalidate_audio();
        self.fullscreen = None;
    }

    pub fn stop_layer(&mut self, id: &str) {
        self.invalidate_audio();
        self.layers.remove(id);
    }

    pub fn stop_all(&mut self) {
        self.invalidate_audio();
        self.fullscreen = None;
        self.layers.clear();
        self.finished.clear();
    }

    fn invalidate_audio(&self) {
        self.audio_generation.fetch_add(1, Ordering::SeqCst);
    }

    fn start_audio_extraction(
        &self,
        resources: &crate::host_files::HostResources,
        file: &str,
        id: Option<String>,
        loop_play: bool,
    ) {
        static NEXT_AUDIO_FILE: AtomicU64 = AtomicU64::new(1);
        let generation = self.audio_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let current_generation = Arc::clone(&self.audio_generation);
        let resources = resources.clone();
        let file = file.to_string();
        let host = self.host.clone();
        std::thread::spawn(move || {
            let _host_scope = host.enter();
            if current_generation.load(Ordering::SeqCst) != generation {
                return;
            }
            let source = match resources.open_media_source(&file) {
                Ok(source) => source,
                Err(error) => {
                    crate::core_debug!(
                        "[media] video audio source unavailable: file={file} error={error}"
                    );
                    return;
                }
            };
            let file_id = NEXT_AUDIO_FILE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "art3m1s-video-audio-{}-{file_id}.wav",
                std::process::id()
            ));
            match art3m1s_media::ffmpeg::decode_audio_to_wav(source, &path) {
                Ok(true) => {
                    if current_generation.load(Ordering::SeqCst) != generation {
                        let _ = std::fs::remove_file(&path);
                        return;
                    }
                    if let Some(path) = path.to_str() {
                        host.emit_media(
                            crate::host_media::HostMediaCommandKind::VideoAudioPlay,
                            crate::host_media::VideoAudioPlay {
                                id: id.as_deref(),
                                path,
                                loop_play,
                            },
                        );
                    } else {
                        let _ = std::fs::remove_file(&path);
                    }
                }
                Ok(false) => {
                    crate::core_debug!("[media] video has no decodable audio stream: file={file}");
                }
                Err(error) => {
                    crate::core_warn!(
                        "[media] video audio decode failed: file={file} error={error}"
                    );
                    let _ = std::fs::remove_file(PathBuf::from(&path));
                }
            }
        });
    }

    pub fn advance(&mut self, delta_ms: u64, gpu: &mut dyn GpuBackend) {
        if !self.enabled {
            return;
        }
        let mut completed = Vec::new();

        if let Some(video) = self.fullscreen.as_mut() {
            if video.advance(delta_ms, gpu) {
                if video.loop_play && video.rewind() {
                    // Continue from the next frame on the following tick.
                } else {
                    completed.push(None);
                }
            }
        }
        for (id, video) in &mut self.layers {
            if video.advance(delta_ms, gpu) {
                if video.loop_play && video.rewind() {
                } else {
                    completed.push(Some(id.clone()));
                }
            }
        }
        for id in completed {
            if id.is_none() {
                self.fullscreen = None;
            } else if let Some(layer_id) = id.as_deref() {
                self.layers.remove(layer_id);
            }
            self.finished.push(id);
        }
    }

    pub fn drain_finished(&mut self) -> Vec<Option<String>> {
        std::mem::take(&mut self.finished)
    }

    pub fn append_fullscreen_texture(
        &mut self,
        frame: &mut DrawList,
        provider: &mut dyn TextureProvider,
        stage_width: u32,
        stage_height: u32,
    ) {
        if self.fullscreen.is_none() {
            return;
        }
        let Some((texture, info)) = provider.resolve(FULLSCREEN_VIDEO_TEXTURE) else {
            return;
        };
        if info.width == 0 || info.height == 0 {
            return;
        }
        let transform = Affine2::from_scale_angle_translation(
            Vec2::new(
                stage_width as f32 / info.width as f32,
                stage_height as f32 / info.height as f32,
            ),
            0.0,
            Vec2::ZERO,
        );
        frame.push(DrawCommand {
            texture,
            size: info,
            transform,
            opacity: 1.0,
            blend: BlendMode::Alpha,
            color: ColorFilter::default(),
            clip: ClipRect::full(info),
            clip_bounds: Some([0.0, 0.0, stage_width as f32, stage_height as f32]),
            shader: None,
            mesh: None,
            stencil: None,
            native_emote: None,
        });
    }
}
