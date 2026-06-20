//! 基于 rodio 的音频后端。
//!
//! 实现 [`AudioBackend`] trait，将引擎音频事件映射到 rodio 的 Sink 操作。
//! 通过 `audio-backend` feature 启用。

use crate::audio::engine::{
    advance_channel_fade, apply_channel_fade_out, apply_channel_gain_fade,
    apply_channel_pan_fade, AudioBackend, AudioState, BgmConfig, FadeState, SeConfig,
    SoundCategory, SoundChannel, SoundFinishEvent, SoundFinishHandler,
};
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::OnceLock;

static AUDIO_FILE_READER: OnceLock<Box<dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync>> =
    OnceLock::new();

/// Register a file reader for audio files. When set, all audio file loading
/// goes through this callback (FFI → Flutter/PFS) instead of `std::fs::read`.
pub fn set_audio_file_reader(f: Box<dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync>) {
    let _ = AUDIO_FILE_READER.set(f);
}

fn read_audio_file(file: &str) -> Option<Vec<u8>> {
    if let Some(reader) = AUDIO_FILE_READER.get() {
        if let Some(data) = reader(file) {
            return Some(data);
        }
    }
    std::fs::read(file).ok()
}

/// 基于 rodio 的音频播放后端。
pub struct RodioBackend {
    state: AudioState,
    _stream: OutputStream,
    handle: OutputStreamHandle,
    bgm_sink: Option<Sink>,
    se_sinks: HashMap<String, Sink>,
    voice_sinks: HashMap<String, Sink>,
    pending_finish: Vec<SoundFinishEvent>,
}

impl std::fmt::Debug for RodioBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RodioBackend")
            .field("state", &self.state)
            .finish()
    }
}

impl RodioBackend {
    pub fn new() -> Result<Self, String> {
        let (stream, handle) =
            OutputStream::try_default().map_err(|e| format!("无法打开音频设备: {e}"))?;
        Ok(Self {
            state: AudioState::default(),
            _stream: stream,
            handle,
            bgm_sink: None,
            se_sinks: HashMap::new(),
            voice_sinks: HashMap::new(),
            pending_finish: Vec::new(),
        })
    }

    fn play_to_sink(
        handle: &OutputStreamHandle,
        data: &[u8],
        loop_play: bool,
    ) -> Result<Sink, String> {
        let source = Decoder::new(Cursor::new(data.to_vec()))
            .map_err(|e| format!("解码音频失败: {e}"))?;
        let sink =
            Sink::try_new(handle).map_err(|e| format!("创建音频通道失败: {e}"))?;
        if loop_play {
            sink.append(source.repeat_infinite());
        } else {
            sink.append(source);
        }
        Ok(sink)
    }

    fn set_channel_gain(channel: &mut SoundChannel, gain: Option<i32>, pan: Option<i32>) {
        if let Some(g) = gain {
            channel.raw_gain = g;
            channel.current_gain = SoundChannel::gain_to_linear(g);
        }
        if let Some(p) = pan {
            channel.raw_pan = p;
            channel.current_pan = SoundChannel::pan_to_linear(p);
        }
    }

    fn start_fade_in(channel: &mut SoundChannel, fade_ms: u64, clock_ms: u64) {
        if fade_ms > 0 {
            let target_gain = channel.current_gain;
            channel.current_gain = 0.0;
            channel.fade = Some(FadeState {
                target_gain,
                target_pan: channel.current_pan,
                from_gain: 0.0,
                from_pan: channel.current_pan,
                start_ms: clock_ms,
                duration_ms: fade_ms,
                stop_on_complete: false,
            });
        }
    }

    fn notify_finish(&mut self, channel: &SoundChannel) {
        let handler = match channel.category {
            SoundCategory::Bgm => self.state.bgm_finish_handler.clone(),
            SoundCategory::Se => self.state.se_finish_handlers.get(&channel.id).cloned(),
            SoundCategory::Voice => None,
        };
        self.pending_finish.push(SoundFinishEvent {
            id: Some(channel.id.clone()),
            category: channel.category,
            handler,
        });
    }

