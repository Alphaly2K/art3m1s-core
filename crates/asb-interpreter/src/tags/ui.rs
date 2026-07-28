//! UI 相关标签处理器
//!
//! 实现 uitrans, lyc, lyc2, lydel, lyprop, loading, saving 等标签。

use super::{ExecutionContext, TagHandler, TagResult};
use crate::error::Result;
use crate::event::{Event, LayerEvent, LoadMaskAction, SystemUiAction, TransitionEvent};
use std::collections::HashMap;

/// [uitrans] UI 转场标签
pub struct UiTransHandler;

impl TagHandler for UiTransHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let time = ctx
            .instruction
            .get("time")
            .or_else(|| ctx.instruction.get("0"))
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(500);

        let fade = ctx.instruction.get("fade").map(String::from);

        Ok(TagResult::Wait(Event::UiTransition(TransitionEvent {
            time,
            fade,
        })))
    }
}

/// [loading] 加载状态标签
pub struct LoadingHandler;

impl TagHandler for LoadingHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let active = ctx.instruction.get("0") == Some("on");

        Ok(TagResult::Emit(Event::LoadingState { active }))
    }
}

/// [saving] 存档状态标签
pub struct SavingHandler;

impl TagHandler for SavingHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let active = ctx.instruction.get("0") == Some("on");

        Ok(TagResult::Emit(Event::SavingState { active }))
    }
}

/// [sysshow] 系统 UI 显示标签
pub struct SysShowHandler;

impl TagHandler for SysShowHandler {
    fn execute(&self, _ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        Ok(TagResult::Emit(Event::SystemUi {
            action: SystemUiAction::Show,
        }))
    }
}

/// [syshide] 系统 UI 隐藏标签
pub struct SysHideHandler;

impl TagHandler for SysHideHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let skip = ctx.instruction.get("skip").map(String::from);

        Ok(TagResult::Emit(Event::SystemUi {
            action: SystemUiAction::Hide { skip },
        }))
    }
}

/// [lyc] 创建图层标签
pub struct LycHandler;

impl TagHandler for LycHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let id = ctx.resolve_param_str("id")?;
        let file = ctx.resolve_param("file")?.as_string();

        // 先发 Create（携带 id/file）。文档中 mask/width/height/color 为附加参数：
        // - file 存在时 width/height/color 被忽略，仅 mask 生效；
        // - file 缺省时进入单色图层模式，需要 width/height/color。
        // 这里不做取舍，把出现的参数原样收集为一次 SetProperties 追加事件，
        // 让核心侧按语义消费（color/width/height 已支持，mask 走 custom）。
        let mut properties = HashMap::new();
        if let Some(mask) = ctx.instruction.get("mask") {
            // mask 是路径，保留字符串形态
            properties.insert("mask".to_string(), mask.to_string());
        }
        if let Some(width) = ctx.instruction.get("width") {
            let v = ctx.evaluator().resolve_param(width)?.as_string();
            properties.insert("width".to_string(), v);
        }
        if let Some(height) = ctx.instruction.get("height") {
            let v = ctx.evaluator().resolve_param(height)?.as_string();
            properties.insert("height".to_string(), v);
        }
        if let Some(color) = ctx.instruction.get("color") {
            // color 是 RRGGBB / AARRGGBB 十六进制串，保留字符串形态
            properties.insert("color".to_string(), color.to_string());
        }

        let create = Event::Layer(LayerEvent::Create {
            id: id.clone(),
            file,
        });
        if properties.is_empty() {
            // 无附加参数时行为与旧实现完全一致，只发 Create
            Ok(TagResult::Emit(create))
        } else {
            Ok(TagResult::EmitMany(vec![
                create,
                Event::Layer(LayerEvent::SetProperties { id, properties }),
            ]))
        }
    }
}

/// [lyc2] 创建图层标签（变体）
pub struct Lyc2Handler;

impl TagHandler for Lyc2Handler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let id = ctx.resolve_param_str("id")?;
        let file = ctx.resolve_param("file")?.as_string();
        let alpha = ctx
            .instruction
            .get("alpha")
            .and_then(|v| v.parse::<u8>().ok());

        Ok(TagResult::Emit(Event::Layer(LayerEvent::Create2 {
            id,
            file,
            alpha,
        })))
    }
}

/// [lydel] 删除图层标签
pub struct LydelHandler;

impl TagHandler for LydelHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let id = ctx.resolve_param_str("id")?;

        Ok(TagResult::Emit(Event::Layer(LayerEvent::Delete { id })))
    }
}

/// [lyprop] 图层属性标签
pub struct LypropHandler;

impl TagHandler for LypropHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let id = ctx.resolve_param_str("id")?;

        // 收集所有属性
        let mut properties = HashMap::new();
        for (key, value) in &ctx.instruction.params {
            if key != "id" {
                // 解析表达式
                let resolved = ctx.evaluator().resolve_param(value)?;
                properties.insert(key.clone(), resolved.as_string());
            }
        }

        // 如果是图层集（包含点），展开到所有子图层
        // 这里我们发出事件，由上层处理图层集逻辑
        Ok(TagResult::Emit(Event::Layer(LayerEvent::SetProperties {
            id,
            properties,
        })))
    }
}

