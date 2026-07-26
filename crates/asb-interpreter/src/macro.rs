//! 宏系统
//!
//! 宏是将多个标签组合成新标签的功能。
//! 宏定义在 macro.iet 文件中。

use crate::error::{Error, Result};
use crate::script::{Instruction, Script};
use std::collections::HashMap;

/// 宏定义
#[derive(Debug, Clone)]
pub struct Macro {
    /// 宏名称
    pub name: String,
    /// 宏指令序列
    pub instructions: Vec<Instruction>,
    /// 参数列表（从宏定义中提取）
    pub params: Vec<String>,
}

impl Macro {
    /// 从脚本中的标签块创建宏
    pub fn from_script_block(name: &str, script: &Script, start_line: usize) -> Result<Self> {
        let mut instructions = Vec::new();
        let mut line = start_line; // 从标签定义行开始（但跳过标签本身）

        // 实际上 start_line 是标签定义后的第一条指令的索引
        // 收集直到 [return] 的指令
        while line < script.len() {
            if let Some(inst) = script.get_instruction(line) {
                if inst.tag == "return" {
                    break;
                }
                instructions.push(inst.clone());
            }
            line += 1;
        }

        // 提取参数（从指令中的变量引用推断）
        let params = Self::extract_params(&instructions);

        Ok(Macro {
            name: name.to_string(),
            instructions,
            params,
        })
    }

    /// 从指令序列中提取参数名
    fn extract_params(instructions: &[Instruction]) -> Vec<String> {
        let mut params = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for inst in instructions {
            // 检查 if 条件的 estimate 参数
            if inst.tag == "if" {
                if let Some(estimate) = inst.get("estimate") {
                    // 提取变量引用，如 $pos
                    for part in estimate.split(|c: char| !c.is_alphanumeric() && c != '_') {
                        if !part.is_empty() && !part.chars().all(|c| c.is_ascii_digit()) {
                            if seen.insert(part.to_string()) {
                                params.push(part.to_string());
                            }
                        }
                    }
                }
            }

            // 检查所有参数中的变量引用
            for value in inst.params.values() {
                if value.starts_with('$') {
                    let var_name = &value[1..];
                    // 提取第一个标识符
                    let ident: String = var_name
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
                        .collect();
                    if !ident.is_empty()
                        && !ident.contains('.')
                        && !ident.chars().all(|c| c.is_ascii_digit())
                        && seen.insert(ident.clone())
                    {
                        params.push(ident);
                    }
                }
            }
        }

        params
    }
}

/// 宏注册表
#[derive(Debug, Default)]
pub struct MacroRegistry {
    /// 宏定义映射
    pub macros: HashMap<String, Macro>,
}

impl MacroRegistry {
    /// 创建新的宏注册表
    pub fn new() -> Self {
        Self::default()
    }

    /// 从脚本加载宏定义
    pub fn load_from_script(&mut self, script: &Script) -> Result<usize> {
        let mut count = 0;

        for (label_name, &line) in &script.labels {
            let macro_def = Macro::from_script_block(label_name, script, line)?;
            self.macros.insert(label_name.clone(), macro_def);
            count += 1;
        }

        Ok(count)
    }

    /// 注册单个宏
    pub fn register(&mut self, macro_def: Macro) {
        self.macros.insert(macro_def.name.clone(), macro_def);
    }

    /// 获取宏定义
    pub fn get(&self, name: &str) -> Option<&Macro> {
        self.macros.get(name)
    }

    /// 检查宏是否存在
    pub fn contains(&self, name: &str) -> bool {
        self.macros.contains_key(name)
    }

