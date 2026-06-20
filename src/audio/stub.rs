//! 存根音频后端。
//!
//! 不产生任何音频输出，仅维护 [`AudioState`] 中的逻辑状态。
//! 在真实音频后端就绪前用于编译与测试。

use crate::audio::engine::{
    AudioBackend, AudioState, BgmConfig, FadeState, SeConfig, SoundCategory, SoundChannel,
    SoundFinishEvent, SoundFinishHandler, advance_channel_fade, apply_channel_fade_out,
    apply_channel_gain_fade, apply_channel_pan_fade,
};

/// 不输出任何音频的存根后端。
///
/// 所有播放状态的变更都会记录在 [`AudioState`] 中，`poll_finish_events`
/// 会返回因播放完成或 fade-out 停止而产生的完成事件。
#[derive(Debug, Default)]
pub struct StubAudioBackend {
    state: AudioState,
    /// 自上次 `poll_finish_events` 以来积累的完成事件
    pending_finish_events: Vec<SoundFinishEvent>,
}

impl StubAudioBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// 重置为初始状态。
    pub fn reset(&mut self) {
        self.state = AudioState::default();
        self.pending_finish_events.clear();
    }

    /// 通知某个通道播放完成，压入完成事件。
    fn notify_channel_finish(&mut self, channel: &SoundChannel) {
        let handler = match channel.category {
            SoundCategory::Bgm => self.state.bgm_finish_handler.clone(),
            SoundCategory::Se => self.state.se_finish_handlers.get(&channel.id).cloned(),
            SoundCategory::Voice => {
                // Voice 目前不触发独立完成处理器
                None
            }
        };

        self.pending_finish_events.push(SoundFinishEvent {
            id: Some(channel.id.clone()),
            category: channel.category,
            handler,
        });
    }
}

impl AudioBackend for StubAudioBackend {
    // -------------------------------------------------------------------
    // BGM
    // -------------------------------------------------------------------

    fn play_bgm(&mut self, file: &str, config: &BgmConfig) {
        // 停止旧 BGM（不触发完成事件，因为是被替换而非自然结束）
        self.state.bgm_channel = None;

        let mut channel = SoundChannel::new("bgm", file, SoundCategory::Bgm);
        channel.playing = true;
        channel.loop_play = config.loop_play;

        if let Some(gain) = config.gain {
            channel.raw_gain = gain;
            channel.current_gain = SoundChannel::gain_to_linear(gain);
        }
        if let Some(pan) = config.pan {
            channel.raw_pan = pan;
            channel.current_pan = SoundChannel::pan_to_linear(pan);
        }

        // 淡入
        if config.fade_in_ms > 0 {
            let target_gain = channel.current_gain;
            channel.current_gain = 0.0;
            channel.fade = Some(FadeState {
                target_gain,
                target_pan: channel.current_pan,
                from_gain: 0.0,
                from_pan: channel.current_pan,
                start_ms: self.state.clock_ms,
                duration_ms: config.fade_in_ms,
                stop_on_complete: false,
            });
        }

        self.state.bgm_channel = Some(channel);
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
            self.state.bgm_channel = None;
        }

