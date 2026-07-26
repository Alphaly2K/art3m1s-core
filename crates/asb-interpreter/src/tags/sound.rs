//! 音频标签处理器
//!
//! 实现 splay, sstop, sfade, span, sxfade, seplay, sestop, sefade, sepan, voice 等音频相关标签。

use super::{ExecutionContext, TagHandler, TagResult};
use crate::error::Result;
use crate::event::Event;

/// [splay] 播放 BGM
pub struct SplayHandler;

impl TagHandler for SplayHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let file = ctx.resolve_param("file")?.as_string();
        let loop_play = ctx
            .instruction
            .get("loop")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(1)
            != 0;
        let gain = ctx
            .instruction
            .get("gain")
            .and_then(|v| v.parse::<i32>().ok());
        let pan = ctx
            .instruction
            .get("pan")
            .and_then(|v| v.parse::<i32>().ok());
        let time = ctx
            .instruction
            .get("time")
            .and_then(|v| v.parse::<u64>().ok());
        // buffer：缓冲毫秒数，-1 表示内存播放（仅 Windows/WASM），缺省默认缓冲
        let buffer = ctx
            .instruction
            .get("buffer")
            .and_then(|v| v.parse::<i32>().ok());

        Ok(TagResult::Emit(Event::BgmPlay {
            file,
            loop_play,
            gain,
            pan,
            fade_time: time,
            buffer,
        }))
    }
}

/// [sstop] 停止 BGM
pub struct SstopHandler;

impl TagHandler for SstopHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let time = ctx
            .instruction
            .get("time")
            .and_then(|v| v.parse::<u64>().ok());
        Ok(TagResult::Emit(Event::BgmStop { fade_time: time }))
    }
}

/// [sfade] BGM 音量渐变
pub struct SfadeHandler;

impl TagHandler for SfadeHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let gain = ctx.resolve_param("gain")?.as_int().unwrap_or(0) as i32;
        let time = ctx
            .instruction
            .get("time")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        Ok(TagResult::Emit(Event::BgmFade { gain, time }))
    }
}

/// [span] BGM 声像
pub struct SpanHandler;

impl TagHandler for SpanHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let pan = ctx.resolve_param("pan")?.as_int().unwrap_or(0) as i32;
        // time：毫秒渐变时间，缺省不渐变（立即切换）
        let time = ctx
            .instruction
            .get("time")
            .and_then(|v| v.parse::<u64>().ok());
        Ok(TagResult::Emit(Event::BgmPan { pan, time }))
    }
}

/// [sxfade] 交叉淡入 BGM
pub struct SxfadeHandler;

impl TagHandler for SxfadeHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let file = ctx.resolve_param("file")?.as_string();
        let loop_play = ctx
            .instruction
            .get("loop")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(1)
            != 0;
        let gain = ctx
            .instruction
            .get("gain")
            .and_then(|v| v.parse::<i32>().ok());
        let pan = ctx
            .instruction
            .get("pan")
            .and_then(|v| v.parse::<i32>().ok());
        let time = ctx
            .instruction
            .get("time")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);

        Ok(TagResult::Emit(Event::BgmCrossFade {
            file,
            loop_play,
            gain,
            pan,
            time,
        }))
    }
}

/// [seplay] 播放 SE
pub struct SeplayHandler;

impl TagHandler for SeplayHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let id = ctx.resolve_param_str("id")?;
        let file = ctx.resolve_param("file")?.as_string();
        let loop_play = ctx
            .instruction
            .get("loop")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0)
            != 0;
        let gain = ctx
            .instruction
            .get("gain")
            .and_then(|v| v.parse::<i32>().ok());
        let pan = ctx
            .instruction
            .get("pan")
            .and_then(|v| v.parse::<i32>().ok());
        let time = ctx
            .instruction
            .get("time")
            .and_then(|v| v.parse::<u64>().ok());
        let skippable = ctx
            .instruction
            .get("skippable")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0)
            != 0;

        Ok(TagResult::Emit(Event::SePlay {
            id,
            file,
            loop_play,
            gain,
            pan,
            fade_time: time,
            skippable,
        }))
    }
}

/// [sestop] 停止 SE
pub struct SestopHandler;

impl TagHandler for SestopHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let id = ctx.resolve_param_str("id")?;
        let time = ctx
            .instruction
            .get("time")
            .and_then(|v| v.parse::<u64>().ok());
        Ok(TagResult::Emit(Event::SeStop {
            id,
            fade_time: time,
        }))
    }
}

