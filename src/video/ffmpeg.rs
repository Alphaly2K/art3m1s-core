//! FFmpeg 视频后端。
//!
//! 使用 FFmpeg 解码视频文件，支持多种格式（MPEG-1/2/4、WMV、Ogg Theora 等）。
//! 解码后的视频帧可以渲染到全屏或视频图层。

use crate::video::engine::*;
use ffmpeg_next as ffmpeg;
use std::collections::VecDeque;
use std::io::Write;

/// FFmpeg 视频后端。
pub struct FfmpegBackend {
    state: VideoState,
    /// 当前播放的视频解码器
    decoder: Option<VideoDecoder>,
    /// 视频完成事件队列
    finish_queue: VecDeque<VideoFinishEvent>,
}

/// 视频解码器状态
struct VideoDecoder {
    /// FFmpeg 输入上下文（用于读取包）
    input: Option<ffmpeg::format::context::Input>,
    /// 包迭代器（保存状态，避免耗尽）
    packet_iterator: Option<std::iter::Peekable<ffmpeg::format::context::input::PacketIter<'static>>>,
    /// FFmpeg 解码器上下文
    decoder: ffmpeg::decoder::Video,
    /// 视频流索引
    stream_index: usize,
    /// 视频宽度
    width: u32,
    /// 视频高度
    height: u32,
    /// 是否有 Alpha 通道
    has_alpha: bool,
    /// 当前帧数据（RGBA）
    current_frame: Option<Vec<u8>>,
    /// 当前帧时间戳（毫秒）
    current_pts_ms: u64,
    /// 视频时长（毫秒）
    duration_ms: u64,
    /// 是否已解码完成
    finished: bool,
    /// 是否已触发完成事件（防止重复触发）
    finish_event_sent: bool,
    /// 已解码的帧数（用于在 PTS 缺失时计算时间戳）
    frame_count: u64,
    /// 下一帧的预期 PTS（毫秒），用于帧同步
    next_frame_pts_ms: u64,
    /// 视频 ID（图层 ID 或 "__fullscreen__"）
    id: String,
    /// 是否循环播放
    loop_play: bool,
    /// 时间基（用于计算时间戳）
    time_base: ffmpeg::util::rational::Rational,
    /// 帧率
    frame_rate: ffmpeg::util::rational::Rational,
    /// 软件缩放上下文（用于转换为 RGBA）
    scaler: Option<ffmpeg::software::scaling::Context>,
    /// 临时文件路径（用于延迟删除）
    temp_file: Option<std::path::PathBuf>,
}

impl FfmpegBackend {
    pub fn new() -> Self {
        ffmpeg::init().unwrap_or(());
        Self {
            state: VideoState::default(),
            decoder: None,
            finish_queue: VecDeque::new(),
        }
    }

