//! 条件标签处理器
//!
//! 实现 if/elseif/else/endif 条件标签和 loop/endloop 循环标签。

use super::{ExecutionContext, TagHandler, TagResult};
use crate::error::Result;

/// 求值 estimate 参数（已经是 $ 开头的表达式）
fn evaluate_estimate(ctx: &ExecutionContext<'_>) -> Result<bool> {
    let condition = ctx.instruction.get("estimate").unwrap_or("1");
    // estimate 值已经是表达式（可能以 $ 开头，也可能是字面量）
    let value = ctx.evaluator().resolve_param(condition)?;
    Ok(value.as_bool())
}

/// 条件为假时的跳转目标。
///
/// [`ExecutionContext::find_else_elseif_or_endif`] 命中的行分三种：
/// - `[elseif]`：跳到该行本身，由 ElseifHandler 重新求值；
/// - `[else]`：跳到该行的**下一行**、直接进入 else 体——若落在 [else] 行本身，
///   ElseHandler 的无条件跳 /if 会让 else 体永不执行（历史 bug）；
/// - `[/if]`：跳到该行本身（EndifHandler 是空操作）。
fn false_branch_target(ctx: &ExecutionContext<'_>) -> Result<usize> {
    let line = ctx.find_else_elseif_or_endif()?;
    let script = ctx
        .get_script(ctx.current_script)
        .ok_or_else(|| crate::error::Error::ScriptNotFound(ctx.current_script.to_string()))?;
    let hit_else = script
        .get_instruction(line)
        .is_some_and(|inst| inst.tag == "else");
    Ok(if hit_else { line + 1 } else { line })
}

/// 从 `from_line`（指向 [elseif]/[else] 行）向后扫描匹配的 [/if] 行号。
///
/// 供解释器在**顺序执行（fallthrough）**到达 elseif/else 时使用：此时前面的
/// 分支已经命中并执行完毕，剩余分支必须整段跳过（Artemis if 链语义，见
/// docs/tag/script/if.md）。嵌套 if 按深度计数跳过。
pub fn find_matching_endif(script: &crate::script::Script, from_line: usize) -> Result<usize> {
    let mut depth = 1;
    let mut line = from_line + 1;

    while line < script.len() {
        if let Some(inst) = script.get_instruction(line) {
            match inst.tag.as_str() {
                "if" => depth += 1,
                "/if" => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(line);
                    }
                }
                _ => {}
            }
        }
        line += 1;
    }

    Err(crate::error::Error::RuntimeError {
        line: from_line,
        message: "未找到匹配的 /if".to_string(),
    })
}

/// [if] 条件标签
pub struct IfHandler;

impl TagHandler for IfHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        if evaluate_estimate(ctx)? {
            Ok(TagResult::Continue)
        } else {
            Ok(TagResult::Jump(false_branch_target(ctx)?))
        }
    }
}

/// [elseif] 条件分支标签
///
/// 仅当经由前面 if/elseif 条件为假的 Jump 到达时才会执行到这里求值；
/// 顺序执行（前面分支已命中）到达 elseif 的场景由解释器在派发前拦截、
/// 整段跳到匹配的 [/if]（见 interpreter.rs 的 fallthrough 拦截）。
pub struct ElseifHandler;

impl TagHandler for ElseifHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        if evaluate_estimate(ctx)? {
            Ok(TagResult::Continue)
        } else {
            Ok(TagResult::Jump(false_branch_target(ctx)?))
        }
    }
}

/// [else] 标签
///
/// 正常执行流里不会派发到这里：条件为假的路径直接跳到 [else] 的下一行进入
/// else 体（见 [`false_branch_target`]），顺序 fallthrough 到达 [else] 则由
/// 解释器拦截跳 /if。此 handler 仅作为兜底（如 [jump] 直接落在 else 行）。
pub struct ElseHandler;

impl TagHandler for ElseHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        Ok(TagResult::Jump(ctx.find_endif()?))
    }
}

/// [/if] 结束标签
pub struct EndifHandler;

impl TagHandler for EndifHandler {
    fn execute(&self, _ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        Ok(TagResult::Continue)
    }
}

/// [loop] 循环标签
pub struct LoopHandler;

impl TagHandler for LoopHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        if evaluate_estimate(ctx)? {
            Ok(TagResult::Continue)
        } else {
            // 条件为假，跳过循环体到 /loop 之后
            Ok(TagResult::Jump(ctx.find_endloop()? + 1))
        }
    }
}

/// [/loop] 结束标签 - 跳回对应的 loop 开始位置
pub struct EndloopHandler;

impl TagHandler for EndloopHandler {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<TagResult> {
        Ok(TagResult::Jump(ctx.find_loop_start()?))
    }
}