/// [sefade] SE 音量渐变
pub struct SefadeHandler;

impl TagHandler for SefadeHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let id = ctx.resolve_param_str("id")?;
        let gain = ctx.resolve_param("gain")?.as_int().unwrap_or(0) as i32;
        let time = ctx
            .instruction
            .get("time")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        Ok(TagResult::Emit(Event::SeFade { id, gain, time }))
    }
}

/// [sepan] SE 声像
pub struct SepanHandler;

impl TagHandler for SepanHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let id = ctx.resolve_param_str("id")?;
        let pan = ctx.resolve_param("pan")?.as_int().unwrap_or(0) as i32;
        // time：毫秒渐变时间，缺省立即切换（同 span）
        let time = ctx
            .instruction
            .get("time")
            .and_then(|v| v.parse::<u64>().ok());
        Ok(TagResult::Emit(Event::SePan { id, pan, time }))
    }
}

/// [voice] 语音播放
///
/// 参数与 seplay 完全一致：id/file/loop/gain/pan/time/buffer/skippable。
pub struct VoiceHandler;

impl TagHandler for VoiceHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        // id 是音轨 ID（层级路径形态），与 seplay 相同用 resolve_param_str 防丢尾零
        let id = match ctx.instruction.get("id") {
            Some(_) => {
                let id = ctx.resolve_param_str("id")?;
                (!id.is_empty()).then_some(id)
            }
            None => None,
        };
        let file = ctx.resolve_param("file")?.as_string();
        let loop_play = ctx
            .instruction
            .get("loop")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0)
            != 0;
        let gain = ctx
            .instruction
            .get("gain")
            .and_then(|v| v.parse::<i32>().ok());
        let pan = ctx
            .instruction
            .get("pan")
            .and_then(|v| v.parse::<i32>().ok());
        let time = ctx
            .instruction
            .get("time")
            .and_then(|v| v.parse::<u64>().ok());
        let skippable = ctx
            .instruction
            .get("skippable")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0)
            != 0;
        Ok(TagResult::Emit(Event::VoicePlay {
            id,
            file,
            loop_play,
            gain,
            pan,
            fade_time: time,
            skippable,
        }))
    }
}

/// [/voice] 语音标签闭合
///
/// 文档语义：voice 与 /voice 成对包住台词时，回想日志中该文本显示为可重放
/// 语音的链接。backlog 子系统尚未实现，这里注册为显式空转，
/// 仅消除 Event::Custom 回退噪音。TODO: backlog 链接重放语音。
pub struct VoiceEndHandler;

impl TagHandler for VoiceEndHandler {
    fn execute(&self, _ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        Ok(TagResult::Continue)
    }
}

/// [setonsoundfinish] 音效完成事件处理
pub struct SetOnSoundFinishHandler;

impl TagHandler for SetOnSoundFinishHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let id = ctx.resolve_param_str("id")?;
        let file = ctx.instruction.get("file").map(String::from);
        let label = ctx.instruction.get("label").map(String::from);
        let call = ctx
            .instruction
            .get("call")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0)
            != 0;
        let handler = ctx.instruction.get("handler").map(String::from);

        Ok(TagResult::Emit(Event::SoundFinishHandler {
            id,
            file,
            label,
            call,
            handler,
        }))
    }
}

/// [delonsoundfinish] 解除音效完成事件处理
pub struct DelOnSoundFinishHandler;

impl TagHandler for DelOnSoundFinishHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let id = ctx.resolve_param_str("id")?;
        Ok(TagResult::Emit(Event::SoundFinishHandlerDel { id }))
    }
}

/// [sefadein] SE 淡入（已弃用）
pub struct SefadeinHandler;

impl TagHandler for SefadeinHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let id = ctx.resolve_param_str("id")?;
        let time = ctx
            .instruction
            .get("time")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        Ok(TagResult::Emit(Event::SeFade {
            id,
            gain: 1000,
            time,
        }))
    }
}

/// [sefadeout] SE 淡出（已弃用）
pub struct SefadeoutHandler;

impl TagHandler for SefadeoutHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let id = ctx.resolve_param_str("id")?;
        let time = ctx
            .instruction
            .get("time")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        Ok(TagResult::Emit(Event::SeFade { id, gain: 0, time }))
    }
}

/// [sfadein] BGM 淡入（已弃用）
pub struct SfadeinHandler;