    /// 展开宏调用
    pub fn expand(&self, name: &str, args: &HashMap<String, String>) -> Result<Vec<Instruction>> {
        let macro_def = self
            .get(name)
            .ok_or_else(|| Error::LabelNotFound(format!("宏未定义: {}", name)))?;

        let mut expanded = Vec::new();

        for inst in &macro_def.instructions {
            let mut new_inst = inst.clone();

            // 替换参数中的变量引用
            for (key, value) in &mut new_inst.params {
                // estimate/cond 是表达式参数：其中的 $param 必须保留给表达式
                // 求值器按变量解析（宏实参会同步落入变量存储）。若在此做纯文本
                // 替换，`$pos == 'left'` 会变成 `center == 'left'`，裸标识符无法
                // 正确求值（docs/spec/macro.md：宏参数自动展开为变量）。
                if key == "estimate" || key == "cond" {
                    continue;
                }
                // 替换所有 $param_name 形式的引用
                let mut result = String::new();
                let mut chars = value.chars().peekable();

                while let Some(c) = chars.next() {
                    if c == '$' {
                        // 提取变量名
                        let mut var_name = String::new();
                        while let Some(&next) = chars.peek() {
                            if next.is_alphanumeric() || next == '_' || next == '.' {
                                var_name.push(next);
                                chars.next();
                            } else {
                                break;
                            }
                        }

                        // 如果在 args 中，替换它
                        if let Some(arg_value) = args.get(&var_name) {
                            result.push_str(arg_value);
                        } else {
                            result.push('$');
                            result.push_str(&var_name);
                        }
                    } else {
                        result.push(c);
                    }
                }

                *value = result;
            }

            expanded.push(new_inst);
        }

        Ok(expanded)
    }
}

/// [macroadd] 标签处理器 - 添加宏文件
pub struct MacroAddHandler;

impl super::TagHandler for MacroAddHandler {
    fn execute(&self, ctx: &mut super::ExecutionContext<'_>) -> Result<super::TagResult> {
        let file = ctx.instruction.get("file").unwrap_or("");

        // 这里应该触发加载宏文件的事件
        // 实际加载由解释器处理
        Ok(super::TagResult::Emit(crate::event::Event::Custom {
            tag: "macroadd".to_string(),
            params: {
                let mut p = HashMap::new();
                p.insert("file".to_string(), file.to_string());
                p
            },
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_macro_parsing() {
        let content = r#"
*chara_a
[if estimate="$pos == 'left'"]
    [lyc id="1" file="chara_a"]
[/if]
[if estimate="$pos == 'center'"]
    [lyc id="3" file="chara_a"]
    [lyprop id="3" left="120"]
[/if]
[return]
"#;

        let script = Script::parse("test", content).unwrap();
        let mut registry = MacroRegistry::new();
        let count = registry.load_from_script(&script).unwrap();

        assert_eq!(count, 1);
        assert!(registry.contains("chara_a"));

        let macro_def = registry.get("chara_a").unwrap();
        // 2个if + 2个lyc + 1个lyprop + 2个/endif = 7
        assert_eq!(macro_def.instructions.len(), 7);
    }

    #[test]
    fn test_macro_expansion() {
        let content = r#"
*test_macro
[var name="result" data="$param0 + ' ' + $param1"]
[return]
"#;

        let script = Script::parse("test", content).unwrap();
        let mut registry = MacroRegistry::new();
        registry.load_from_script(&script).unwrap();

        let mut args = HashMap::new();
        args.insert("param0".to_string(), "'Hello'".to_string());
        args.insert("param1".to_string(), "'World'".to_string());

        let expanded = registry.expand("test_macro", &args).unwrap();
        assert_eq!(expanded.len(), 1);
        assert_eq!(expanded[0].get("data"), Some("'Hello' + ' ' + 'World'"));
    }

    #[test]
    fn test_macro_expansion_keeps_expression_params_intact() {
        // estimate/cond 表达式中的 $param 必须原样保留，交由表达式求值器
        // 按变量解析（宏实参会同步写入变量存储）。
        let content = r#"
*chara
[if estimate="$pos == 'left'"]
    [lyc id="1" file="$pos"]
[/if]
[jump cond="$pos == 'right'" label="r"]
[return]
"#;

        let script = Script::parse("test", content).unwrap();
        let mut registry = MacroRegistry::new();
        registry.load_from_script(&script).unwrap();

        let mut args = HashMap::new();
        args.insert("pos".to_string(), "center".to_string());

        let expanded = registry.expand("chara", &args).unwrap();
        assert_eq!(expanded[0].get("estimate"), Some("$pos == 'left'"));
        assert_eq!(expanded[1].get("file"), Some("center"));
        assert_eq!(expanded[3].get("cond"), Some("$pos == 'right'"));
    }
}