/// [lyshader] 注册图层 shader 标签
pub struct LyshaderHandler;

impl TagHandler for LyshaderHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let id = ctx.resolve_param_str("id")?;
        let file = ctx.resolve_param("file")?.as_string();
        Ok(TagResult::Emit(Event::ShaderLoad { id, file }))
    }
}

/// [loadmask] 加载遮罩标签
pub struct LoadMaskHandler;

impl TagHandler for LoadMaskHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let action = match ctx.instruction.get("0") {
            Some("del") => LoadMaskAction::Delete,
            _ => LoadMaskAction::Show,
        };

        let time = ctx
            .instruction
            .get("time")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);

        Ok(TagResult::Emit(Event::LoadMask { action, time }))
    }
}

/// [alldelete] 全部删除标签
pub struct AllDeleteHandler;

impl TagHandler for AllDeleteHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let time = ctx
            .instruction
            .get("time")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);

        Ok(TagResult::Emit(Event::AllDelete { time }))
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
    fn lyc_without_extra_params_emits_single_create() {
        // 无 mask/width/height/color 时行为与旧实现一致：只发一个 Create
        let TagResult::Emit(Event::Layer(LayerEvent::Create { id, file })) =
            exec(&LycHandler, "lyc", &[("id", "0"), ("file", "bg")])
        else {
            panic!("lyc 无附加参数时应只发 Create");
        };
        assert_eq!(id, "0");
        assert_eq!(file, "bg");
    }

    #[test]
    fn lyc_with_mask_and_size_appends_set_properties() {
        // lyc.md：mask/width/height/color 为附加参数，Create 后追加 SetProperties
        let TagResult::EmitMany(events) = exec(
            &LycHandler,
            "lyc",
            &[
                ("id", "1.80"),
                ("file", "cg"),
                ("mask", "cg_mask"),
                ("width", "640"),
                ("height", "480"),
                ("color", "AARRGGBB"),
            ],
        ) else {
            panic!("lyc 带附加参数应发 EmitMany");
        };
        assert_eq!(events.len(), 2, "先 Create 再 SetProperties");

        // 第一个是 Create，且 id 尾零保留
        let Event::Layer(LayerEvent::Create { id, file }) = &events[0] else {
            panic!("第一个事件应为 Create");
        };
        assert_eq!(id, "1.80", "id 尾零保留");
        assert_eq!(file, "cg");

        // 第二个是 SetProperties，携带 mask/width/height/color
        let Event::Layer(LayerEvent::SetProperties { id, properties }) = &events[1] else {
            panic!("第二个事件应为 SetProperties");
        };
        assert_eq!(id, "1.80");
        assert_eq!(properties.get("mask").map(String::as_str), Some("cg_mask"));
        assert_eq!(properties.get("width").map(String::as_str), Some("640"));
        assert_eq!(properties.get("height").map(String::as_str), Some("480"));
        assert_eq!(
            properties.get("color").map(String::as_str),
            Some("AARRGGBB")
        );
    }

    #[test]
    fn lyc_solid_color_mode_carries_size_and_color() {
        // file 缺省=单色图层模式，需 width/height/color；仍先发 Create（file 空）
        let TagResult::EmitMany(events) = exec(
            &LycHandler,
            "lyc",
            &[
                ("id", "5"),
                ("width", "100"),
                ("height", "50"),
                ("color", "FF00FF"),
            ],
        ) else {
            panic!("lyc 单色模式应发 EmitMany");
        };
        let Event::Layer(LayerEvent::SetProperties { properties, .. }) = &events[1] else {
            panic!("第二个事件应为 SetProperties");
        };
        assert_eq!(properties.get("width").map(String::as_str), Some("100"));
        assert_eq!(properties.get("height").map(String::as_str), Some("50"));
        assert_eq!(properties.get("color").map(String::as_str), Some("FF00FF"));
        assert!(properties.get("mask").is_none(), "未指定 mask 不应出现该键");
    }

    #[test]
    fn lyshader_emits_resolved_registration_event() {
        let lua = mlua::Lua::new();
        let instruction = Instruction {
            tag: "lyshader".into(),
            params: HashMap::from([
                ("id".into(), "sepia".into()),
                ("file".into(), "system/shader/pc/sepia.hlsl".into()),
            ]),
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

        let TagResult::Emit(Event::ShaderLoad { id, file }) =
            LyshaderHandler.execute(&mut ctx).unwrap()
        else {
            panic!("lyshader should emit ShaderLoad");
        };
        assert_eq!(id, "sepia");
        assert_eq!(file, "system/shader/pc/sepia.hlsl");
    }
}