    fn sync_sink_volume(&self, sink: &Sink, channel: &SoundChannel) {
        let category_vol = match channel.category {
            SoundCategory::Bgm => self.state.bgm_volume,
            SoundCategory::Se => self.state.se_volume,
            SoundCategory::Voice => self.state.voice_volume,
        };
        let effective = channel.current_gain * category_vol * self.state.master_volume;
        sink.set_volume(effective);
    }
}

impl AudioBackend for RodioBackend {
    fn play_bgm(&mut self, file: &str, config: &BgmConfig) {
        if let Some(sink) = self.bgm_sink.take() {
            sink.stop();
        }
        self.state.bgm_channel = None;

        let data = match read_audio_file(file) {
            Some(d) => d,
            None => return,
        };

        match Self::play_to_sink(&self.handle, &data, config.loop_play) {
            Ok(sink) => {
                let mut channel = SoundChannel::new("bgm", file, SoundCategory::Bgm);
                channel.playing = true;
                channel.loop_play = config.loop_play;
                Self::set_channel_gain(&mut channel, config.gain, config.pan);
                Self::start_fade_in(&mut channel, config.fade_in_ms, self.state.clock_ms);
                self.sync_sink_volume(&sink, &channel);
                self.state.bgm_channel = Some(channel);
                self.bgm_sink = Some(sink);
            }
            Err(_) => {}
        }
    }

    fn stop_bgm(&mut self, fade_time_ms: u64) -> bool {
        let Some(ref mut channel) = self.state.bgm_channel else {
            return false;
        };
        if !channel.playing {
            return false;
        }
        apply_channel_fade_out(channel, fade_time_ms, self.state.clock_ms);
        if fade_time_ms == 0 {
            if let Some(sink) = self.bgm_sink.take() {
                sink.stop();
            }
            self.state.bgm_channel = None;
        }
        true
    }

    fn crossfade_bgm(&mut self, file: &str, config: &BgmConfig) {
        let fade_time = config.fade_in_ms;
        if let Some(ref mut old) = self.state.bgm_channel {
            if old.playing {
                apply_channel_fade_out(old, fade_time, self.state.clock_ms);
            }
        }

        let data = match read_audio_file(file) {
            Some(d) => d,
            None => return,
        };

        match Self::play_to_sink(&self.handle, &data, config.loop_play) {
            Ok(sink) => {
                let mut channel = SoundChannel::new("bgm", file, SoundCategory::Bgm);
                channel.playing = true;
                channel.loop_play = config.loop_play;
                Self::set_channel_gain(&mut channel, config.gain, config.pan);
                Self::start_fade_in(&mut channel, fade_time, self.state.clock_ms);
                self.sync_sink_volume(&sink, &channel);
                self.state.bgm_channel = Some(channel);
                self.bgm_sink = Some(sink);
            }
            Err(_) => {}
        }
    }

    fn fade_bgm_gain(&mut self, target_gain_raw: i32, time_ms: u64) {
        if let Some(ref mut channel) = self.state.bgm_channel {
            if channel.playing {
                apply_channel_gain_fade(channel, target_gain_raw, time_ms, self.state.clock_ms);
            }
        }
    }

    fn pan_bgm(&mut self, target_pan_raw: i32, time_ms: u64) {
        if let Some(ref mut channel) = self.state.bgm_channel {
            if channel.playing {
                apply_channel_pan_fade(channel, target_pan_raw, time_ms, self.state.clock_ms);
            }
        }
    }

