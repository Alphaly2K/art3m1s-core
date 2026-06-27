use super::CoreRuntime;
use crate::audio::{BgmConfig, SeConfig, SoundFinishHandler};
use crate::host_media::{self as hm, HostMediaCommandKind as Kind};
use crate::runtime::input;
use crate::save::AudioSnapshot;
use crate::video::{VideoConfig, VideoFinishHandler};
use asb_interpreter::Event;
use std::collections::HashMap;

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
        self.audio.stop_all_sounds();
        self.video.stop_all_videos();
        hm::emit(Kind::AudioStopAll, hm::EmptyPayload {});
        hm::emit(Kind::VideoStopAll, hm::EmptyPayload {});
    }

    pub(super) fn restore_audio_snapshot(&mut self, snapshot: &AudioSnapshot) {
        snapshot.restore_into(self.audio.as_mut());

        if let Some(bgm) = &snapshot.bgm {
            let resolved_file = self.resolve_magic_media_path(&bgm.file);
            hm::emit(
                Kind::AudioBgmPlay,
                hm::BgmPlay {
                    file: &bgm.file,
                    resolved_file: Some(&resolved_file),
                    loop_play: bgm.loop_play,
                    gain: Some(bgm.gain),
                    pan: Some(bgm.pan),
                    fade_ms: 0,
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

    pub(super) fn advance_media_and_enqueue_finish_handlers(&mut self, delta_ms: u64) {
        self.audio.advance(delta_ms);
        self.video.advance(delta_ms);

        if !crate::ffi::media_command_callback_registered() {
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

            let video_finish_events = self.video.poll_finish_events();
            for event in video_finish_events {
                self.video_finished
                    .store(true, std::sync::atomic::Ordering::SeqCst);

                // Enqueue handler tags if registered
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

    pub fn notify_video_finished(&mut self, id: Option<&str>) {
        let handler = self.video.video_state().finish_handler.clone();
        match id {
            Some(layer_id) => {
                self.video.stop_layer(layer_id);
            }
            None => {
                self.video.stop_fullscreen();
            }
        }
        // Discard any queued fallback completion from the internal state machine.
        let _ = self.video.poll_finish_events();

        self.video_finished
            .store(true, std::sync::atomic::Ordering::SeqCst);
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

    fn apply_audio_event(&mut self, event: &Event) -> bool {
        match event {
            Event::BgmPlay {
                file,
                loop_play,
                gain,
                pan,
                fade_time,
            } => {
                let resolved_file = self.resolve_magic_media_path(file);
                self.audio.play_bgm(
                    file,
                    &BgmConfig {
                        loop_play: *loop_play,
                        gain: *gain,
                        pan: *pan,
                        fade_in_ms: fade_time.unwrap_or(0),
                        buffer_size: None,
                    },
                );
                hm::emit(
                    Kind::AudioBgmPlay,
                    hm::BgmPlay {
                        file,
                        resolved_file: Some(&resolved_file),
                        loop_play: *loop_play,
                        gain: *gain,
                        pan: *pan,
                        fade_ms: fade_time.unwrap_or(0),
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
            Event::BgmPan { pan } => {
                self.audio.pan_bgm(*pan, 0);
                hm::emit(Kind::AudioBgmPan, hm::BgmPan { pan: *pan });
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
            Event::SePan { id, pan } => {
                self.audio.pan_se(id, *pan, 0);
                hm::emit(Kind::AudioSePan, hm::SePan { id, pan: *pan });
                true
            }
            Event::VoicePlay {
                file,
                gain,
                pan,
                fade_time,
            } => {
                let resolved_file = self.resolve_magic_media_path(file);
                self.voice_serial = self.voice_serial.saturating_add(1);
                let voice_id = format!("voice:{}", self.voice_serial);
                self.audio.play_voice(
                    &voice_id,
                    file,
                    &SeConfig {
                        loop_play: false,
                        gain: *gain,
                        pan: *pan,
                        fade_in_ms: fade_time.unwrap_or(0),
                        buffer_size: None,
                        skippable: false,
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
            Event::VideoPlay {
                id,
                file,
                skip,
                loop_play,
            } => {
                crate::core_debug!("[Video] VideoPlay: file={}, id={:?}", file, id);
                let resolved_file = self.resolve_magic_media_path(file);
                let config = VideoConfig {
                    file: file.clone(),
                    skippable: *skip,
                    loop_play: *loop_play,
                    delay_margin_ms: None,
                };
                match id {
                    Some(layer_id) => self.video.play_layer(layer_id, &config),
                    None => self.video.play_fullscreen(&config),
                }
                hm::emit(
                    Kind::VideoPlay,
                    hm::VideoPlay {
                        id: id.as_deref(),
                        file,
                        resolved_file: Some(&resolved_file),
                        skippable: *skip,
                        loop_play: *loop_play,
                    },
                );
                true
            }
            Event::VideoFinishHandler {
                file,
                label,
                call,
                handler,
            } => {
                self.video.set_finish_handler(VideoFinishHandler {
                    file: file.clone(),
                    label: label.clone(),
                    call: *call,
                    handler: handler.clone(),
                });
                true
            }
            Event::VideoFinishHandlerDel => {
                self.video.remove_finish_handler();
                true
            }
            _ => false,
        }
    }

    pub(super) fn apply_system_audio_volume(&mut self) {
        let vars = self.interpreter.variables_handle();
        let vars = vars.lock().unwrap();
        let bgm_volume = vars.get("s.bgmvol").and_then(|value| match value {
            asb_interpreter::Value::Int(v) => Some((*v as f32 / 1000.0).clamp(0.0, 1.0)),
            _ => None,
        });
        let se_volume = vars.get("s.sevol").and_then(|value| match value {
            asb_interpreter::Value::Int(v) => Some((*v as f32 / 1000.0).clamp(0.0, 1.0)),
            _ => None,
        });
        drop(vars);

        if let Some(v) = bgm_volume {
            self.audio.set_bgm_volume(v);
            hm::emit(
                Kind::AudioSetVolume,
                hm::AudioSetVolume {
                    channel: "bgm",
                    value: v,
                },
            );
        }
        if let Some(v) = se_volume {
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