    /// 打开视频文件并初始化解码器
    fn open_video(&mut self, id: &str, file: &str, loop_play: bool) -> bool {
        crate::core_info!("[FFmpeg] 开始打开视频: {} (id={})", file, id);

        // 通过 FFI 请求视频文件数据（前端负责文件 IO）
        let video_data = match crate::ffi::request_file(file) {
            Ok(data) => {
                crate::core_info!("[FFmpeg] 视频文件加载成功: {} ({} bytes)", file, data.len());
                data
            }
            Err(e) => {
                crate::core_error!("[FFmpeg] 无法加载视频文件 {}: {}", file, e);
                // 加载失败时使用模拟数据，让脚本继续执行
                return self.create_mock_decoder(id, file, loop_play);
            }
        };

        crate::core_info!("视频文件已加载: {} ({} bytes)", file, video_data.len());

        // 将视频数据写入临时文件（FFmpeg 需要从文件读取）
        let temp_path = std::env::temp_dir().join(format!("art3m1s_video_{}.tmp", id));
        match std::fs::write(&temp_path, &video_data) {
            Ok(_) => {}
            Err(e) => {
                crate::core_error!("无法写入临时视频文件: {}", e);
                return self.create_mock_decoder(id, file, loop_play);
            }
        }

        // 使用 FFmpeg 打开视频文件
        let input = match ffmpeg::format::input(&temp_path) {
            Ok(input) => input,
            Err(e) => {
                crate::core_error!("无法解析视频文件 {}: {}", file, e);
                let _ = std::fs::remove_file(&temp_path);
                return self.create_mock_decoder(id, file, loop_play);
            }
        };

        // 找到视频流
        let stream = input
            .streams()
            .find(|s| s.parameters().medium() == ffmpeg::media::Type::Video);
        let stream = match stream {
            Some(s) => s,
            None => {
                crate::core_error!("视频文件中没有找到视频流: {}", file);
                let _ = std::fs::remove_file(&temp_path);
                return self.create_mock_decoder(id, file, loop_play);
            }
        };

        let stream_index = stream.index();
        let time_base = stream.time_base();
        let frame_rate = stream.rate();
        // FFmpeg duration 的单位是 AV_TIME_BASE (1/1000000 秒)
        let duration_ms = if input.duration() > 0 {
            (input.duration() as u64 * 1000) / 1_000_000
        } else {
            5000 // 默认 5 秒
        };

        // 创建解码器
        let decoder = match ffmpeg::codec::context::Context::from_parameters(stream.parameters()) {
            Ok(context) => match context.decoder().video() {
                Ok(dec) => dec,
                Err(e) => {
                    crate::core_error!("无法创建视频解码器: {}", e);
                    let _ = std::fs::remove_file(&temp_path);
                    return self.create_mock_decoder(id, file, loop_play);
                }
            },
            Err(e) => {
                crate::core_error!("无法创建解码器上下文: {}", e);
                let _ = std::fs::remove_file(&temp_path);
                return self.create_mock_decoder(id, file, loop_play);
            }
        };

        let width = decoder.width();
        let height = decoder.height();
        let format = decoder.format();
        let codec_id = decoder.id();

        crate::core_info!(
            "[FFmpeg] 视频信息: {}x{}, format={:?}, codec={:?}, duration={}ms",
            width,
            height,
            format,
            codec_id,
            duration_ms
        );

        // 创建软件缩放上下文（将视频帧转换为 RGBA）
        let scaler = ffmpeg::software::scaling::Context::get(
            format,
            width,
            height,
            ffmpeg::format::Pixel::RGBA,
            width,
            height,
            ffmpeg::software::scaling::flag::Flags::BILINEAR,
        )
        .ok();

        self.decoder = Some(VideoDecoder {
            input: Some(input),
            packet_iterator: None, // 稍后初始化
            decoder,
            stream_index,
            width,
            height,
            has_alpha: false,
            current_frame: None,
            current_pts_ms: 0,
            duration_ms,
            finished: false,
            finish_event_sent: false,
            frame_count: 0,
            next_frame_pts_ms: 0,
            id: id.to_string(),
            loop_play,
            time_base,
            frame_rate,
            scaler,
            temp_file: Some(temp_path.clone()),
        });

        crate::core_info!(
            "[FFmpeg] 视频已初始化: {} ({}x{}, {}ms)",
            file,
            width,
            height,
            duration_ms
        );
        true
    }

    /// 创建模拟解码器（用于加载失败的情况）
    fn create_mock_decoder(&mut self, id: &str, file: &str, loop_play: bool) -> bool {
        let width = 1280;
        let height = 720;
        let duration_ms = 5000;

        crate::core_info!(
            "[FFmpeg] 使用模拟视频数据: {} ({}x{}, {}ms)",
            file,
            width,
            height,
            duration_ms
        );

        // 创建模拟解码器状态
        self.decoder = Some(VideoDecoder {
            input: None,
            packet_iterator: None,
            decoder: unsafe { std::mem::zeroed() }, // 模拟解码器不使用实际的 FFmpeg 解码器
            stream_index: 0,
            width,
            height,
            has_alpha: false,
            current_frame: None,
            current_pts_ms: 0,
            duration_ms,
            finished: false,
            finish_event_sent: false,
            frame_count: 0,
            next_frame_pts_ms: 0,
            id: id.to_string(),
            loop_play,
            time_base: ffmpeg::util::rational::Rational::new(1, 1000),
            frame_rate: ffmpeg::util::rational::Rational::new(30, 1),
            scaler: None,
            temp_file: None,
        });

        crate::core_info!("[FFmpeg] 模拟解码器创建完成");
        true
    }

