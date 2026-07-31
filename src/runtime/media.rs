use super::CoreRuntime;
use crate::audio::{BgmConfig, SeConfig, SoundFinishHandler};
use crate::host_media::{self as hm, HostMediaCommandKind as Kind};
use crate::runtime::input;
use crate::save::AudioSnapshot;
use crate::video::{VideoConfig, VideoFinishHandler, video_layer_texture_name};
use asb_interpreter::Event;
use asb_interpreter::tags::var_handler::{SoundChannelInfo, SoundInfoSnapshot};
use std::collections::HashMap;
use std::sync::Mutex;

/// 声音播放状态镜像，供 `var system=get_sound_info` 的宿主查询钩子读取。
///
/// 钩子是进程级注册点（var 标签路径拿不到 runtime 实例），因此这里维护一份
/// 进程级快照，media 事件应用后与每帧推进时刷新。
static SOUND_INFO: Mutex<SoundInfoSnapshot> = Mutex::new(SoundInfoSnapshot {
    bgm: None,
    se: Vec::new(),
});

/// 读取当前声音快照（get_sound_info 钩子入口）。
pub(super) fn sound_info_snapshot() -> SoundInfoSnapshot {
    SOUND_INFO.lock().unwrap().clone()
}

pub(super) fn clear_sound_info_snapshot() {
    *SOUND_INFO.lock().unwrap() = SoundInfoSnapshot {
        bgm: None,
        se: Vec::new(),
    };
}

/// splay 的 A-B 循环文件名约定：file 以 `_a` 结尾（扩展名前）时，
/// 返回把 `_a` 换成 `_b` 的循环段文件名；否则 None。
fn ab_loop_file(file: &str) -> Option<String> {
    // 只在最后一个路径段上找扩展名，避免把目录里的点当扩展名分隔符
    let (stem, ext) = match file.rfind('.') {
        Some(dot) if !file[dot..].contains('/') && !file[dot..].contains('\\') => {
            (&file[..dot], &file[dot..])
        }
        _ => (file, ""),
    };
    stem.strip_suffix("_a").map(|base| format!("{base}_b{ext}"))
}

impl CoreRuntime {
    pub fn set_volume(&mut self, volume_type: &str, value: f32) {
        let v = value.clamp(0.0, 1.0);
        match volume_type {
            "master" => self.audio.set_master_volume(v),
            "bgm" => self.audio.set_bgm_volume(v),
            "se" => self.audio.set_se_volume(v),
            "voice" => self.audio.set_voice_volume(v),
            _ => {}
        }
        hm::emit(
            Kind::AudioSetVolume,
            hm::AudioSetVolume {
                channel: volume_type,
                value: v,
            },
        );
    }

    pub(super) fn apply_media_event(&mut self, event: &Event) {
        if self.apply_audio_event(event) {
            return;
        }
        let _ = self.apply_video_event(event);
    }

    fn resolve_magic_media_path(&self, name: &str) -> String {
        super::magic_path::resolve_path(&self.magic_paths, name)
    }

    pub(super) fn stop_all_media(&mut self) {
        let video_layer_ids: Vec<String> = self
            .video
            .video_state()
            .video_layers
            .keys()
            .cloned()
            .collect();
        self.audio.stop_all_sounds();
        self.video.stop_all_videos();
        for id in video_layer_ids {
            self.clear_video_layer_texture(&id);
        }
        hm::emit(Kind::AudioStopAll, hm::EmptyPayload {});
        hm::emit(Kind::VideoStopAll, hm::EmptyPayload {});
        self.refresh_sound_info_snapshot();
    }

