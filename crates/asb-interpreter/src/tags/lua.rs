//! Lua 调用标签处理器
//!
//! 实现 calllua 标签和 [lua]/[/lua] 块。
//! 每个 Lua 函数都会接收一个 engine 对象作为第一个参数。

use super::{ExecutionContext, TagHandler, TagResult};
use crate::error::Result;

/// [calllua] 调用 Lua 函数
pub struct CallLuaHandler;

impl TagHandler for CallLuaHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        let function_name = ctx.instruction.get("function").unwrap_or("");

        if function_name.is_empty() {
            return Err(crate::error::Error::RuntimeError {
                line: ctx.current_line,
                message: "calllua 缺少 function 参数".to_string(),
            });
        }

        // 收集额外参数（除 function 外的所有参数都传递给 Lua 函数）
        let mut extra_params = std::collections::HashMap::new();
        for (key, value) in &ctx.instruction.params {
            if key != "function" {
                extra_params.insert(key.clone(), value.clone());
            }
        }

        // 调用 Lua 函数，传入 engine 对象和额外参数
        call_lua_function(ctx.lua, function_name, &extra_params)?;

        Ok(TagResult::Continue)
    }
}

/// 调用 Lua 函数，将 engine 对象作为第一个参数、extra_params 作为第二个参数（Lua 表）传入。
pub fn call_lua_function(
    lua: &mlua::Lua,
    function_name: &str,
    extra_params: &std::collections::HashMap<String, String>,
) -> Result<()> {
    // 处理嵌套函数名（如 "sv.save"）
    let parts: Vec<&str> = function_name.split('.').collect();

    // 尝试获取函数
    let func = if parts.len() > 1 {
        // 嵌套路径，如 sv.save
        let mut current: mlua::Value = lua.globals().get(parts[0])?;
        for &part in &parts[1..] {
            match current {
                mlua::Value::Table(t) => {
                    current = t.get(part)?;
                }
                _ => {
                    return Ok(());
                }
            }
        }
        match current {
            mlua::Value::Function(f) => f,
            _ => return Ok(()),
        }
    } else {
        // 全局函数
        let globals = lua.globals();
        match globals.get::<mlua::Function>(function_name) {
            Ok(f) => f,
            Err(_) => return Ok(()),
        }
    };

    // 构造 param 表供 Lua 函数读取
    let param_table = lua.create_table()?;
    for (k, v) in extra_params {
        param_table.set(k.as_str(), v.as_str())?;
    }

    // 获取 engine 对象
    let engine: mlua::Value = lua.globals().get("__engine")?;

    let result: mlua::Result<()> = match engine {
        mlua::Value::UserData(ud) => func.call((ud, param_table)),
        _ => func.call((param_table,)),
    };

    // 探针：记录失败调用（设置 ART3M1S_PROBE=1 启用）
    if result.is_err() && std::env::var("ART3M1S_PROBE").is_ok() {
        let err_msg = match &result {
            Err(e) => format!("{e}"),
            _ => String::new(),
        };
        let params_str: Vec<String> = extra_params
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        eprintln!(
            "[PROBE] calllua {function_name}({}) -> ERR: {err_msg}",
            params_str.join(", ")
        );
    }

    result?;
    Ok(())
}

/// 调用 Lua 函数，传入 engine 对象和预构建的 Lua table 作为 params。
/// 用于 `e:enqueueTag{"calllua", ...}` 等需要保留嵌套表结构的场景。
pub fn call_lua_function_with_table(
    lua: &mlua::Lua,
    function_name: &str,
    param_table: mlua::Table,
) -> crate::error::Result<()> {
    let parts: Vec<&str> = function_name.split('.').collect();
    let func = if parts.len() > 1 {
        let mut current: mlua::Value = lua.globals().get(parts[0])?;
        for &part in &parts[1..] {
            current = match current {
                mlua::Value::Table(t) => t.get(part)?,
                _ => return Ok(()),
            };
        }
        match current {
            mlua::Value::Function(f) => f,
            _ => return Ok(()),
        }
    } else {
        match lua.globals().get::<mlua::Function>(function_name) {
            Ok(f) => f,
            Err(_) => return Ok(()),
        }
    };

    let engine: mlua::Value = lua.globals().get("__engine")?;
    let result: mlua::Result<()> = match engine {
        mlua::Value::UserData(ud) => func.call((ud, param_table)),
        _ => func.call((param_table,)),
    };
    result?;
    Ok(())
}
