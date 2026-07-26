//! Host media command protocol.
//!
//! Core only produces media commands and consumes completion notifications.
//! Audio sample transport is intentionally not part of this Dart-facing FFI
//! path: stable audio should be implemented by the host/native side as an audio
//! sink (ring buffer or native pull callback), while Dart controls lifecycle.
//! Video decode is also host-owned. Fullscreen display stays in the host;
//! decoded layer frames enter core through the native frame-upload FFI.

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
        }
    }
}

pub fn emit<T: Serialize>(kind: HostMediaCommandKind, payload: T) {
    let payload = serde_json::to_value(payload).unwrap_or_else(|e| {
        crate::core_warn!(
            "[host-media] {} payload 序列化失败，降级为空对象: {e}",
            kind.as_str()
        );
        json!({})
    });
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
    /// A-B 循环的循环段文件（`foo_a.ogg`→`foo_b.ogg` 命名约定）：
    /// 宿主播完引导段（file）后应无限循环该文件；None=普通播放。
    pub loop_file: Option<&'a str>,
    /// 循环段文件的 magic path 解析结果。
    pub resolved_loop_file: Option<&'a str>,
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
    /// 渐变时间（毫秒），0=立即切换
    pub time_ms: u64,
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
    /// 渐变时间（毫秒），0=立即切换
    pub time_ms: u64,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgm_play_payload_carries_ab_loop_segment() {
        // splay 的 A-B 循环：foo_a（引导段）+ foo_b（循环段）随命令一起下发宿主
        let value = serde_json::to_value(BgmPlay {
            file: "bgm/foo_a.ogg",
            resolved_file: Some("sound/bgm/foo_a.ogg"),
            loop_play: true,
            gain: None,
            pan: None,
            fade_ms: 0,
            loop_file: Some("bgm/foo_b.ogg"),
            resolved_loop_file: Some("sound/bgm/foo_b.ogg"),
        })
        .unwrap();
        assert_eq!(value["loop_file"], "bgm/foo_b.ogg");
        assert_eq!(value["resolved_loop_file"], "sound/bgm/foo_b.ogg");

        // 普通播放：字段为 null，宿主按整曲循环处理
        let value = serde_json::to_value(BgmPlay {
            file: "bgm/theme.ogg",
            resolved_file: None,
            loop_play: true,
            gain: None,
            pan: None,
            fade_ms: 0,
            loop_file: None,
            resolved_loop_file: None,
        })
        .unwrap();
        assert!(value["loop_file"].is_null());
    }

    #[test]
    fn pan_wire_payloads_carry_fade_time() {
        // [span]/[sepan] 的 time 参数经 time_ms 字段透传给宿主渐变
        let value = serde_json::to_value(BgmPan {
            pan: -1000,
            time_ms: 500,
        })
        .unwrap();
        assert_eq!(value["pan"], -1000);
        assert_eq!(value["time_ms"], 500);

        let value = serde_json::to_value(SePan {
            id: "1.80",
            pan: 1000,
            time_ms: 0,
        })
        .unwrap();
        assert_eq!(value["id"], "1.80");
        assert_eq!(value["pan"], 1000);
        assert_eq!(value["time_ms"], 0);
    }

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