    fn play_se(&mut self, id: &str, file: &str, config: &SeConfig) {
        if config.skippable && self.state.is_skipping {
            return;
        }
        if let Some(sink) = self.se_sinks.remove(id) {
            sink.stop();
        }
        self.state.se_channels.remove(id);

        let data = match read_audio_file(file) {
            Some(d) => d,
            None => return,
        };

        match Self::play_to_sink(&self.handle, &data, config.loop_play) {
            Ok(sink) => {
                let mut channel = SoundChannel::new(id, file, SoundCategory::Se);
                channel.playing = true;
                channel.loop_play = config.loop_play;
                Self::set_channel_gain(&mut channel, config.gain, config.pan);
                Self::start_fade_in(&mut channel, config.fade_in_ms, self.state.clock_ms);
                self.sync_sink_volume(&sink, &channel);
                self.state.se_channels.insert(id.to_string(), channel);
                self.se_sinks.insert(id.to_string(), sink);
            }
            Err(_) => {}
        }
    }

    fn stop_se(&mut self, id: &str, fade_time_ms: u64) -> bool {
        let Some(channel) = self.state.se_channels.get_mut(id) else {
            return false;
        };
        if !channel.playing {
            return false;
        }
        apply_channel_fade_out(channel, fade_time_ms, self.state.clock_ms);
        if fade_time_ms == 0 {
            if let Some(sink) = self.se_sinks.remove(id) {
                sink.stop();
            }
            self.state.se_channels.remove(id);
        }
        true
    }

    fn fade_se_gain(&mut self, id: &str, target_gain_raw: i32, time_ms: u64) {
        if let Some(channel) = self.state.se_channels.get_mut(id) {
            if channel.playing {
                apply_channel_gain_fade(channel, target_gain_raw, time_ms, self.state.clock_ms);
            }
        }
    }

    fn pan_se(&mut self, id: &str, target_pan_raw: i32, time_ms: u64) {
        if let Some(channel) = self.state.se_channels.get_mut(id) {
            if channel.playing {
                apply_channel_pan_fade(channel, target_pan_raw, time_ms, self.state.clock_ms);
            }
        }
    }

    fn play_voice(&mut self, id: &str, file: &str, config: &SeConfig) {
        self.state.voice_channels.remove(id);
        if let Some(sink) = self.voice_sinks.remove(id) {
            sink.stop();
        }

        let data = match read_audio_file(file) {
            Some(d) => d,
            None => return,
        };

        match Self::play_to_sink(&self.handle, &data, config.loop_play) {
            Ok(sink) => {
                let mut channel = SoundChannel::new(id, file, SoundCategory::Voice);
                channel.playing = true;
                channel.loop_play = config.loop_play;
                Self::set_channel_gain(&mut channel, config.gain, config.pan);
                Self::start_fade_in(&mut channel, config.fade_in_ms, self.state.clock_ms);
                self.sync_sink_volume(&sink, &channel);
                self.state.voice_channels.insert(id.to_string(), channel);
                self.voice_sinks.insert(id.to_string(), sink);
            }
            Err(_) => {}
        }
    }

    fn stop_all_sounds(&mut self) {
        if let Some(sink) = self.bgm_sink.take() {
            sink.stop();
        }
        for (_, sink) in self.se_sinks.drain() {
            sink.stop();
        }
        for (_, sink) in self.voice_sinks.drain() {
            sink.stop();
        }
        self.state.bgm_channel = None;
        self.state.se_channels.clear();
        self.state.voice_channels.clear();
    }

    fn set_master_volume(&mut self, volume: f32) {
        self.state.master_volume = volume.clamp(0.0, 1.0);
    }

    fn set_bgm_volume(&mut self, volume: f32) {
        self.state.bgm_volume = volume.clamp(0.0, 1.0);
    }

    fn set_se_volume(&mut self, volume: f32) {
        self.state.se_volume = volume.clamp(0.0, 1.0);
    }

    fn set_voice_volume(&mut self, volume: f32) {
        self.state.voice_volume = volume.clamp(0.0, 1.0);
    }

    fn set_skipping(&mut self, skipping: bool) {
        self.state.is_skipping = skipping;
    }

