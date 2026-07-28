//! 剧情脚本标签处理器
//!
//! 实现 print, rt, rp, font, ruby, link, glyph, scetween 等剧情文本相关标签。

use super::{ExecutionContext, TagHandler, TagResult};
use crate::error::Result;
use crate::event::Event;
use std::collections::HashMap;

/// [print] 显示场景文本
pub struct PrintHandler;

impl TagHandler for PrintHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let data = ctx.instruction.get("data").unwrap_or("");
        let value = ctx.evaluator().resolve_param(data)?;
        Ok(TagResult::Emit(Event::ScenarioText {
            content: value.as_string(),
            inline: false,
        }))
    }
}

/// [rt] 场景文本换行
pub struct RtHandler;

impl TagHandler for RtHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        // omitblankline=1 时若最后一行为空行则不换行（防止意外空行），缺省 0 始终换行
        let omitblankline = ctx
            .instruction
            .get("omitblankline")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0)
            != 0;
        Ok(TagResult::Emit(Event::LineBreak { omitblankline }))
    }
}

/// [rp] 场景文本分页
pub struct RpHandler;

impl TagHandler for RpHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let backlog = ctx
            .instruction
            .get("backlog")
            .and_then(|v| v.parse::<i32>().ok());
        Ok(TagResult::Emit(Event::PageBreak { backlog }))
    }
}

/// [font] 字体设置
pub struct FontHandler;

impl TagHandler for FontHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let mut settings = HashMap::new();
        for (key, value) in &ctx.instruction.params {
            let resolved = ctx.evaluator().resolve_param(value)?;
            settings.insert(key.clone(), resolved.as_string());
        }
        Ok(TagResult::Emit(Event::FontSettings(settings)))
    }
}

/// [font_close] 回退字体设置
pub struct FontCloseHandler;

impl TagHandler for FontCloseHandler {
    fn execute(&self, _ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        Ok(TagResult::Emit(Event::FontClose))
    }
}

/// [fontdefault] 默认字体设置
pub struct FontDefaultHandler;

impl TagHandler for FontDefaultHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let mut settings = HashMap::new();
        for (key, value) in &ctx.instruction.params {
            let resolved = ctx.evaluator().resolve_param(value)?;
            settings.insert(key.clone(), resolved.as_string());
        }
        Ok(TagResult::Emit(Event::FontDefault(settings)))
    }
}

/// [fontinit] 初始化字体为默认
pub struct FontInitHandler;

impl TagHandler for FontInitHandler {
    fn execute(&self, _ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        Ok(TagResult::Emit(Event::FontInit))
    }
}

/// [ruby] 开始注音
pub struct RubyHandler;

impl TagHandler for RubyHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        // text 为 STRING：用 resolve_param_str 解析（支持 $ 变量/单引号字面量），
        // 但保留字符串形态不做数值强转，避免注音里的数字尾零被 f64 解析丢失
        // （如 "1.80" 被 resolve_param 误判为浮点会变成 "1.8"）。
        let text = ctx.resolve_param_str("text")?;
        Ok(TagResult::Emit(Event::RubyStart { text }))
    }
}

/// [/ruby] 结束注音
pub struct RubyEndHandler;

impl TagHandler for RubyEndHandler {
    fn execute(&self, _ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        Ok(TagResult::Emit(Event::RubyEnd))
    }
}

/// [link] 开始链接
pub struct LinkHandler;

impl TagHandler for LinkHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let file = ctx.instruction.get("file").map(String::from);
        let label = ctx.instruction.get("label").map(String::from);
        let link_type = ctx
            .instruction
            .get("type")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0);
        // color/shadowcolor/outlinecolor 仅在 type=1 时用于强调显示；
        // 缺省（None）时由核心落回文档默认 0x000000。
        let color = ctx.instruction.get("color").map(String::from);
        let shadowcolor = ctx.instruction.get("shadowcolor").map(String::from);
        let outlinecolor = ctx.instruction.get("outlinecolor").map(String::from);
        Ok(TagResult::Emit(Event::LinkStart {
            file,
            label,
            link_type,
            color,
            shadowcolor,
            outlinecolor,
        }))
    }
}

/// [/link] 结束链接
pub struct LinkEndHandler;

impl TagHandler for LinkEndHandler {
    fn execute(&self, _ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        Ok(TagResult::Emit(Event::LinkEnd))
    }
}