impl TagHandler for SfadeinHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let time = ctx
            .instruction
            .get("time")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        Ok(TagResult::Emit(Event::BgmFade { gain: 1000, time }))
    }
}

/// [sfadeout] BGM 淡出（已弃用）
pub struct SfadeoutHandler;

impl TagHandler for SfadeoutHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let time = ctx
            .instruction
            .get("time")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        Ok(TagResult::Emit(Event::BgmFade { gain: 0, time }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::Instruction;
    use crate::variable::VariableStore;

    /// 用给定参数执行单个标签处理器，返回 TagResult
    fn exec(handler: &dyn TagHandler, tag: &str, params: &[(&str, &str)]) -> TagResult {
        let lua = mlua::Lua::new();
        let instruction = Instruction {
            tag: tag.into(),
            params: params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            line: 1,
        };
        let mut variables = VariableStore::new();
        let get_script = |_name: &str| None;
        let mut ctx = ExecutionContext {
            variables: &mut variables,
            lua: &lua,
            current_script: "test",
            current_line: 0,
            instruction: &instruction,
            get_script: &get_script,
        };
        handler.execute(&mut ctx).unwrap()
    }

    #[test]
    fn splay_parses_buffer_param() {
        let TagResult::Emit(Event::BgmPlay { buffer, .. }) = exec(
            &SplayHandler,
            "splay",
            &[("file", "bgm01"), ("buffer", "-1")],
        ) else {
            panic!("splay 应产出 BgmPlay");
        };
        assert_eq!(buffer, Some(-1), "-1 表示内存播放");

        let TagResult::Emit(Event::BgmPlay { buffer, .. }) =
            exec(&SplayHandler, "splay", &[("file", "bgm01")])
        else {
            panic!("splay 应产出 BgmPlay");
        };
        assert_eq!(buffer, None, "缺省使用默认缓冲");
    }

    #[test]
    fn span_and_sepan_parse_fade_time() {
        let TagResult::Emit(Event::BgmPan { pan, time }) = exec(
            &SpanHandler,
            "span",
            &[("pan", "-1000"), ("time", "500")],
        ) else {
            panic!("span 应产出 BgmPan");
        };
        assert_eq!(pan, -1000);
        assert_eq!(time, Some(500));

        let TagResult::Emit(Event::BgmPan { time, .. }) =
            exec(&SpanHandler, "span", &[("pan", "0")])
        else {
            panic!("span 应产出 BgmPan");
        };
        assert_eq!(time, None, "缺省立即切换");

        let TagResult::Emit(Event::SePan { id, pan, time }) = exec(
            &SepanHandler,
            "sepan",
            &[("id", "1.80"), ("pan", "1000"), ("time", "250")],
        ) else {
            panic!("sepan 应产出 SePan");
        };
        assert_eq!(id, "1.80", "ID 保留字符串形态不丢尾零");
        assert_eq!(pan, 1000);
        assert_eq!(time, Some(250));
    }

    #[test]
    fn voice_parses_seplay_compatible_params() {
        let TagResult::Emit(Event::VoicePlay {
            id,
            file,
            loop_play,
            gain,
            pan,
            fade_time,
            skippable,
        }) = exec(
            &VoiceHandler,
            "voice",
            &[
                ("id", "vo1"),
                ("file", "v001"),
                ("loop", "1"),
                ("gain", "800"),
                ("pan", "-100"),
                ("time", "120"),
                ("skippable", "1"),
            ],
        )
        else {
            panic!("voice 应产出 VoicePlay");
        };
        assert_eq!(id.as_deref(), Some("vo1"));
        assert_eq!(file, "v001");
        assert!(loop_play);
        assert_eq!(gain, Some(800));
        assert_eq!(pan, Some(-100));
        assert_eq!(fade_time, Some(120));
        assert!(skippable);

        // 缺省时 id=None（由核心自动编号），loop/skippable 关闭
        let TagResult::Emit(Event::VoicePlay {
            id,
            loop_play,
            skippable,
            ..
        }) = exec(&VoiceHandler, "voice", &[("file", "v002")])
        else {
            panic!("voice 应产出 VoicePlay");
        };
        assert_eq!(id, None);
        assert!(!loop_play);
        assert!(!skippable);
    }

    #[test]
    fn voice_close_tag_is_noop() {
        assert!(matches!(
            exec(&VoiceEndHandler, "/voice", &[]),
            TagResult::Continue
        ));
    }
}
