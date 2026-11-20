use super::{AudioChannelSnapshot, AudioSnapshot};
use crate::audio::{AudioBackend, BgmConfig, SeConfig, SoundChannel};

impl From<&SoundChannel> for AudioChannelSnapshot {
    fn from(channel: &SoundChannel) -> Self {
        Self {
            id: channel.id.clone(),
            file: channel.file.clone(),
            loop_play: channel.loop_play,
            gain: channel.raw_gain,
            pan: channel.raw_pan,
            skippable: channel.skippable,
        }
    }
}

impl AudioSnapshot {
    pub fn from_audio(audio: &dyn AudioBackend) -> Self {
        let state = audio.audio_state();
        let bgm = state
            .bgm_channel
            .as_ref()
            .filter(|channel| channel.playing)
            .map(AudioChannelSnapshot::from);
        let mut se: Vec<_> = state
            .se_channels
            .values()
            .filter(|channel| channel.playing)
            .map(AudioChannelSnapshot::from)
            .collect();
        let mut voice: Vec<_> = state
            .voice_channels
            .values()
            .filter(|channel| channel.playing)
            .map(AudioChannelSnapshot::from)
            .collect();
        se.sort_by(|a, b| a.id.cmp(&b.id));
        voice.sort_by(|a, b| a.id.cmp(&b.id));
        Self { bgm, se, voice }
    }

    pub fn restore_into(&self, audio: &mut dyn AudioBackend) {
        if let Some(bgm) = &self.bgm {
            audio.play_bgm(
                &bgm.file,
                &BgmConfig {
                    loop_play: bgm.loop_play,
                    gain: Some(bgm.gain),
                    pan: Some(bgm.pan),
                    fade_in_ms: 0,
                    buffer_size: None,
                },
            );
        }
        for se in &self.se {
            audio.play_se(
                &se.id,
                &se.file,
                &SeConfig {
                    loop_play: se.loop_play,
                    gain: Some(se.gain),
                    pan: Some(se.pan),
                    fade_in_ms: 0,
                    buffer_size: None,
                    skippable: se.skippable,
                },
            );
        }
        for voice in &self.voice {
            audio.play_voice(
                &voice.id,
                &voice.file,
                &SeConfig {
                    loop_play: voice.loop_play,
                    gain: Some(voice.gain),
                    pan: Some(voice.pan),
                    fade_in_ms: 0,
                    buffer_size: None,
                    skippable: voice.skippable,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::AudioStateBackend;

    #[test]
    fn audio_snapshot_replays_active_channels_from_start() {
        let mut audio = AudioStateBackend::new();
        audio.play_bgm(
            "bgm/scene.ogg",
            &BgmConfig {
                loop_play: true,
                gain: Some(700),
                pan: Some(-100),
                fade_in_ms: 1000,
                buffer_size: None,
            },
        );
        audio.play_se(
            "amb",
            "se/wind.ogg",
            &SeConfig {
                loop_play: true,
                gain: Some(400),
                pan: Some(100),
                fade_in_ms: 500,
                buffer_size: None,
                skippable: true,
            },
        );
        audio.advance(250);

        let snapshot = AudioSnapshot::from_audio(&audio);
        assert_eq!(snapshot.bgm.as_ref().unwrap().file, "bgm/scene.ogg");
        assert_eq!(snapshot.bgm.as_ref().unwrap().gain, 700);
        assert_eq!(snapshot.se[0].file, "se/wind.ogg");
        assert!(snapshot.se[0].skippable);

        let mut restored = AudioStateBackend::new();
        snapshot.restore_into(&mut restored);

        let state = restored.audio_state();
        let bgm = state.bgm_channel.as_ref().unwrap();
        assert_eq!(bgm.file, "bgm/scene.ogg");
        assert_eq!(bgm.current_gain, SoundChannel::gain_to_linear(700));
        assert!(bgm.fade.is_none());
        let se = state.se_channels.get("amb").unwrap();
        assert_eq!(se.file, "se/wind.ogg");
        assert_eq!(se.current_gain, SoundChannel::gain_to_linear(400));
        assert!(se.skippable);
        assert!(se.fade.is_none());
    }
}