#[cfg(test)]
mod tests {
    use crate::variable::Value;
    use crate::{CallbackResult, Event, Interpreter, InterpreterConfig};

    /// 执行脚本到 [stop]，返回指定变量的值
    fn run_and_get(script: &str, var: &str) -> Option<Value> {
        let mut interpreter = Interpreter::new(InterpreterConfig::default());
        interpreter.set_callback(|event| match event {
            Event::Wait { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        });
        interpreter.load_script("test", script).unwrap();
        interpreter.start("test", "main").unwrap();
        interpreter.run().unwrap();
        interpreter.get_variable(var)
    }

    #[test]
    fn if_false_executes_else_body() {
        // 历史 bug：if 为假时 Jump 落在 [else] 行本身，ElseHandler 无条件跳
        // /if，else 体永不执行（r 保持 'none'）。
        let r = run_and_get(
            r#"
*main
[var name="r" data="'none'"]
[if estimate="0"]
[var name="r" data="'then'"]
[else]
[var name="r" data="'else'"]
[/if]
[stop]
"#,
            "r",
        );
        assert_eq!(r, Some(Value::String("else".to_string())));
    }

    #[test]
    fn if_true_skips_elseif_and_else_branches() {
        // 历史 bug：if 为真执行体后 fallthrough 到 [elseif] 时重新求值，
        // 条件同真则第二个分支体也被执行（n 变成 11）。
        let n = run_and_get(
            r#"
*main
[var name="i" data="3"]
[var name="n" data="0"]
[if estimate="$i == 3"]
[var name="n" data="$n + 1"]
[elseif estimate="$i == 3"]
[var name="n" data="$n + 10"]
[else]
[var name="n" data="$n + 100"]
[/if]
[stop]
"#,
            "n",
        );
        assert_eq!(n.and_then(|v| v.as_int()), Some(1));
    }

    #[test]
    fn elseif_chain_evaluates_in_order_and_takes_first_match() {
        // if 为假跳入 elseif 逐个求值；第一个命中的 elseif 执行后，
        // 其余 elseif/else 全部跳过。
        let r = run_and_get(
            r#"
*main
[var name="i" data="4"]
[if estimate="$i == 3"]
[var name="r" data="'if'"]
[elseif estimate="$i == 4"]
[var name="r" data="'elseif1'"]
[elseif estimate="$i == 4"]
[var name="r" data="'elseif2'"]
[else]
[var name="r" data="'else'"]
[/if]
[stop]
"#,
            "r",
        );
        assert_eq!(r, Some(Value::String("elseif1".to_string())));
    }

    #[test]
    fn all_conditions_false_falls_back_to_else() {
        // if/elseif 全假 → else 兜底分支执行（文档 if.md 示例语义）。
        let r = run_and_get(
            r#"
*main
[var name="i" data="9"]
[var name="r" data="'none'"]
[if estimate="$i == 3"]
[var name="r" data="'if'"]
[elseif estimate="$i == 4"]
[var name="r" data="'elseif'"]
[else]
[var name="r" data="'else'"]
[/if]
[stop]
"#,
            "r",
        );
        assert_eq!(r, Some(Value::String("else".to_string())));
    }

    #[test]
    fn nested_if_inside_taken_branch_does_not_reactivate_outer_chain() {
        // 分支体内的嵌套 if（为假、跳到内层 /if）不得让外层 elseif 被重新求值。
        let r = run_and_get(
            r#"
*main
[var name="r" data="'none'"]
[if estimate="1"]
[if estimate="0"]
[var name="r" data="'inner'"]
[/if]
[var name="r" data="'outer'"]
[elseif estimate="1"]
[var name="r" data="'elseif'"]
[/if]
[stop]
"#,
            "r",
        );
        assert_eq!(r, Some(Value::String("outer".to_string())));
    }

    #[test]
    fn if_false_with_nested_if_skips_to_else_correctly() {
        // if 为假时的深度扫描要越过 then 体内的嵌套 if/if 配对，落到外层 else 体。
        let r = run_and_get(
            r#"
*main
[var name="r" data="'none'"]
[if estimate="0"]
[if estimate="1"]
[var name="r" data="'inner'"]
[/if]
[else]
[var name="r" data="'else'"]
[/if]
[stop]
"#,
            "r",
        );
        assert_eq!(r, Some(Value::String("else".to_string())));
    }

    #[test]
    fn if_without_else_skips_to_endif_when_false() {
        // 无 elseif/else 的裸 if：为假时直接跳 /if，之后的指令照常执行。
        let r = run_and_get(
            r#"
*main
[var name="r" data="'before'"]
[if estimate="0"]
[var name="r" data="'then'"]
[/if]
[var name="r" data="'after'"]
[stop]
"#,
            "r",
        );
        assert_eq!(r, Some(Value::String("after".to_string())));
    }
}