    pub(super) fn restore_audio_snapshot(&mut self, snapshot: &AudioSnapshot) {
        snapshot.restore_into(self.audio.as_mut());

        if let Some(bgm) = &snapshot.bgm {
            let resolved_file = self.resolve_magic_media_path(&bgm.file);
            // 读档恢复同样按文件名约定重建 A-B 循环段
            let loop_file = bgm.loop_play.then(|| ab_loop_file(&bgm.file)).flatten();
            if let Some(channel) = &mut self.audio.audio_state_mut().bgm_channel {
                channel.loop_file = loop_file.clone();
            }
            let resolved_loop_file = loop_file
                .as_deref()
                .map(|f| self.resolve_magic_media_path(f));
            hm::emit(
                Kind::AudioBgmPlay,
                hm::BgmPlay {
                    file: &bgm.file,
                    resolved_file: Some(&resolved_file),
                    loop_play: bgm.loop_play,
                    gain: Some(bgm.gain),
                    pan: Some(bgm.pan),
                    fade_ms: 0,
                    loop_file: loop_file.as_deref(),
                    resolved_loop_file: resolved_loop_file.as_deref(),
                },
            );
        }
        for se in &snapshot.se {
            let resolved_file = self.resolve_magic_media_path(&se.file);
            hm::emit(
                Kind::AudioSePlay,
                hm::SePlay {
                    id: &se.id,
                    file: &se.file,
                    resolved_file: Some(&resolved_file),
                    loop_play: se.loop_play,
                    gain: Some(se.gain),
                    pan: Some(se.pan),
                    fade_ms: 0,
                    skippable: se.skippable,
                },
            );
        }
        for voice in &snapshot.voice {
            let resolved_file = self.resolve_magic_media_path(&voice.file);
            hm::emit(
                Kind::AudioVoicePlay,
                hm::VoicePlay {
                    id: &voice.id,
                    file: &voice.file,
                    resolved_file: Some(&resolved_file),
                    gain: Some(voice.gain),
                    pan: Some(voice.pan),
                    fade_ms: 0,
                },
            );
        }
    }

    /// 把音频状态镜像进 get_sound_info 快照（BGM + SE，SE 按 ID 升序）。
    pub(super) fn refresh_sound_info_snapshot(&self) {
        let state = self.audio.audio_state();
        let to_info = |channel: &crate::audio::SoundChannel| SoundChannelInfo {
            id: channel.id.clone(),
            playing: channel.playing,
            gain: i64::from(channel.raw_gain),
            pan: i64::from(channel.raw_pan),
        };
        let bgm = state.bgm_channel.as_ref().map(&to_info);
        let mut se: Vec<SoundChannelInfo> = state.se_channels.values().map(&to_info).collect();
        se.sort_by(|a, b| a.id.cmp(&b.id));
        *SOUND_INFO.lock().unwrap() = SoundInfoSnapshot { bgm, se };
    }

    pub(super) fn advance_media_and_enqueue_finish_handlers(&mut self, delta_ms: u64) {
        let host_media = crate::ffi::media_command_callback_registered();
        self.audio.advance(delta_ms);
        self.refresh_sound_info_snapshot();
        if !host_media {
            self.video.advance(delta_ms);
        }

        if !host_media {
            for event in self.audio.poll_finish_events() {
                if let Some(handler) = event.handler {
                    input::enqueue_handler_tags(
                        &self.interpreter,
                        handler.handler.as_deref(),
                        handler.file.as_deref(),
                        handler.label.as_deref(),
                        handler.call,
                        &HashMap::new(),
                        &[],
                    );
                }
            }
        }

        for event in self.video.poll_finish_events() {
            if host_media {
                // Completion is owned by the host decoder for both modes.
                continue;
            }
            if let Some(id) = event.id.as_deref() {
                self.clear_video_layer_texture(id);
            }
            if event.id.is_none() {
                self.video_finished
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }

            // Enqueue handler tags if registered.
            if let Some(handler) = event.handler {
                input::enqueue_handler_tags(
                    &self.interpreter,
                    handler.handler.as_deref(),
                    handler.file.as_deref(),
                    handler.label.as_deref(),
                    handler.call,
                    &HashMap::new(),
                    &[],
                );
            }
        }
    }