    fn is_se_playing(&self, id: &str) -> bool {
        self.state
            .se_channels
            .get(id)
            .map(|c| c.playing)
            .unwrap_or(false)
    }

    fn is_bgm_playing(&self) -> bool {
        self.state
            .bgm_channel
            .as_ref()
            .map(|c| c.playing)
            .unwrap_or(false)
    }

    fn set_sound_finish_handler(&mut self, id: Option<&str>, handler: SoundFinishHandler) {
        match id {
            Some(se_id) => {
                self.state
                    .se_finish_handlers
                    .insert(se_id.to_string(), handler);
            }
            None => {
                self.state.bgm_finish_handler = Some(handler);
            }
        }
    }

    fn remove_sound_finish_handler(&mut self, id: Option<&str>) {
        match id {
            Some(se_id) => {
                self.state.se_finish_handlers.remove(se_id);
            }
            None => {
                self.state.bgm_finish_handler = None;
            }
        }
    }

    fn advance(&mut self, delta_ms: u64) {
        self.state.clock_ms = self.state.clock_ms.saturating_add(delta_ms);
        let now = self.state.clock_ms;

        // BGM fade
        if let Some(ref mut channel) = self.state.bgm_channel {
            if advance_channel_fade(channel, now) {
                if let Some(sink) = self.bgm_sink.take() {
                    sink.stop();
                }
                let stopped = self.state.bgm_channel.take();
                if let Some(stopped) = stopped {
                    self.notify_finish(&stopped);
                }
            }
        }

        // SE fades
        let mut finished_se: Vec<String> = Vec::new();
        for (id, channel) in self.state.se_channels.iter_mut() {
            if advance_channel_fade(channel, now) {
                finished_se.push(id.clone());
            }
        }
        for id in &finished_se {
            if let Some(sink) = self.se_sinks.remove(id) {
                sink.stop();
            }
            if let Some(channel) = self.state.se_channels.remove(id) {
                self.notify_finish(&channel);
            }
        }

        // Voice fades
        let mut finished_voice: Vec<String> = Vec::new();
        for (id, channel) in self.state.voice_channels.iter_mut() {
            if advance_channel_fade(channel, now) {
                finished_voice.push(id.clone());
            }
        }
        for id in &finished_voice {
            if let Some(sink) = self.voice_sinks.remove(id) {
                sink.stop();
            }
            if let Some(channel) = self.state.voice_channels.remove(id) {
                self.notify_finish(&channel);
            }
        }

        // Natural end detection for non-looping SE
        let mut natural_finish: Vec<String> = Vec::new();
        for (id, sink) in self.se_sinks.iter() {
            if sink.empty() {
                if let Some(ch) = self.state.se_channels.get(id) {
                    if ch.playing && !ch.loop_play {
                        natural_finish.push(id.clone());
                    }
                }
            }
        }
        for id in &natural_finish {
            if let Some(sink) = self.se_sinks.remove(id) {
                sink.stop();
            }
            if let Some(channel) = self.state.se_channels.remove(id) {
                self.notify_finish(&channel);
            }
        }

        // Sync volumes
        if let Some(ref sink) = self.bgm_sink {
            if let Some(ref channel) = self.state.bgm_channel {
                self.sync_sink_volume(sink, channel);
            }
        }
        for (id, sink) in self.se_sinks.iter() {
            if let Some(channel) = self.state.se_channels.get(id) {
                self.sync_sink_volume(sink, channel);
            }
        }
        for (id, sink) in self.voice_sinks.iter() {
            if let Some(channel) = self.state.voice_channels.get(id) {
                self.sync_sink_volume(sink, channel);
            }
        }
    }

    fn poll_finish_events(&mut self) -> Vec<SoundFinishEvent> {
        std::mem::take(&mut self.pending_finish)
    }

    fn audio_state(&self) -> &AudioState {
        &self.state
    }

    fn audio_state_mut(&mut self) -> &mut AudioState {
        &mut self.state
    }
}