    /// 解码下一帧
    fn decode_next_frame(&mut self) {
        let Some(ref mut decoder_state) = self.decoder else {
            return;
        };
        if decoder_state.finished {
            return;
        }

        // 从 input 中读取包
        let Some(ref mut input) = decoder_state.input else {
            crate::core_warn!("[FFmpeg] input 未初始化");
            return;
        };

        let mut found_frame = false;
        let mut packets_read = 0;

        // 迭代包直到找到视频帧（最多处理 10 个包，避免阻塞）
        // 使用 unsafe 来绕过生命周期检查
        let packets_iter = unsafe {
            // 将 input 的引用转换为 'static 的引用
            // 这是安全的，因为我们在同一个函数中使用这个迭代器，不会保存它
            std::mem::transmute::<_, std::iter::Peekable<ffmpeg::format::context::input::PacketIter<'static>>>(
                input.packets().peekable()
            )
        };

        for (stream, packet) in packets_iter {
            if packets_read >= 10 {
                break;
            }

            if stream.index() == decoder_state.stream_index {
                packets_read += 1;
                if packets_read <= 3 {
                    crate::core_info!("[FFmpeg] 处理视频包 #{}: pts={:?}, size={}", packets_read, packet.pts(), packet.size());
                }

                // 发送包到解码器
                if let Err(e) = decoder_state.decoder.send_packet(&packet) {
                    crate::core_warn!("[FFmpeg] 发送包到解码器失败: {}", e);
                    continue;
                }

                // 尝试接收解码后的帧
                let mut frame = ffmpeg::util::frame::Video::empty();
                match decoder_state.decoder.receive_frame(&mut frame) {
                    Ok(()) => {
                        crate::core_info!("[FFmpeg] 成功解码一帧: {}x{}, format={:?}", frame.width(), frame.height(), frame.format());
                        // 将帧转换为 RGBA
                        if let Some(ref mut scaler) = decoder_state.scaler {
                            let mut rgba_frame = ffmpeg::util::frame::Video::new(
                                ffmpeg::format::Pixel::RGBA,
                                decoder_state.width,
                                decoder_state.height,
                            );
                            if let Err(e) = scaler.run(&frame, &mut rgba_frame) {
                                crate::core_warn!("帧转换失败: {}", e);
                            } else {
                                // 提取 RGBA 数据
                                let width = rgba_frame.width() as usize;
                                let height = rgba_frame.height() as usize;
                                let stride = rgba_frame.stride(0) as usize;
                                let mut rgba_data = Vec::with_capacity(width * height * 4);

                                for y in 0..height {
                                    let start = y * stride;
                                    let end = start + width * 4;
                                    rgba_data.extend_from_slice(&rgba_frame.data(0)[start..end]);
                                }

                                // 计算 PTS（毫秒）
                                // 如果 packet.pts() 为 None，则根据帧号和帧率计算
                                let pts_ms = if let Some(pts) = packet.pts() {
                                    (pts as u64 * decoder_state.time_base.numerator() as u64 * 1000)
                                        / decoder_state.time_base.denominator() as u64
                                } else {
                                    // 使用帧号和帧率计算时间戳
                                    let frame_interval_ms = if decoder_state.frame_rate.numerator() > 0 {
                                        (decoder_state.frame_rate.denominator() as u64 * 1000)
                                            / decoder_state.frame_rate.numerator() as u64
                                    } else {
                                        33 // 默认 30fps
                                    };
                                    decoder_state.frame_count * frame_interval_ms
                                };

                                decoder_state.current_frame = Some(rgba_data);
                                decoder_state.current_pts_ms = pts_ms;

                                // 计算下一帧的预期 PTS
                                let frame_interval_ms = if decoder_state.frame_rate.numerator() > 0 {
                                    (decoder_state.frame_rate.denominator() as u64 * 1000)
                                        / decoder_state.frame_rate.numerator() as u64
                                } else {
                                    33 // 默认 30fps
                                };
                                decoder_state.frame_count += 1;
                                decoder_state.next_frame_pts_ms = decoder_state.frame_count * frame_interval_ms;

                                found_frame = true;
                            }
                        }
                        break; // 找到一帧后退出
                    }
                    Err(ffmpeg::Error::Other { errno: 11 }) => {
                        // EAGAIN: 需要更多数据
                        continue;
                    }
                    Err(e) => {
                        crate::core_warn!("[FFmpeg] 解码帧失败: {:?}", e);
                        continue;
                    }
                }
            }
        }