    pub fn notify_sound_finished(&mut self, id: Option<&str>) {
        let handler = if let Some(id) = id {
            self.audio.audio_state().se_finish_handlers.get(id).cloned()
        } else {
            self.audio.audio_state().bgm_finish_handler.clone()
        };
        match id {
            Some(id) => {
                self.audio.stop_se(id, 0);
                self.audio.audio_state_mut().voice_channels.remove(id);
            }
            None => {
                self.audio.stop_bgm(0);
            }
        }
        // Discard queued fallback completions from the internal state machine.
        let _ = self.audio.poll_finish_events();
        self.refresh_sound_info_snapshot();

        if let Some(handler) = handler {
            input::enqueue_handler_tags(
                &self.interpreter,
                handler.handler.as_deref(),
                handler.file.as_deref(),
                handler.label.as_deref(),
                handler.call,
                &HashMap::new(),
                &[],
            );
        }
    }

    pub(super) fn is_voice_playing(&self) -> bool {
        let state = self.audio.audio_state();
        state.voice_channels.values().any(|ch| ch.playing)
            || state
                .se_channels
                .values()
                .any(|ch| ch.playing && ch.file.contains(":vo/"))
    }

    /// 指定 ID 的声音（语音或 SE）是否在播放。automode syncse 门控用。
    pub(super) fn is_sound_playing(&self, id: &str) -> bool {
        let state = self.audio.audio_state();
        state
            .voice_channels
            .get(id)
            .or_else(|| state.se_channels.get(id))
            .is_some_and(|ch| ch.playing)
    }

    /// automode 自动前进前，syncse 列出的声音是否都已播完（空列表退化为
    /// "任意语音在播"的通用门控）。
    pub(super) fn automode_sync_ready(&self) -> bool {
        let sync = self.control.automode_sync_se();
        if sync.is_empty() {
            return !self.is_voice_playing();
        }
        !sync.iter().any(|id| self.is_sound_playing(id))
    }