/// [linkdisable] 禁用链接
pub struct LinkDisableHandler;

impl TagHandler for LinkDisableHandler {
    fn execute(&self, _ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        Ok(TagResult::Emit(Event::LinkDisable))
    }
}

/// [linkenable] 启用链接
pub struct LinkEnableHandler;

impl TagHandler for LinkEnableHandler {
    fn execute(&self, _ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        Ok(TagResult::Emit(Event::LinkEnable))
    }
}

/// [glyph] 点击等待图标设置
pub struct GlyphHandler;

impl TagHandler for GlyphHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let mut settings = HashMap::new();
        for (key, value) in &ctx.instruction.params {
            settings.insert(key.clone(), value.clone());
        }
        Ok(TagResult::Emit(Event::GlyphConfig(settings)))
    }
}

/// [chgmsg] 切换消息层
pub struct ChgmsgHandler;

/// chgmsg 匿名消息层的序号（保证同进程内生成的随机 ID 不重复）
static CHGMSG_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl TagHandler for ChgmsgHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        // id 缺省时按文档"设置为随机值"——生成一个新的匿名消息层 ID，
        // 而不是落回缺省消息层（一次性切换后通常由 /chgmsg 回退）。
        let id = match ctx.instruction.get("id").filter(|v| !v.is_empty()) {
            Some(id) => Some(id.to_string()),
            None => {
                let serial = CHGMSG_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(0);
                Some(format!("chgmsg_{nanos:x}_{serial}"))
            }
        };
        // stack=0 时不把前一设置压入消息层堆栈（防存档膨胀），缺省 1 压栈
        let stack = ctx
            .instruction
            .get("stack")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(1)
            != 0;
        // layered：0=还原常规消息层 / 1=视为图像层处理 / 缺省=独立于图像层之上
        let layered = ctx
            .instruction
            .get("layered")
            .and_then(|v| v.parse::<i32>().ok());
        Ok(TagResult::Emit(Event::MessageLayerSwitch {
            id,
            stack,
            layered,
        }))
    }
}

/// [chgmsg_close] 回退消息层
pub struct ChgmsgCloseHandler;

impl TagHandler for ChgmsgCloseHandler {
    fn execute(&self, _ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        Ok(TagResult::Emit(Event::MessageLayerPop))
    }
}

/// [scetween] 文本动画
pub struct ScetweenHandler;

impl TagHandler for ScetweenHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let mut params = HashMap::new();
        for (key, value) in &ctx.instruction.params {
            let resolved = ctx.evaluator().resolve_param(value)?;
            params.insert(key.clone(), resolved.as_string());
        }
        Ok(TagResult::Emit(Event::TextAnimation(params)))
    }
}

/// [scein] 场景进入
pub struct SceinHandler;

impl TagHandler for SceinHandler {
    fn execute(&self, _ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        Ok(TagResult::Emit(Event::SceneIn))
    }
}

/// [sceout] 场景退出
pub struct SceoutHandler;

impl TagHandler for SceoutHandler {
    fn execute(&self, _ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        Ok(TagResult::Emit(Event::SceneOut))
    }
}

/// [automode] 自动模式设置
pub struct AutomodeHandler;

impl TagHandler for AutomodeHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let allow = ctx
            .instruction
            .get("allow")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(1);
        let layer = ctx.instruction.get("layer").map(String::from);
        // stopbyclick/stopbystop 缺省=保留之前设置，故用 Option 透传
        let stopbyclick = ctx
            .instruction
            .get("stopbyclick")
            .and_then(|v| v.parse::<i32>().ok())
            .map(|v| v != 0);
        let stopbystop = ctx
            .instruction
            .get("stopbystop")
            .and_then(|v| v.parse::<i32>().ok())
            .map(|v| v != 0);
        // syncse 是 STRING ARRAY：以逗号分隔的 SE ID 列表；缺省=保留之前设置
        let syncse = ctx.instruction.get("syncse").map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect::<Vec<String>>()
        });
        Ok(TagResult::Emit(Event::AutoModeConfig {
            allow: allow != 0,
            layer,
            stopbyclick,
            stopbystop,
            syncse,
        }))
    }
}

/// [skip] 跳过设置
pub struct SkipHandler;

