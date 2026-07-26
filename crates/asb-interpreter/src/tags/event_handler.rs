//! 事件处理器标签
//!
//! 实现 seton*/delon* 系列事件处理器标签。

use super::{ExecutionContext, TagHandler, TagResult};
use crate::error::Result;
use crate::event::Event;

/// 通用的事件处理器设置处理器
macro_rules! event_handler_struct {
    ($name:ident, $event_name:expr) => {
        pub struct $name;

        impl TagHandler for $name {
            fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
                let file = ctx.instruction.get("file").map(String::from);
                let label = ctx.instruction.get("label").map(String::from);
                let call = ctx
                    .instruction
                    .get("call")
                    .and_then(|v| v.parse::<i32>().ok())
                    .unwrap_or(0)
                    != 0;
                let handler = ctx.instruction.get("handler").map(String::from);

                // 已知字段以外的参数（key、adv、ui、btn 等）透传给宿主，
                // 宿主在事件触发时作为 Lua 回调的 param 表传回。
                let known = ["file", "label", "call", "handler"];
                let mut extra_params = std::collections::HashMap::new();
                for (k, v) in &ctx.instruction.params {
                    if !known.contains(&k.as_str()) {
                        extra_params.insert(k.clone(), v.clone());
                    }
                }

                Ok(TagResult::Emit(Event::SetEventHandler {
                    event_name: $event_name.to_string(),
                    file,
                    label,
                    call,
                    handler,
                    extra_params,
                }))
            }
        }
    };
}

/// 通用的事件处理器解除处理器
macro_rules! event_handler_del_struct {
    ($name:ident, $event_name:expr) => {
        pub struct $name;

        impl TagHandler for $name {
            fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
                // 带 key 时只解除该键，不带 key 时解除整个事件类型
                let key = ctx.instruction.get("key").map(String::from);
                Ok(TagResult::Emit(Event::DelEventHandler {
                    event_name: $event_name.to_string(),
                    key,
                }))
            }
        }
    };
}

// 事件处理器
event_handler_struct!(SetOnPushHandler, "push");
event_handler_struct!(SetOnAutomodeInHandler, "automodein");
event_handler_struct!(SetOnAutomodeOutHandler, "automodeout");
event_handler_struct!(SetOnBacklogInHandler, "backlogin");
event_handler_struct!(SetOnBacklogOutHandler, "backlogout");
event_handler_struct!(SetOnCommandSkipInHandler, "commandskipin");
event_handler_struct!(SetOnCommandSkipOutHandler, "commandskipout");
event_handler_struct!(SetOnControlSkipInHandler, "controlskipin");
event_handler_struct!(SetOnControlSkipOutHandler, "controlskipout");
event_handler_struct!(SetOnDirchgHandler, "dirchg");
event_handler_struct!(SetOnHideInHandler, "hidein");
event_handler_struct!(SetOnHideOutHandler, "hideout");

// 事件处理器解除
event_handler_del_struct!(DelOnPushHandler, "push");
event_handler_del_struct!(DelOnAutomodeInHandler, "automodein");
event_handler_del_struct!(DelOnAutomodeOutHandler, "automodeout");
event_handler_del_struct!(DelOnBacklogInHandler, "backlogin");
event_handler_del_struct!(DelOnBacklogOutHandler, "backlogout");
event_handler_del_struct!(DelOnCommandSkipInHandler, "commandskipin");
event_handler_del_struct!(DelOnCommandSkipOutHandler, "commandskipout");
event_handler_del_struct!(DelOnControlSkipInHandler, "controlskipin");
event_handler_del_struct!(DelOnControlSkipOutHandler, "controlskipout");
event_handler_del_struct!(DelOnDirchgHandler, "dirchg");
event_handler_del_struct!(DelOnHideInHandler, "hidein");
event_handler_del_struct!(DelOnHideOutHandler, "hideout");

/// [setonwindowbutton] 窗口按钮按下事件处理器（仅 Windows）
///
/// button：0=关闭按钮(×) / 1=最大化按钮 / 2=最小化按钮 / 缺省=遗留用法（不推荐）。
/// 引擎按 (event_name, key) 索引处理器，这里把 button 值作为 key 透传，
/// 使 delonwindowbutton 能只删除指定按钮的处理器。
pub struct SetOnWindowButtonHandler;

impl TagHandler for SetOnWindowButtonHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let file = ctx.instruction.get("file").map(String::from);
        let label = ctx.instruction.get("label").map(String::from);
        let call = ctx
            .instruction
            .get("call")
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(0)
            != 0;
        let handler = ctx.instruction.get("handler").map(String::from);

        let known = ["file", "label", "call", "handler"];
        let mut extra_params = std::collections::HashMap::new();
        for (k, v) in &ctx.instruction.params {
            if !known.contains(&k.as_str()) {
                extra_params.insert(k.clone(), v.clone());
            }
        }
        // button 值作为索引 key（脚本显式给了 key 时以脚本为准）
        if !extra_params.contains_key("key")
            && let Some(button) = ctx.instruction.get("button")
        {
            extra_params.insert("key".to_string(), button.to_string());
        }

        Ok(TagResult::Emit(Event::SetEventHandler {
            event_name: "windowbutton".to_string(),
            file,
            label,
            call,
            handler,
            extra_params,
        }))
    }
}

/// [delonwindowbutton] 取消窗口按钮事件处理器
///
/// button 参数指定要删除的按钮处理器（与 setonwindowbutton 的 button 对应）；
/// 缺省为遗留用法——删除全部 windowbutton 处理器。
pub struct DelOnWindowButtonHandler;

impl TagHandler for DelOnWindowButtonHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let key = ctx
            .instruction
            .get("button")
            .or_else(|| ctx.instruction.get("key"))
            .map(String::from);
        Ok(TagResult::Emit(Event::DelEventHandler {
            event_name: "windowbutton".to_string(),
            key,
        }))
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
    fn setonwindowbutton_indexes_by_button_value() {
        let TagResult::Emit(Event::SetEventHandler {
            event_name,
            label,
            extra_params,
            ..
        }) = exec(
            &SetOnWindowButtonHandler,
            "setonwindowbutton",
            &[("button", "0"), ("label", "on_close"), ("call", "1")],
        )
        else {
            panic!("setonwindowbutton 应产出 SetEventHandler");
        };
        assert_eq!(event_name, "windowbutton");
        assert_eq!(label.as_deref(), Some("on_close"));
        assert_eq!(
            extra_params.get("key").map(String::as_str),
            Some("0"),
            "button 值应作为 (event_name, key) 索引的 key"
        );
        // button 原值也保留在 extra_params 中，供宿主回调引用
        assert_eq!(extra_params.get("button").map(String::as_str), Some("0"));
    }

    #[test]
    fn delonwindowbutton_deletes_only_specified_button() {
        let TagResult::Emit(Event::DelEventHandler { event_name, key }) = exec(
            &DelOnWindowButtonHandler,
            "delonwindowbutton",
            &[("button", "1")],
        ) else {
            panic!("delonwindowbutton 应产出 DelEventHandler");
        };
        assert_eq!(event_name, "windowbutton");
        assert_eq!(key.as_deref(), Some("1"), "只删除指定按钮的处理器");

        // 缺省（遗留用法）：删除全部 windowbutton 处理器
        let TagResult::Emit(Event::DelEventHandler { key, .. }) =
            exec(&DelOnWindowButtonHandler, "delonwindowbutton", &[])
        else {
            panic!("delonwindowbutton 应产出 DelEventHandler");
        };
        assert_eq!(key, None);
    }
}