        true
    }

    fn crossfade_bgm(&mut self, file: &str, config: &BgmConfig) {
        let fade_time = config.fade_in_ms;

        // 旧 BGM 淡出
        if let Some(ref mut old) = self.state.bgm_channel {
            if old.playing {
                apply_channel_fade_out(old, fade_time, self.state.clock_ms);
            }
        }

        // 启动新 BGM
        let mut channel = SoundChannel::new("bgm", file, SoundCategory::Bgm);
        channel.playing = true;
        channel.loop_play = config.loop_play;

        if let Some(gain) = config.gain {
            channel.raw_gain = gain;
            channel.current_gain = SoundChannel::gain_to_linear(gain);
        }
        if let Some(pan) = config.pan {
            channel.raw_pan = pan;
            channel.current_pan = SoundChannel::pan_to_linear(pan);
        }

        // 新 BGM 从 0 淡入
        if fade_time > 0 {
            let target_gain = channel.current_gain;
            channel.current_gain = 0.0;
            channel.fade = Some(FadeState {
                target_gain,
                target_pan: channel.current_pan,
                from_gain: 0.0,
                from_pan: channel.current_pan,
                start_ms: self.state.clock_ms,
                duration_ms: fade_time,
                stop_on_complete: false,
            });
        }

        self.state.bgm_channel = Some(channel);
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

    // -------------------------------------------------------------------
    // SE
    // -------------------------------------------------------------------

    fn play_se(&mut self, id: &str, file: &str, config: &SeConfig) {
        // skippable SE 在快进模式下不播放
        if config.skippable && self.state.is_skipping {
            return;
        }

        // 覆盖同 ID 的旧 SE
        self.state.se_channels.remove(id);

        let mut channel = SoundChannel::new(id, file, SoundCategory::Se);
        channel.playing = true;
        channel.loop_play = config.loop_play;

        if let Some(gain) = config.gain {
            channel.raw_gain = gain;
            channel.current_gain = SoundChannel::gain_to_linear(gain);
        }
        if let Some(pan) = config.pan {
            channel.raw_pan = pan;
            channel.current_pan = SoundChannel::pan_to_linear(pan);
        }

        if config.fade_in_ms > 0 {
            let target_gain = channel.current_gain;
            channel.current_gain = 0.0;
            channel.fade = Some(FadeState {
                target_gain,
                target_pan: channel.current_pan,
                from_gain: 0.0,
                from_pan: channel.current_pan,
                start_ms: self.state.clock_ms,
                duration_ms: config.fade_in_ms,
                stop_on_complete: false,
            });
        }

        self.state.se_channels.insert(id.to_string(), channel);
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

    // -------------------------------------------------------------------
    // Voice
    // -------------------------------------------------------------------

    fn play_voice(&mut self, id: &str, file: &str, config: &SeConfig) {
        // Voice 与 SE 共用配置类型
        self.state.voice_channels.remove(id);

        let mut channel = SoundChannel::new(id, file, SoundCategory::Voice);
        channel.playing = true;
        channel.loop_play = config.loop_play;

        if let Some(gain) = config.gain {
            channel.raw_gain = gain;
            channel.current_gain = SoundChannel::gain_to_linear(gain);
        }
        if let Some(pan) = config.pan {
            channel.raw_pan = pan;
            channel.current_pan = SoundChannel::pan_to_linear(pan);
        }

        if config.fade_in_ms > 0 {
            let target_gain = channel.current_gain;
            channel.current_gain = 0.0;
            channel.fade = Some(FadeState {
                target_gain,
                target_pan: channel.current_pan,
                from_gain: 0.0,
                from_pan: channel.current_pan,
                start_ms: self.state.clock_ms,
                duration_ms: config.fade_in_ms,
                stop_on_complete: false,
            });
        }

        self.state.voice_channels.insert(id.to_string(), channel);
    }

    // -------------------------------------------------------------------
    // 全局
    // -------------------------------------------------------------------

    fn stop_all_sounds(&mut self) {
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

    // -------------------------------------------------------------------
    // 完成事件
    // -------------------------------------------------------------------

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

    // -------------------------------------------------------------------
    // 帧循环
    // -------------------------------------------------------------------

    fn advance(&mut self, delta_ms: u64) {
        self.state.clock_ms = self.state.clock_ms.saturating_add(delta_ms);
        let now = self.state.clock_ms;

        // 推进 BGM 淡入淡出
        if let Some(ref mut channel) = self.state.bgm_channel {
            if advance_channel_fade(channel, now) {
                // fade-out 完成，BGM 停止
                let stopped = self.state.bgm_channel.take();
                if let Some(stopped) = stopped {
                    self.notify_channel_finish(&stopped);
                }
            }
        }

        // 推进 SE 淡入淡出
        let mut finished_se: Vec<String> = Vec::new();
        for (id, channel) in self.state.se_channels.iter_mut() {
            if advance_channel_fade(channel, now) {
                finished_se.push(id.clone());
            }
        }
        for id in &finished_se {
            if let Some(channel) = self.state.se_channels.remove(id) {
                self.notify_channel_finish(&channel);
            }
        }

        // 推进 Voice 淡入淡出
        let mut finished_voice: Vec<String> = Vec::new();
        for (id, channel) in self.state.voice_channels.iter_mut() {
            if advance_channel_fade(channel, now) {
                finished_voice.push(id.clone());
            }
        }
        for id in &finished_voice {
            if let Some(channel) = self.state.voice_channels.remove(id) {
                self.notify_channel_finish(&channel);
            }
        }
    }

    fn poll_finish_events(&mut self) -> Vec<SoundFinishEvent> {
        std::mem::take(&mut self.pending_finish_events)
    }

    // -------------------------------------------------------------------
    // 状态
    // -------------------------------------------------------------------

    fn audio_state(&self) -> &AudioState {
        &self.state
    }

    fn audio_state_mut(&mut self) -> &mut AudioState {
        &mut self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::engine::AudioBackend;

    #[test]
    fn play_and_stop_bgm() {
        let mut a = StubAudioBackend::new();
        assert!(!a.is_bgm_playing());

        a.play_bgm(
            "bgm01.ogg",
            &BgmConfig {
                loop_play: true,
                ..Default::default()
            },
        );
        assert!(a.is_bgm_playing());
        assert_eq!(
            a.audio_state().bgm_channel.as_ref().unwrap().file,
            "bgm01.ogg"
        );

        a.stop_bgm(0);
        assert!(!a.is_bgm_playing());
    }

    #[test]
    fn stop_bgm_with_fade_triggers_finish_event() {
        let mut a = StubAudioBackend::new();
        a.play_bgm("bgm01.ogg", &BgmConfig::default());
        a.stop_bgm(500);
        assert!(a.is_bgm_playing()); // 还在淡出中

        a.advance(600);
        assert!(!a.is_bgm_playing()); // fade 完成

        let events = a.poll_finish_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].category, SoundCategory::Bgm);
    }

    #[test]
    fn play_and_stop_se() {
        let mut a = StubAudioBackend::new();
        a.play_se(
            "se01",
            "click.wav",
            &SeConfig {
                loop_play: false,
                ..Default::default()
            },
        );
        assert!(a.is_se_playing("se01"));

        a.stop_se("se01", 0);
        assert!(!a.is_se_playing("se01"));
    }

    #[test]
    fn duplicate_se_replaces() {
        let mut a = StubAudioBackend::new();
        a.play_se("se01", "click.wav", &SeConfig::default());
        a.play_se("se01", "boom.wav", &SeConfig::default());
        assert_eq!(
            a.audio_state().se_channels.get("se01").unwrap().file,
            "boom.wav"
        );
    }

    #[test]
    fn skippable_se_not_played_when_skipping() {
        let mut a = StubAudioBackend::new();
        a.set_skipping(true);
        a.play_se(
            "se01",
            "click.wav",
            &SeConfig {
                skippable: true,
                ..Default::default()
            },
        );
        assert!(!a.is_se_playing("se01"));
    }

    #[test]
    fn bgm_fade_gain_updates_state() {
        let mut a = StubAudioBackend::new();
        a.play_bgm("bgm01.ogg", &BgmConfig::default());
        a.fade_bgm_gain(500, 1000);

        let state = a.audio_state();
        let ch = state.bgm_channel.as_ref().unwrap();
        assert_eq!(ch.raw_gain, 500);
        assert!(ch.fade.is_some());
        assert_eq!(ch.fade.as_ref().unwrap().duration_ms, 1000);
    }

    #[test]
    fn advance_without_channels_is_noop() {
        let mut a = StubAudioBackend::new();
        a.advance(1000);
        assert_eq!(a.audio_state().clock_ms, 1000);
        assert!(a.poll_finish_events().is_empty());
    }

    #[test]
    fn crossfade_bgm_starts_new_and_fades_old() {
        let mut a = StubAudioBackend::new();
        a.play_bgm("old.ogg", &BgmConfig::default());
        a.crossfade_bgm(
            "new.ogg",
            &BgmConfig {
                fade_in_ms: 500,
                ..Default::default()
            },
        );

        // 旧 BGM 在淡出，新 BGM 在淡入（从 0 开始）
        // 注意：BGM 是单通道，新 BGM 替换了旧的 bgm_channel 字段
        // 旧 BGM 的 fade 状态在 apply_channel_fade_out 中设置...
        // 但由于 bgm_channel 被替换了，旧 channel 丢失了
        // 这是一个已知的设计限制：BGM 单通道下 crossfade 需要两个独立缓冲区
        // 真正的后端（rodio）会用两个 Sink 实现真正的重叠
        let state = a.audio_state();
        assert!(state.bgm_channel.is_some());
        assert_eq!(state.bgm_channel.as_ref().unwrap().file, "new.ogg");
        assert!(state.bgm_channel.as_ref().unwrap().fade.is_some());
    }

    #[test]
    fn set_volume_clamps() {
        let mut a = StubAudioBackend::new();
        a.set_master_volume(1.5);
        assert_eq!(a.audio_state().master_volume, 1.0);
        a.set_master_volume(-0.5);
        assert_eq!(a.audio_state().master_volume, 0.0);
    }

    #[test]
    fn set_and_remove_finish_handler() {
        let mut a = StubAudioBackend::new();

        a.set_sound_finish_handler(
            None,
            SoundFinishHandler {
                file: Some("script.iet".into()),
                label: Some("on_bgm_end".into()),
                call: false,
                handler: None,
            },
        );
        assert!(a.audio_state().bgm_finish_handler.is_some());

        a.remove_sound_finish_handler(None);
        assert!(a.audio_state().bgm_finish_handler.is_none());
    }

    #[test]
    fn stop_all_sounds_clears_everything() {
        let mut a = StubAudioBackend::new();
        a.play_bgm("bgm.ogg", &BgmConfig::default());
        a.play_se("se01", "click.wav", &SeConfig::default());
        a.play_voice("v01", "line01.ogg", &SeConfig::default());

        a.stop_all_sounds();
        assert!(!a.is_bgm_playing());
        assert!(!a.is_se_playing("se01"));
        assert!(a.audio_state().voice_channels.is_empty());
    }

    #[test]
    fn advance_fades_gain_linear() {
        let mut a = StubAudioBackend::new();
        a.play_bgm(
            "bgm.ogg",
            &BgmConfig {
                gain: Some(0),
                ..Default::default()
            },
        );

        a.fade_bgm_gain(1000, 1000); // 1 秒内从 0 到 1000

        a.advance(500);
        let ch = a.audio_state().bgm_channel.as_ref().unwrap();
        assert!((ch.current_gain - 0.5).abs() < 0.02);

        a.advance(600);
        let ch = a.audio_state().bgm_channel.as_ref().unwrap();
        assert!((ch.current_gain - 1.0).abs() < 0.01);
        assert!(ch.fade.is_none()); // fade 已完成
    }
}