impl TagHandler for SkipHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        // 文档：allow/unread 缺省时均继承之前的设置，故用 Option 透传
        let allow = ctx
            .instruction
            .get("allow")
            .and_then(|v| v.parse::<i32>().ok())
            .map(|v| v != 0);
        let unread = ctx
            .instruction
            .get("unread")
            .and_then(|v| v.parse::<i32>().ok())
            .map(|v| v != 0);
        Ok(TagResult::Emit(Event::SkipConfig {
            allow,
            skip_unread: unread,
        }))
    }
}

/// [backlog] 历史设置
pub struct BacklogHandler;

impl TagHandler for BacklogHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let allow = ctx
            .instruction
            .get("allow")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(1);
        // messagelayer：历史文本消息层 ID；缺省（None）=继承先前设置
        let messagelayer = ctx.instruction.get("messagelayer").map(String::from);
        // includefont：0 不含字体信息 / 1（默认）含字体信息；缺省（None）=继承先前设置
        let includefont = ctx
            .instruction
            .get("includefont")
            .and_then(|v| v.parse::<i32>().ok())
            .map(|v| v != 0);
        // hide：STRING ARRAY（逗号分隔图层 ID）进入历史时临时隐藏；缺省（None）=继承先前设置
        let hide = ctx.instruction.get("hide").map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect::<Vec<String>>()
        });
        // layer：进入历史时自动显示并与自动模式同步的图层 ID；缺省（None）=禁用自动显示
        let layer = ctx.instruction.get("layer").map(String::from);
        // clear：1 时清除已存储的历史剧情，缺省/0 不清除
        let clear = ctx
            .instruction
            .get("clear")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0)
            != 0;
        Ok(TagResult::Emit(Event::BacklogConfig {
            allow: allow != 0,
            messagelayer,
            includefont,
            hide,
            layer,
            clear,
        }))
    }
}

/// [hide] 隐藏模式
pub struct HideHandler;

impl TagHandler for HideHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let allow = ctx
            .instruction
            .get("allow")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(1);
        // window 是 STRING ARRAY（逗号分隔的图层 ID 列表）：
        // 隐藏时同时临时隐藏这些图层；缺省（None）=继承之前的设置。
        let window = ctx.instruction.get("window").map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect::<Vec<String>>()
        });
        Ok(TagResult::Emit(Event::HideConfig {
            allow: allow != 0,
            window,
        }))
    }
}

/// [alreadyread] 已读判定设置
pub struct AlreadyreadHandler;

impl TagHandler for AlreadyreadHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        // 文档默认值为 1（进行已读未读判定）
        let mode = ctx
            .instruction
            .get("mode")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(1);
        Ok(TagResult::Emit(Event::AlreadyReadConfig { mode }))
    }
}

/// [writebacklog] 历史写入设置
pub struct WritebacklogHandler;

impl TagHandler for WritebacklogHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        // 文档参数名为 mode，缺省/0 不存入，1 存入
        let mode = ctx
            .instruction
            .get("mode")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0);
        Ok(TagResult::Emit(Event::WriteBacklogConfig {
            mode: mode != 0,
        }))
    }
}

/// [indent] 缩进设置
pub struct IndentHandler;

impl TagHandler for IndentHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        // pair：每两个字符一组，交替列出缩进开始/结束字符（如 "「」『』"）
        let pair = ctx.instruction.get("pair").unwrap_or("").to_string();
        // range：从行首数 N 个字符之后出现的缩进开始字符不识别；缺省任意位置都识别
        let range = ctx
            .instruction
            .get("range")
            .and_then(|v| v.parse::<usize>().ok());
        // nest：缺省 0 已处于缩进状态时忽略后续开始字符 / 1 重复嵌套缩进
        let nest = ctx
            .instruction
            .get("nest")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0)
            != 0;
        Ok(TagResult::Emit(Event::IndentConfig { pair, range, nest }))
    }
}

/// prohibit 标签缺省的行首禁则字符（不能出现在行首的字符）
pub const DEFAULT_PROHIBIT_HEAD: &str = "!?%)]},.:;、。，．・：；！？」』）｝〕］】";
/// prohibit 标签缺省的行尾禁则字符（不能出现在行尾的字符）
pub const DEFAULT_PROHIBIT_FOOT: &str = "([{「『（｛〔［【";
/// wordparts 标签缺省的单词组成字符集合
pub const DEFAULT_WORDPARTS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// [prohibit] 禁则处理
pub struct ProhibitHandler;