    pub fn notify_video_finished(&mut self, id: Option<&str>) {
        // 图层视频优先取按 ID 登记的处理器，缺省回退到全局；全屏视频用全局。
        let handler = match id {
            Some(layer_id) => self
                .video
                .video_state()
                .layer_finish_handlers
                .get(layer_id)
                .or(self.video.video_state().finish_handler.as_ref())
                .cloned(),
            None => self.video.video_state().finish_handler.clone(),
        };
        match id {
            Some(layer_id) => {
                self.video.stop_layer(layer_id);
                self.clear_video_layer_texture(layer_id);
            }
            None => {
                self.video.stop_fullscreen();
            }
        }
        // Discard any queued fallback completion from the internal state machine.
        let _ = self.video.poll_finish_events();

        if id.is_none() {
            self.video_finished
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        if let Some(handler) = handler {
            input::enqueue_handler_tags(
                &self.interpreter,
                handler.handler.as_deref(),
                handler.file.as_deref(),
                handler.label.as_deref(),
                handler.call,
                &HashMap::new(),
                &[],
            );
        }
    }

    /// Upload a decoded RGBA frame directly from host-owned memory.
    ///
    /// The caller only needs to keep `rgba` alive for this synchronous call.
    /// The provider sends it directly to GL and does not retain a CPU copy.
    pub fn upload_video_layer_frame(
        &mut self,
        id: &str,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> bool {
        let is_playing = self
            .video
            .video_state()
            .video_layers
            .get(id)
            .is_some_and(|channel| channel.playing);
        if !is_playing {
            return false;
        }

        let texture_name = video_layer_texture_name(id);
        let saved_ctx = self.gl_ctx.bind_save();
        let uploaded = self
            .texture_provider
            .upload_video_rgba(&texture_name, width, height, rgba);
        self.gl_ctx.restore(saved_ctx);
        uploaded
    }

    fn bind_video_layer_texture(&mut self, id: &str) {
        self.compositor
            .set_layer_file(id, Some(video_layer_texture_name(id)));
        self.sync_layer_info_all();
    }

    fn clear_video_layer_texture(&mut self, id: &str) {
        let texture_name = video_layer_texture_name(id);
        self.compositor
            .clear_layer_file_if_matches(id, &texture_name);
        self.sync_layer_info_all();
    }

    fn apply_audio_event(&mut self, event: &Event) -> bool {
        let applied = self.apply_audio_event_inner(event);
        if applied {
            // 音频状态变化后刷新 get_sound_info 快照
            self.refresh_sound_info_snapshot();
        }
        applied
    }

    fn apply_audio_event_inner(&mut self, event: &Event) -> bool {
        match event {
            Event::BgmPlay {
                file,
                loop_play,
                gain,
                pan,
                fade_time,
                buffer,
            } => {
                let resolved_file = self.resolve_magic_media_path(file);
                self.audio.play_bgm(
                    file,
                    &BgmConfig {
                        loop_play: *loop_play,
                        gain: *gain,
                        pan: *pan,
                        fade_in_ms: fade_time.unwrap_or(0),
                        buffer_size: *buffer,
                    },
                );
                // A-B 循环：file 为 foo_a.* 且循环播放时，引导段播完后无限循环 foo_b.*
                let loop_file = loop_play.then(|| ab_loop_file(file)).flatten();
                if let Some(channel) = &mut self.audio.audio_state_mut().bgm_channel {
                    channel.loop_file = loop_file.clone();
                }
                let resolved_loop_file = loop_file
                    .as_deref()
                    .map(|f| self.resolve_magic_media_path(f));
                hm::emit(
                    Kind::AudioBgmPlay,
                    hm::BgmPlay {
                        file,
                        resolved_file: Some(&resolved_file),
                        loop_play: *loop_play,
                        gain: *gain,
                        pan: *pan,
                        fade_ms: fade_time.unwrap_or(0),
                        loop_file: loop_file.as_deref(),
                        resolved_loop_file: resolved_loop_file.as_deref(),
                    },
                );
                true
            }
            Event::BgmStop { fade_time } => {
                self.audio.stop_bgm(fade_time.unwrap_or(0));
                hm::emit(
                    Kind::AudioBgmStop,
                    hm::BgmStop {
                        fade_ms: fade_time.unwrap_or(0),
                    },
                );
                true
            }
            Event::BgmFade { gain, time } => {
                self.audio.fade_bgm_gain(*gain, *time);
                hm::emit(
                    Kind::AudioBgmFade,
                    hm::BgmFade {
                        gain: *gain,
                        time_ms: *time,
                    },
                );
                true
            }
            Event::BgmPan { pan, time } => {
                // [span] time=毫秒渐变时间，缺省立即切换
                let time_ms = time.unwrap_or(0);
                self.audio.pan_bgm(*pan, time_ms);
                hm::emit(Kind::AudioBgmPan, hm::BgmPan { pan: *pan, time_ms });
                true
            }
            Event::BgmCrossFade {
                file,
                loop_play,
                gain,
                pan,
                time,
            } => {
                let resolved_file = self.resolve_magic_media_path(file);
                self.audio.crossfade_bgm(
                    file,
                    &BgmConfig {
                        loop_play: *loop_play,
                        gain: *gain,
                        pan: *pan,
                        fade_in_ms: *time,
                        buffer_size: None,
                    },
                );
                hm::emit(
                    Kind::AudioBgmCrossfade,
                    hm::BgmCrossfade {
                        file,
                        resolved_file: Some(&resolved_file),
                        loop_play: *loop_play,
                        gain: *gain,
                        pan: *pan,
                        time_ms: *time,
                    },
                );
                true
            }
            Event::SePlay {
                id,
                file,
                loop_play,
                gain,
                pan,
                fade_time,
                skippable,
            } => {
                let resolved_file = self.resolve_magic_media_path(file);
                self.audio.play_se(
                    id,
                    file,
                    &SeConfig {
                        loop_play: *loop_play,
                        gain: *gain,
                        pan: *pan,
                        fade_in_ms: fade_time.unwrap_or(0),
                        buffer_size: None,
                        skippable: *skippable,
                    },
                );
                hm::emit(
                    Kind::AudioSePlay,
                    hm::SePlay {
                        id,
                        file,
                        resolved_file: Some(&resolved_file),
                        loop_play: *loop_play,
                        gain: *gain,
                        pan: *pan,
                        fade_ms: fade_time.unwrap_or(0),
                        skippable: *skippable,
                    },
                );
                true
            }
            Event::SeStop { id, fade_time } => {
                self.audio.stop_se(id, fade_time.unwrap_or(0));
                hm::emit(
                    Kind::AudioSeStop,
                    hm::SeStop {
                        id,
                        fade_ms: fade_time.unwrap_or(0),
                    },
                );
                true
            }
            Event::SeFade { id, gain, time } => {
                self.audio.fade_se_gain(id, *gain, *time);
                hm::emit(
                    Kind::AudioSeFade,
                    hm::SeFade {
                        id,
                        gain: *gain,
                        time_ms: *time,
                    },
                );
                true
            }
            Event::SePan { id, pan, time } => {
                // [sepan] time=毫秒渐变时间，缺省立即切换
                let time_ms = time.unwrap_or(0);
                self.audio.pan_se(id, *pan, time_ms);
                hm::emit(
                    Kind::AudioSePan,
                    hm::SePan {
                        id,
                        pan: *pan,
                        time_ms,
                    },
                );
                true
            }
            Event::VoicePlay {
                id,
                file,
                loop_play,
                gain,
                pan,
                fade_time,
                skippable,
            } => {
                let resolved_file = self.resolve_magic_media_path(file);
                // 脚本显式给了 id 时用之（可被 sestop/sepan 等按 ID 控制），
                // 缺省沿用自动编号 voice:{serial}。
                let voice_id = match id {
                    Some(id) if !id.is_empty() => id.clone(),
                    _ => {
                        self.voice_serial = self.voice_serial.saturating_add(1);
                        format!("voice:{}", self.voice_serial)
                    }
                };
                self.audio.play_voice(
                    &voice_id,
                    file,
                    &SeConfig {
                        loop_play: *loop_play,
                        gain: *gain,
                        pan: *pan,
                        fade_in_ms: fade_time.unwrap_or(0),
                        buffer_size: None,
                        skippable: *skippable,
                    },
                );
                hm::emit(
                    Kind::AudioVoicePlay,
                    hm::VoicePlay {
                        id: &voice_id,
                        file,
                        resolved_file: Some(&resolved_file),
                        gain: *gain,
                        pan: *pan,
                        fade_ms: fade_time.unwrap_or(0),
                    },
                );
                true
            }
            Event::StopAllSounds { .. } => {
                self.audio.stop_all_sounds();
                hm::emit(Kind::AudioStopAll, hm::EmptyPayload {});
                true
            }
            Event::SoundFinishHandler {
                id,
                file,
                label,
                call,
                handler,
            } => {
                self.audio.set_sound_finish_handler(
                    if id.is_empty() {
                        None
                    } else {
                        Some(id.as_str())
                    },
                    SoundFinishHandler {
                        file: file.clone(),
                        label: label.clone(),
                        call: *call,
                        handler: handler.clone(),
                    },
                );
                true
            }
            Event::SoundFinishHandlerDel { id } => {
                self.audio.remove_sound_finish_handler(if id.is_empty() {
                    None
                } else {
                    Some(id.as_str())
                });
                true
            }
            _ => false,
        }
    }

    fn apply_video_event(&mut self, event: &Event) -> bool {
        match event {
            // TODO(video): skip=2（仅右键菜单方式跳过）目前与 1 同样按可跳过
            // 转发给宿主；mode（Windows VMR/EVR）对 Flutter 宿主不适用，忽略。
            Event::VideoPlay {
                id,
                file,
                skip,
                loop_play,
                delay_margin_ms,
                mode: _,
            } => {
                crate::core_debug!("[Video] VideoPlay: file={}, id={:?}", file, id);
                let resolved_file = self.resolve_magic_media_path(file);
                let skippable = *skip != 0;
                let config = VideoConfig {
                    file: file.clone(),
                    skippable,
                    loop_play: *loop_play,
                    delay_margin_ms: *delay_margin_ms,
                };
                match id {
                    Some(layer_id) => {
                        self.video.play_layer(layer_id, &config);
                        self.bind_video_layer_texture(layer_id);
                    }
                    None => self.video.play_fullscreen(&config),
                }
                hm::emit(
                    Kind::VideoPlay,
                    hm::VideoPlay {
                        id: id.as_deref(),
                        file,
                        resolved_file: Some(&resolved_file),
                        skippable,
                        loop_play: *loop_play,
                    },
                );
                true
            }
            // setonvideofinish：id=Some(层ID) 按图层登记完成处理器，
            // id=None 为全屏/全局处理器（对齐音频 se_finish_handlers 的按 ID 派发）。
            Event::VideoFinishHandler {
                id,
                file,
                label,
                call,
                handler,
            } => {
                self.video.set_finish_handler(
                    id.as_deref(),
                    VideoFinishHandler {
                        file: file.clone(),
                        label: label.clone(),
                        call: *call,
                        handler: handler.clone(),
                    },
                );
                true
            }
            // delonvideofinish：按 id 解除对应图层处理器；id=None 清全局处理器。
            Event::VideoFinishHandlerDel { id } => {
                self.video.remove_finish_handler(id.as_deref());
                true
            }
            _ => false,
        }
    }

    pub(super) fn apply_system_audio_volume(&mut self) {
        /// Artemis 音量变量是 0-1000 的整数刻度。
        const VOLUME_SCALE: f32 = 1000.0;

        let vars = self.interpreter.variables_handle();
        let vars = vars.lock().unwrap();
        let read_volume = |key: &str| {
            vars.get(key).and_then(|value| match value {
                asb_interpreter::Value::Int(v) => Some((*v as f32 / VOLUME_SCALE).clamp(0.0, 1.0)),
                _ => None,
            })
        };
        let bgm_volume = read_volume("s.bgmvol");
        let se_volume = read_volume("s.sevol");
        drop(vars);

        // 只在值变化时下发，避免每帧向宿主重发相同命令。
        if let Some(v) = bgm_volume
            && self.last_system_volume.0 != Some(v)
        {
            self.last_system_volume.0 = Some(v);
            self.audio.set_bgm_volume(v);
            hm::emit(
                Kind::AudioSetVolume,
                hm::AudioSetVolume {
                    channel: "bgm",
                    value: v,
                },
            );
        }
        if let Some(v) = se_volume
            && self.last_system_volume.1 != Some(v)
        {
            self.last_system_volume.1 = Some(v);
            self.audio.set_se_volume(v);
            hm::emit(
                Kind::AudioSetVolume,
                hm::AudioSetVolume {
                    channel: "se",
                    value: v,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ab_loop_file;

    #[test]
    fn ab_loop_naming_convention_maps_a_segment_to_b_segment() {
        // 文档 splay.md：foo_a.ogg（引导段）→ foo_b.ogg（循环段）
        assert_eq!(ab_loop_file("foo_a.ogg"), Some("foo_b.ogg".to_string()));
        assert_eq!(
            ab_loop_file("bgm/theme_a.ogg"),
            Some("bgm/theme_b.ogg".to_string())
        );
        // magic path / 无扩展名也按词尾 _a 处理
        assert_eq!(ab_loop_file(":bgm/foo_a"), Some(":bgm/foo_b".to_string()));

        // 非 _a 结尾不构成 A-B 循环
        assert_eq!(ab_loop_file("foo.ogg"), None);
        assert_eq!(ab_loop_file("foo_b.ogg"), None);
        // 目录名里的点不能被当成扩展名分隔符
        assert_eq!(
            ab_loop_file("dir.v2/foo_a.ogg"),
            Some("dir.v2/foo_b.ogg".to_string())
        );
    }
}
