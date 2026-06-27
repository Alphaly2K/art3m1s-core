//! Host media command protocol.
//!
//! Core only produces media commands and consumes completion notifications.
//! Audio sample transport is intentionally not part of this Dart-facing FFI
//! path: stable audio should be implemented by the host/native side as an audio
//! sink (ring buffer or native pull callback), while Dart controls lifecycle.
//! Video decode/display is also host-owned; core keeps only video state and
//! synchronization points.

use serde::Serialize;
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostMediaCommandKind {
    AudioSetVolume,
    AudioBgmPlay,
    AudioBgmStop,
    AudioBgmFade,
    AudioBgmPan,
    AudioBgmCrossfade,
    AudioSePlay,
    AudioSeStop,
    AudioSeFade,
    AudioSePan,
    AudioVoicePlay,
    AudioStopAll,
    VideoPlay,
    VideoStopAll,
    VideoLayerFrame,
}

impl HostMediaCommandKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AudioSetVolume => "audio_set_volume",
            Self::AudioBgmPlay => "audio_bgm_play",
            Self::AudioBgmStop => "audio_bgm_stop",
            Self::AudioBgmFade => "audio_bgm_fade",
            Self::AudioBgmPan => "audio_bgm_pan",
            Self::AudioBgmCrossfade => "audio_bgm_crossfade",
            Self::AudioSePlay => "audio_se_play",
            Self::AudioSeStop => "audio_se_stop",
            Self::AudioSeFade => "audio_se_fade",
            Self::AudioSePan => "audio_se_pan",
            Self::AudioVoicePlay => "audio_voice_play",
            Self::AudioStopAll => "audio_stop_all",
            Self::VideoPlay => "video_play",
            Self::VideoStopAll => "video_stop_all",
            Self::VideoLayerFrame => "video_layer_frame",
        }
    }
}

pub fn emit<T: Serialize>(kind: HostMediaCommandKind, payload: T) {
    let payload = serde_json::to_value(payload).unwrap_or_else(|_| json!({}));
    crate::ffi::emit_media_command(kind.as_str(), payload);
}

#[derive(Debug, Serialize)]
pub struct EmptyPayload {}

#[derive(Debug, Serialize)]
pub struct AudioSetVolume<'a> {
    pub channel: &'a str,
    pub value: f32,
}

#[derive(Debug, Serialize)]
pub struct BgmPlay<'a> {
    pub file: &'a str,
    pub resolved_file: Option<&'a str>,
    #[serde(rename = "loop")]
    pub loop_play: bool,
    pub gain: Option<i32>,
    pub pan: Option<i32>,
    pub fade_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct BgmStop {
    pub fade_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct BgmFade {
    pub gain: i32,
    pub time_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct BgmPan {
    pub pan: i32,
}

#[derive(Debug, Serialize)]
pub struct BgmCrossfade<'a> {
    pub file: &'a str,
    pub resolved_file: Option<&'a str>,
    #[serde(rename = "loop")]
    pub loop_play: bool,
    pub gain: Option<i32>,
    pub pan: Option<i32>,
    pub time_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct SePlay<'a> {
    pub id: &'a str,
    pub file: &'a str,
    pub resolved_file: Option<&'a str>,
    #[serde(rename = "loop")]
    pub loop_play: bool,
    pub gain: Option<i32>,
    pub pan: Option<i32>,
    pub fade_ms: u64,
    pub skippable: bool,
}

#[derive(Debug, Serialize)]
pub struct SeStop<'a> {
    pub id: &'a str,
    pub fade_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct SeFade<'a> {
    pub id: &'a str,
    pub gain: i32,
    pub time_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct SePan<'a> {
    pub id: &'a str,
    pub pan: i32,
}

#[derive(Debug, Serialize)]
pub struct VoicePlay<'a> {
    pub id: &'a str,
    pub file: &'a str,
    pub resolved_file: Option<&'a str>,
    pub gain: Option<i32>,
    pub pan: Option<i32>,
    pub fade_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct VideoPlay<'a> {
    pub id: Option<&'a str>,
    pub file: &'a str,
    pub resolved_file: Option<&'a str>,
    pub skippable: bool,
    #[serde(rename = "loop")]
    pub loop_play: bool,
}

/// Host-to-renderer handoff marker for video layers.
///
/// Core may emit this once it supports host-provided layer textures.  The
/// payload intentionally describes a texture/resource handle, not decoded PCM
/// or raw video bytes moving through Dart.
#[derive(Debug, Serialize)]
pub struct VideoLayerFrame<'a> {
    pub id: &'a str,
    pub texture: &'a str,
    pub width: u32,
    pub height: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_play_wire_payload_keeps_frontend_field_names() {
        let value = serde_json::to_value(VideoPlay {
            id: Some("movie"),
            file: ":mv/opening",
            resolved_file: Some("movie/opening"),
            skippable: true,
            loop_play: false,
        })
        .unwrap();

        assert_eq!(value["id"], "movie");
        assert_eq!(value["file"], ":mv/opening");
        assert_eq!(value["resolved_file"], "movie/opening");
        assert_eq!(value["skippable"], true);
        assert_eq!(value["loop"], false);
        assert!(value.get("loop_play").is_none());
    }
}