impl TagHandler for ProhibitHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        // head/foot 均为连续字符串（无需分隔符），缺省时落回文档默认禁则表
        let head = ctx
            .instruction
            .get("head")
            .unwrap_or(DEFAULT_PROHIBIT_HEAD)
            .to_string();
        let foot = ctx
            .instruction
            .get("foot")
            .unwrap_or(DEFAULT_PROHIBIT_FOOT)
            .to_string();
        Ok(TagResult::Emit(Event::ProhibitConfig { head, foot }))
    }
}

/// [wordparts] 单词部分字符
pub struct WordpartsHandler;

impl TagHandler for WordpartsHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        // parts 为连续字符串（无需分隔符），缺省时落回文档默认字符集
        let parts = ctx
            .instruction
            .get("parts")
            .unwrap_or(DEFAULT_WORDPARTS)
            .to_string();
        Ok(TagResult::Emit(Event::WordpartsConfig { parts }))
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
    fn hide_parses_window_layer_array() {
        // hide.md：window 是 STRING ARRAY（同时临时隐藏的图层 ID）；缺省继承
        let TagResult::Emit(Event::HideConfig { allow, window }) = exec(
            &HideHandler,
            "hide",
            &[("allow", "1"), ("window", "mw, face,ui.btn")],
        ) else {
            panic!("hide 应产出 HideConfig");
        };
        assert!(allow);
        assert_eq!(
            window,
            Some(vec![
                "mw".to_string(),
                "face".to_string(),
                "ui.btn".to_string()
            ])
        );

        // window 缺省 → None（继承之前设置）
        let TagResult::Emit(Event::HideConfig { allow, window }) =
            exec(&HideHandler, "hide", &[("allow", "0")])
        else {
            panic!("hide 应产出 HideConfig");
        };
        assert!(!allow);
        assert_eq!(window, None);
    }

    #[test]
    fn rt_parses_omitblankline() {
        let TagResult::Emit(Event::LineBreak { omitblankline }) = exec(&RtHandler, "rt", &[])
        else {
            panic!("rt 应产出 LineBreak");
        };
        assert!(!omitblankline, "缺省应始终换行");

        let TagResult::Emit(Event::LineBreak { omitblankline }) =
            exec(&RtHandler, "rt", &[("omitblankline", "1")])
        else {
            panic!("rt 应产出 LineBreak");
        };
        assert!(omitblankline);
    }

    #[test]
    fn chgmsg_parses_stack_and_layered() {
        let TagResult::Emit(Event::MessageLayerSwitch { id, stack, layered }) = exec(
            &ChgmsgHandler,
            "chgmsg",
            &[("id", "mw2"), ("stack", "0"), ("layered", "1")],
        ) else {
            panic!("chgmsg 应产出 MessageLayerSwitch");
        };
        assert_eq!(id.as_deref(), Some("mw2"));
        assert!(!stack, "stack=0 不压栈");
        assert_eq!(layered, Some(1));
    }

    #[test]
    fn chgmsg_generates_random_id_when_omitted() {
        let TagResult::Emit(Event::MessageLayerSwitch {
            id: id1,
            stack,
            layered,
        }) = exec(&ChgmsgHandler, "chgmsg", &[])
        else {
            panic!("chgmsg 应产出 MessageLayerSwitch");
        };
        let TagResult::Emit(Event::MessageLayerSwitch { id: id2, .. }) =
            exec(&ChgmsgHandler, "chgmsg", &[])
        else {
            panic!("chgmsg 应产出 MessageLayerSwitch");
        };
        let id1 = id1.expect("缺省 id 应生成随机值");
        let id2 = id2.expect("缺省 id 应生成随机值");
        assert!(!id1.is_empty());
        assert_ne!(id1, id2, "两次生成的随机 ID 不应重复");
        assert!(stack, "stack 缺省压栈");
        assert_eq!(layered, None, "layered 缺省=独立于图像层");
    }

    #[test]
    fn automode_parses_stop_gates_and_syncse() {
        let TagResult::Emit(Event::AutoModeConfig {
            allow,
            layer,
            stopbyclick,
            stopbystop,
            syncse,
        }) = exec(
            &AutomodeHandler,
            "automode",
            &[
                ("allow", "1"),
                ("layer", "automark"),
                ("stopbyclick", "0"),
                ("stopbystop", "1"),
                ("syncse", "se1, se2"),
            ],
        )
        else {
            panic!("automode 应产出 AutoModeConfig");
        };
        assert!(allow);
        assert_eq!(layer.as_deref(), Some("automark"));
        assert_eq!(stopbyclick, Some(false));
        assert_eq!(stopbystop, Some(true));
        assert_eq!(syncse, Some(vec!["se1".to_string(), "se2".to_string()]));

        // 缺省参数保留之前设置（None）
        let TagResult::Emit(Event::AutoModeConfig {
            stopbyclick,
            stopbystop,
            syncse,
            ..
        }) = exec(&AutomodeHandler, "automode", &[("allow", "0")])
        else {
            panic!("automode 应产出 AutoModeConfig");
        };
        assert_eq!(stopbyclick, None);
        assert_eq!(stopbystop, None);
        assert_eq!(syncse, None);
    }

    #[test]
    fn skip_defaults_inherit_previous_settings() {
        let TagResult::Emit(Event::SkipConfig { allow, skip_unread }) =
            exec(&SkipHandler, "skip", &[])
        else {
            panic!("skip 应产出 SkipConfig");
        };
        assert_eq!(allow, None, "缺省继承之前设置");
        assert_eq!(skip_unread, None, "缺省继承之前设置");

        let TagResult::Emit(Event::SkipConfig { allow, skip_unread }) =
            exec(&SkipHandler, "skip", &[("allow", "0"), ("unread", "1")])
        else {
            panic!("skip 应产出 SkipConfig");
        };
        assert_eq!(allow, Some(false));
        assert_eq!(skip_unread, Some(true));
    }

    #[test]
    fn alreadyread_defaults_to_mode_1() {
        let TagResult::Emit(Event::AlreadyReadConfig { mode }) =
            exec(&AlreadyreadHandler, "alreadyread", &[])
        else {
            panic!("alreadyread 应产出 AlreadyReadConfig");
        };
        assert_eq!(mode, 1, "文档默认值为 1（进行判定）");
    }

    #[test]
    fn writebacklog_reads_mode_param_default_off() {
        let TagResult::Emit(Event::WriteBacklogConfig { mode }) =
            exec(&WritebacklogHandler, "writebacklog", &[])
        else {
            panic!("writebacklog 应产出 WriteBacklogConfig");
        };
        assert!(!mode, "缺省不存入历史");

        let TagResult::Emit(Event::WriteBacklogConfig { mode }) =
            exec(&WritebacklogHandler, "writebacklog", &[("mode", "1")])
        else {
            panic!("writebacklog 应产出 WriteBacklogConfig");
        };
        assert!(mode);
    }

    #[test]
    fn backlog_parses_all_params() {
        // backlog.md：messagelayer/includefont/hide/layer/clear 全套参数
        let TagResult::Emit(Event::BacklogConfig {
            allow,
            messagelayer,
            includefont,
            hide,
            layer,
            clear,
        }) = exec(
            &BacklogHandler,
            "backlog",
            &[
                ("allow", "1"),
                ("messagelayer", "blmsg"),
                ("includefont", "0"),
                ("hide", "mw, face"),
                ("layer", "blmark"),
                ("clear", "1"),
            ],
        )
        else {
            panic!("backlog 应产出 BacklogConfig");
        };
        assert!(allow);
        assert_eq!(messagelayer.as_deref(), Some("blmsg"));
        assert_eq!(includefont, Some(false));
        assert_eq!(hide, Some(vec!["mw".to_string(), "face".to_string()]));
        assert_eq!(layer.as_deref(), Some("blmark"));
        assert!(clear, "clear=1 应清除历史");
    }

    #[test]
    fn backlog_defaults_inherit_previous_settings() {
        // 缺省时 messagelayer/includefont/hide/layer=None（继承/禁用），clear=false
        let TagResult::Emit(Event::BacklogConfig {
            allow,
            messagelayer,
            includefont,
            hide,
            layer,
            clear,
        }) = exec(&BacklogHandler, "backlog", &[("allow", "0")])
        else {
            panic!("backlog 应产出 BacklogConfig");
        };
        assert!(!allow);
        assert_eq!(messagelayer, None, "缺省继承先前设置");
        assert_eq!(includefont, None, "缺省继承先前设置");
        assert_eq!(hide, None, "缺省继承先前设置");
        assert_eq!(layer, None, "缺省禁用自动显示");
        assert!(!clear, "缺省不清除历史");
    }

    #[test]
    fn link_parses_emphasis_colors() {
        // link.md：type=1 时 color/shadowcolor/outlinecolor 用于强调显示
        let TagResult::Emit(Event::LinkStart {
            file,
            label,
            link_type,
            color,
            shadowcolor,
            outlinecolor,
        }) = exec(
            &LinkHandler,
            "link",
            &[
                ("label", "choice_a"),
                ("type", "1"),
                ("color", "FF0000"),
                ("shadowcolor", "00FF00"),
                ("outlinecolor", "0000FF"),
            ],
        )
        else {
            panic!("link 应产出 LinkStart");
        };
        assert_eq!(file, None);
        assert_eq!(label.as_deref(), Some("choice_a"));
        assert_eq!(link_type, 1);
        assert_eq!(color.as_deref(), Some("FF0000"));
        assert_eq!(shadowcolor.as_deref(), Some("00FF00"));
        assert_eq!(outlinecolor.as_deref(), Some("0000FF"));

        // 缺省时三个颜色均为 None（由核心落回 0x000000）
        let TagResult::Emit(Event::LinkStart {
            color,
            shadowcolor,
            outlinecolor,
            ..
        }) = exec(&LinkHandler, "link", &[("label", "x")])
        else {
            panic!("link 应产出 LinkStart");
        };
        assert_eq!(color, None);
        assert_eq!(shadowcolor, None);
        assert_eq!(outlinecolor, None);
    }

    #[test]
    fn ruby_text_resolves_but_keeps_trailing_zero() {
        // ruby.md：text 为 STRING；用 resolve_param_str 解析保留尾零
        let TagResult::Emit(Event::RubyStart { text }) =
            exec(&RubyHandler, "ruby", &[("text", "カピバラ")])
        else {
            panic!("ruby 应产出 RubyStart");
        };
        assert_eq!(text, "カピバラ");

        // 形如 "1.80" 的数字串不能被误当浮点丢尾零
        let TagResult::Emit(Event::RubyStart { text }) =
            exec(&RubyHandler, "ruby", &[("text", "1.80")])
        else {
            panic!("ruby 应产出 RubyStart");
        };
        assert_eq!(text, "1.80", "尾零必须保留");
    }

    #[test]
    fn indent_parses_pair_range_nest() {
        let TagResult::Emit(Event::IndentConfig { pair, range, nest }) = exec(
            &IndentHandler,
            "indent",
            &[("pair", "「」『』"), ("range", "3"), ("nest", "1")],
        ) else {
            panic!("indent 应产出 IndentConfig");
        };
        assert_eq!(pair, "「」『』");
        assert_eq!(range, Some(3));
        assert!(nest);

        let TagResult::Emit(Event::IndentConfig { pair, range, nest }) =
            exec(&IndentHandler, "indent", &[])
        else {
            panic!("indent 应产出 IndentConfig");
        };
        assert_eq!(pair, "");
        assert_eq!(range, None, "缺省任意位置都识别");
        assert!(!nest, "缺省不嵌套");
    }

    #[test]
    fn prohibit_falls_back_to_documented_defaults() {
        let TagResult::Emit(Event::ProhibitConfig { head, foot }) =
            exec(&ProhibitHandler, "prohibit", &[])
        else {
            panic!("prohibit 应产出 ProhibitConfig");
        };
        assert_eq!(head, DEFAULT_PROHIBIT_HEAD);
        assert_eq!(foot, DEFAULT_PROHIBIT_FOOT);

        let TagResult::Emit(Event::ProhibitConfig { head, foot }) = exec(
            &ProhibitHandler,
            "prohibit",
            &[("head", "。、"), ("foot", "「")],
        ) else {
            panic!("prohibit 应产出 ProhibitConfig");
        };
        assert_eq!(head, "。、");
        assert_eq!(foot, "「");
    }

    #[test]
    fn wordparts_falls_back_to_documented_default() {
        let TagResult::Emit(Event::WordpartsConfig { parts }) =
            exec(&WordpartsHandler, "wordparts", &[])
        else {
            panic!("wordparts 应产出 WordpartsConfig");
        };
        assert_eq!(parts, DEFAULT_WORDPARTS);

        let TagResult::Emit(Event::WordpartsConfig { parts }) =
            exec(&WordpartsHandler, "wordparts", &[("parts", "абв")])
        else {
            panic!("wordparts 应产出 WordpartsConfig");
        };
        assert_eq!(parts, "абв");
    }
}