        crate::core_info!("[FFmpeg] decode_next_frame 完成: 读取了 {} 个包, 找到帧={}", packets_read, found_frame);

        if !found_frame && packets_read == 0 {
            // 没有读取到任何包，标记为完成
            crate::core_info!("[FFmpeg] 没有更多帧，标记为完成");
            decoder_state.finished = true;
        }
    }
}

impl VideoBackend for FfmpegBackend {
    fn play_fullscreen(&mut self, config: &VideoConfig) {
        crate::core_info!("[FFmpeg] play_fullscreen 被调用: file={}", config.file);
        self.stop_fullscreen();

        let id = "__fullscreen__";
        crate::core_info!("[FFmpeg] 准备调用 open_video");
        if !self.open_video(id, &config.file, config.loop_play) {
            crate::core_error!("[FFmpeg] open_video 失败");
            return;
        }
        crate::core_info!("[FFmpeg] open_video 成功");

        let mut channel = VideoChannel::new(id, &config.file);
        channel.loop_play = config.loop_play;
        channel.skippable = config.skippable;
        channel.playing = true;

        if let Some(ref decoder) = self.decoder {
            channel.width = decoder.width;
            channel.height = decoder.height;
            channel.has_alpha = decoder.has_alpha;
            channel.duration_ms = decoder.duration_ms;
        }

        self.state.fullscreen_video = Some(channel);
        crate::core_info!("[FFmpeg] 全屏视频播放已开始");
    }

    fn stop_fullscreen(&mut self) -> bool {
        // 删除临时文件
        if let Some(ref decoder) = self.decoder {
            if let Some(ref temp_file) = decoder.temp_file {
                let _ = std::fs::remove_file(temp_file);
            }
        }
        self.decoder = None;
        self.state.fullscreen_video.take().is_some()
    }

    fn is_fullscreen_playing(&self) -> bool {
        self.state
            .fullscreen_video
            .as_ref()
            .map_or(false, |v| v.playing)
    }

    fn play_layer(&mut self, id: &str, config: &VideoConfig) {
        self.stop_layer(id);

        if !self.open_video(id, &config.file, config.loop_play) {
            return;
        }

        let mut channel = VideoChannel::new(id, &config.file);
        channel.loop_play = config.loop_play;
        channel.skippable = config.skippable;
        channel.playing = true;

        if let Some(ref decoder) = self.decoder {
            channel.width = decoder.width;
            channel.height = decoder.height;
            channel.has_alpha = decoder.has_alpha;
            channel.duration_ms = decoder.duration_ms;
        }

        self.state.video_layers.insert(id.to_string(), channel);
    }

    fn stop_layer(&mut self, id: &str) -> bool {
        if let Some(ref decoder) = self.decoder {
            if decoder.id == id {
                // 删除临时文件
                if let Some(ref temp_file) = decoder.temp_file {
                    let _ = std::fs::remove_file(temp_file);
                }
                self.decoder = None;
            }
        }
        self.state.video_layers.remove(id).is_some()
    }

    fn is_layer_playing(&self, id: &str) -> bool {
        self.state.video_layers.get(id).map_or(false, |v| v.playing)
    }

    fn stop_all_videos(&mut self) {
        // 删除临时文件
        if let Some(ref decoder) = self.decoder {
            if let Some(ref temp_file) = decoder.temp_file {
                let _ = std::fs::remove_file(temp_file);
            }
        }
        self.decoder = None;
        self.state.fullscreen_video = None;
        self.state.video_layers.clear();
    }

    fn set_finish_handler(&mut self, handler: VideoFinishHandler) {
        self.state.finish_handler = Some(handler);
    }

    fn remove_finish_handler(&mut self) {
        self.state.finish_handler = None;
    }

    fn advance(&mut self, delta_ms: u64) {
        self.state.clock_ms += delta_ms;

        // 基于时间同步解码帧
        // 只有当 clock_ms >= 下一帧的预期 PTS 时才解码
        // 这样就能实现真正的帧同步，不会一次性解码所有帧
        let should_decode = match &self.decoder {
            Some(decoder) => !decoder.finished && self.state.clock_ms >= decoder.next_frame_pts_ms,
            None => false,
        };

        if should_decode {
            self.decode_next_frame();
        }

        let Some(ref mut decoder) = self.decoder else { return };

        // 更新播放位置
        if let Some(ref mut video) = self.state.fullscreen_video {
            if video.playing {
                video.position_ms += delta_ms;
                if video.position_ms >= video.duration_ms && video.duration_ms > 0 {
                    if video.loop_play {
                        video.position_ms = 0;
                        // 重置解码器状态以循环播放
                        decoder.finished = false;
                        decoder.finish_event_sent = false;
                        decoder.current_pts_ms = 0;
                    } else {
                        video.playing = false;
                        decoder.finished = true;
                    }
                }
            }
        }

        for channel in self.state.video_layers.values_mut() {
            if channel.playing {
                channel.position_ms += delta_ms;
                if channel.position_ms >= channel.duration_ms && channel.duration_ms > 0 {
                    if channel.loop_play {
                        channel.position_ms = 0;
                        // 重置解码器状态以循环播放
                        if decoder.id == channel.id {
                            decoder.finished = false;
                            decoder.finish_event_sent = false;
                            decoder.current_pts_ms = 0;
                        }
                    } else {
                        channel.playing = false;
                        if decoder.id == channel.id {
                            decoder.finished = true;
                        }
                    }
                }
            }
        }

        // 检查是否完成（只触发一次）
        if decoder.finished && !decoder.loop_play && !decoder.finish_event_sent {
            let handler = self.state.finish_handler.clone();
            let id = if decoder.id == "__fullscreen__" {
                None
            } else {
                Some(decoder.id.clone())
            };
            self.finish_queue
                .push_back(VideoFinishEvent { id, handler });

            // 标记已发送完成事件
            decoder.finish_event_sent = true;
        }
    }

    fn poll_finish_events(&mut self) -> Vec<VideoFinishEvent> {
        self.finish_queue.drain(..).collect()
    }

    fn video_state(&self) -> &VideoState {
        &self.state
    }

    fn video_state_mut(&mut self) -> &mut VideoState {
        &mut self.state
    }

    fn get_frame(&mut self, id: &str) -> Option<&[u8]> {
        self.decoder.as_ref().and_then(|d| {
            if d.id == id {
                d.current_frame.as_deref()
            } else {
                None
            }
        })
    }

    fn get_fullscreen_frame(&mut self) -> Option<&[u8]> {
        self.decoder.as_ref().and_then(|d| {
            if d.id == "__fullscreen__" {
                d.current_frame.as_deref()
            } else {
                None
            }
        })
    }
}
