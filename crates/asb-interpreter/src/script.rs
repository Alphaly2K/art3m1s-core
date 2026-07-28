//! 脚本解析与表示
//!
//! 解析 ASB 脚本文本，生成可执行的指令序列。
//! 解析前先经过预处理器（&autoinsert / &linetag / &scpsupport，见
//! [`preprocess`] 模块），预处理保持行号一一对应。

pub mod preprocess;

use crate::error::{Error, Result};
use std::collections::HashMap;

pub use preprocess::TagIni;

/// 单条指令
#[derive(Debug, Clone)]
pub struct Instruction {
    /// 标签名
    pub tag: String,
    /// 参数键值对
    pub params: HashMap<String, String>,
    /// 原始行号
    pub line: usize,
}

impl Instruction {
    /// 获取参数值
    pub fn get(&self, key: &str) -> Option<&str> {
        self.params.get(key).map(|s| s.as_str())
    }

    /// 获取参数值，如果不存在则返回默认值
    pub fn get_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.params.get(key).map(|s| s.as_str()).unwrap_or(default)
    }

    /// 获取无键参数（第一个参数或 key 为 "0" 的参数）
    pub fn get_default(&self) -> Option<&str> {
        self.params
            .get("0")
            .or(self.params.values().next())
            .map(|s| s.as_str())
    }

    /// 检查是否有某个参数
    pub fn has(&self, key: &str) -> bool {
        self.params.contains_key(key)
    }
}

/// 解析后的脚本
#[derive(Debug, Clone)]
pub struct Script {
    /// 脚本名称
    pub name: String,
    /// 标签名到行号的映射
    pub labels: HashMap<String, usize>,
    /// 指令序列
    pub instructions: Vec<Instruction>,
}

impl Script {
    /// 从文本解析脚本
    ///
    /// 若脚本包含预处理指令（[&autoinsert]/[&linetag]/[&scpsupport]），
    /// 先经预处理管线变换（引用全局注册的 tag.ini，见
    /// [`preprocess::install_tag_ini`]），再解析。预处理是「一行进一行出」，
    /// 指令行号不受影响。
    pub fn parse(name: &str, content: &str) -> Result<Self> {
        // 快路径：预处理指令都以 "[&" 开头，全文没有就跳过整条管线
        if !content.contains("[&") {
            return Self::parse_preprocessed(name, content);
        }
        let pre = preprocess::with_global_tag_ini(|ini| preprocess::preprocess(content, ini));
        Self::parse_preprocessed(name, &pre)
    }

    /// 从文本解析脚本，显式指定 tag.ini（绕开全局注册表；测试/宿主定制用）
    pub fn parse_with_tag_ini(name: &str, content: &str, tag_ini: Option<&TagIni>) -> Result<Self> {
        let pre = preprocess::preprocess(content, tag_ini);
        Self::parse_preprocessed(name, &pre)
    }

    /// 解析已完成预处理的脚本文本
    fn parse_preprocessed(name: &str, content: &str) -> Result<Self> {
        let mut labels = HashMap::new();
        let mut instructions = Vec::new();

        let lines: Vec<&str> = content.lines().collect();
        let mut line_idx = 0;

        'lines: while line_idx < lines.len() {
            let line = lines[line_idx].trim();
            let line_num = line_idx + 1;

            // 跳过空行
            if line.is_empty() {
                line_idx += 1;
                continue;
            }

            // 跳过注释行（以 // 或 ; 开头）
            if line.starts_with("//") || line.starts_with(';') {
                line_idx += 1;
                continue;
            }

            // 标签定义 (*labelname)
            if let Some(label_name) = line.strip_prefix('*') {
                let label_name = label_name.trim().to_string();
                if !label_name.is_empty() {
                    labels.insert(label_name, instructions.len());
                }
                line_idx += 1;
                continue;
            }

            // 指令解析 - 一行中可能混合多个标签与剧情文本段
            // （Artemis 语义：未被 [ 和 ] 包围的部分都是剧情文本，见
            // docs/spec/script_syntax.md；预处理器注入的
            // [onlinehead]text[onlineend] 也依赖此规则）
            if line.contains('[') {
                let segments = split_line_segments(line);

                for segment in segments {
                    let inner = match &segment {
                        LineSegment::Tag(inner) => inner.as_str(),
                        LineSegment::Text(text) => {
                            let text = text.trim();
                            if text.is_empty() {
                                continue;
                            }
                            // 标签后的行内注释：// 起到行尾，剩余段全部丢弃
                            // （保持旧解析器对尾随注释不产出文本的行为）
                            if text.starts_with("//") {
                                break;
                            }
                            let mut params = HashMap::new();
                            params.insert("text".to_string(), text.to_string());
                            instructions.push(Instruction {
                                tag: "__text".to_string(),
                                params,
                                line: line_num,
                            });
                            continue;
                        }
                    };

                    // 检查是否是 [lua] 块开始
                    if inner.trim() == "lua" {
                        // 收集直到 [/lua] 的所有行作为 Lua 代码
                        let mut lua_code = String::new();
                        line_idx += 1;
                        let mut found_end = false;

                        while line_idx < lines.len() {
                            let lua_line = lines[line_idx].trim();
                            if lua_line == "[/lua]" {
                                found_end = true;
                                line_idx += 1;
                                break;
                            }
                            if !lua_code.is_empty() {
                                lua_code.push('\n');
                            }
                            lua_code.push_str(lines[line_idx]);
                            line_idx += 1;
                        }

                        if !found_end {
                            return Err(Error::ParseError {
                                line: line_num,
                                message: "未找到 [/lua] 结束标记".to_string(),
                            });
                        }

                        // 创建特殊的 Lua 块指令
                        let mut params = HashMap::new();
                        params.insert("code".to_string(), lua_code);
                        instructions.push(Instruction {
                            tag: "__lua_block".to_string(),
                            params,
                            line: line_num,
                        });
                        // line_idx 已指向 [/lua] 的下一行，直接进入外层循环下一轮，
                        // 不能再走末尾的统一 +1，否则会吞掉 [/lua] 紧邻的下一行。
                        continue 'lines;
                    }

                    match parse_instruction(inner, line_num) {
                        Ok(instruction) => {
                            instructions.push(instruction);
                        }
                        Err(e) => {
                            return Err(Error::ParseError {
                                line: line_num,
                                message: e.to_string(),
                            });
                        }
                    }
                }
                line_idx += 1;
                continue;
            }

            // 剧情文本（未被 [] 包围的内容）
            // 创建特殊的 text 标签
            let mut params = HashMap::new();
            params.insert("text".to_string(), line.to_string());
            instructions.push(Instruction {
                tag: "__text".to_string(),
                params,
                line: line_num,
            });
            line_idx += 1;
        }

        Ok(Script {
            name: name.to_string(),
            labels,
            instructions,
        })
    }

    /// 获取标签对应的行号
    ///
    /// label 为空串表示「文件开头」：jump/call 标签的 label 参数缺省时应跳到
    /// 文件开头（docs/tag/script/jump.md：label 缺省=默认为文件开头）。统一在
    /// 此处兜底，使同文件跳转、跨文件跳转与调用栈路径共享同一缺省语义。
    pub fn get_label_line(&self, label: &str) -> Option<usize> {
        if label.is_empty() {
            return Some(0);
        }
        self.labels.get(label).copied()
    }

    /// 获取指令
    pub fn get_instruction(&self, line: usize) -> Option<&Instruction> {
        self.instructions.get(line)
    }

    /// 获取指令数量
    pub fn len(&self) -> usize {
        self.instructions.len()
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.instructions.is_empty()
    }
}

/// 一行文本的组成段：标签或剧情文本
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LineSegment {
    /// 标签内容（不含外层 `[` 和 `]`）
    Tag(String),
    /// 标签之外的剧情文本
    Text(String),
}

/// 把一行文本按出现顺序切分成标签段与文本段（支持一行多个标签、
/// 文本与标签混排）。
///
/// 例如: `[onlinehead]「ぷるぷる」@[rp]` ->
/// `[Tag("onlinehead"), Text("「ぷるぷる」@"), Tag("rp")]`
///
/// 标签内的双引号字符串里允许出现 `[`/`]` 而不闭合标签（&autoinsert 的
/// command 参数「可以包含括号」，见 docs/tag/preprocessor/autoinsert.md）。
/// 行尾未闭合的 `[...` 按文本处理（旧实现直接丢弃，这里保守地保留为文本）。
pub(crate) fn split_line_segments(line: &str) -> Vec<LineSegment> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut in_tag = false;
    let mut in_quote = false; // 仅在标签内部有效

    for ch in line.chars() {
        match ch {
            '"' if in_tag => {
                in_quote = !in_quote;
                current.push(ch);
            }
            '[' if !in_tag => {
                if !current.is_empty() {
                    segments.push(LineSegment::Text(std::mem::take(&mut current)));
                }
                in_tag = true;
            }
            ']' if in_tag && !in_quote => {
                segments.push(LineSegment::Tag(std::mem::take(&mut current)));
                in_tag = false;
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        if in_tag {
            // 未闭合的 '['：还原括号，按文本保留
            current.insert(0, '[');
        }
        segments.push(LineSegment::Text(current));
    }

    segments
}

/// 解析指令内容
fn parse_instruction(content: &str, line: usize) -> Result<Instruction> {
    let content = content.trim();

    if content.is_empty() {
        return Err(Error::ParseError {
            line,
            message: "空指令".to_string(),
        });
    }

    // 分割标签名和参数
    let mut parts = content.splitn(2, char::is_whitespace);
    let tag = parts.next().unwrap().trim().to_string();
    let params_str = parts.next().unwrap_or("").trim();

    let params = parse_params(params_str, line)?;

    Ok(Instruction { tag, params, line })
}

/// 解析参数字符串
fn parse_params(params_str: &str, line: usize) -> Result<HashMap<String, String>> {
    let mut params = HashMap::new();
    let mut chars = params_str.chars().peekable();
    let mut param_index = 0;

    while chars.peek().is_some() {
        // 跳过空白
        while chars.peek().map(|c| c.is_whitespace()).unwrap_or(false) {
            chars.next();
        }

        if chars.peek().is_none() {
            break;
        }

        // 读取键
        let mut key = String::new();
        while let Some(&c) = chars.peek() {
            if c == '=' || c.is_whitespace() {
                break;
            }
            key.push(chars.next().unwrap());
        }

        if key.is_empty() {
            break;
        }

        // 跳过空白
        while chars.peek().map(|c| c.is_whitespace()).unwrap_or(false) {
            chars.next();
        }

        // 检查是否有 =
        if chars.peek() != Some(&'=') {
            // 无值参数，使用索引作为键
            params.insert(param_index.to_string(), key);
            param_index += 1;
            continue;
        }

        // 消耗 =
        chars.next();

        // 跳过空白
        while chars.peek().map(|c| c.is_whitespace()).unwrap_or(false) {
            chars.next();
        }

        // 读取值
        let value = if chars.peek() == Some(&'"') {
            // 引号包裹的值
            chars.next(); // 消耗开头的引号
            let mut value = String::new();
            loop {
                match chars.next() {
                    Some('"') => break,
                    Some(c) => value.push(c),
                    None => {
                        return Err(Error::ParseError {
                            line,
                            message: "未闭合的引号".to_string(),
                        });
                    }
                }
            }
            value
        } else {
            // 无引号的值
            let mut value = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                value.push(chars.next().unwrap());
            }
            value
        };

        // 如果键是数字，转换为索引格式
        if key.chars().all(|c| c.is_ascii_digit()) {
            params.insert(key, value);
        } else {
            params.insert(key, value);
        }

        param_index += 1;
    }

    Ok(params)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_script() {
        let content = r#"
*main
[calllua function="scriptMainloop"]
[jump label="next"]
*next
[return]
"#;

        let script = Script::parse("test", content).unwrap();
        assert_eq!(script.labels.get("main"), Some(&0));
        assert_eq!(script.labels.get("next"), Some(&2));
        assert_eq!(script.instructions.len(), 3);
    }

    #[test]
    fn test_parse_params() {
        let content = r#"
*test
[uitrans 0="500" time="1000"]
[uitrans fade="$t.trns"]
[jump cond="$t.check==0" label="next"]
"#;

        let script = Script::parse("test", content).unwrap();

        let inst1 = &script.instructions[0];
        assert_eq!(inst1.tag, "uitrans");
        assert_eq!(inst1.get("0"), Some("500"));
        assert_eq!(inst1.get("time"), Some("1000"));

        let inst2 = &script.instructions[1];
        assert_eq!(inst2.get("fade"), Some("$t.trns"));

        let inst3 = &script.instructions[2];
        assert_eq!(inst3.get("cond"), Some("$t.check==0"));
        assert_eq!(inst3.get("label"), Some("next"));
    }

    #[test]
    fn test_parse_text() {
        let content = r#"
*main
这是剧情文本
[return]
"#;

        let script = Script::parse("test", content).unwrap();
        assert_eq!(script.instructions[0].tag, "__text");
        assert_eq!(script.instructions[0].get("text"), Some("这是剧情文本"));
    }

    #[test]
    fn test_get_label_line_empty_label_means_file_start() {
        // jump/call 的 label 缺省语义：空 label 跳到文件开头（行 0）。
        let content = r#"
[stop]
*main
[jump]
"#;
        let script = Script::parse("test", content).unwrap();
        assert_eq!(script.get_label_line(""), Some(0));
        assert_eq!(script.get_label_line("main"), Some(1));
        assert_eq!(script.get_label_line("missing"), None);
    }

    #[test]
    fn test_mixed_text_and_tags_in_one_line() {
        // Artemis 语义：未被 [ ] 包围的部分是剧情文本（docs/spec/script_syntax.md），
        // 文本段与标签段按行内顺序产出指令。
        let script = Script::parse("test", "[onlinehead]「ぷるぷる」@[rp]").unwrap();
        let tags: Vec<&str> = script.instructions.iter().map(|i| i.tag.as_str()).collect();
        assert_eq!(tags, vec!["onlinehead", "__text", "rp"]);
        assert_eq!(script.instructions[1].get("text"), Some("「ぷるぷる」@"));
    }

    #[test]
    fn test_trailing_line_comment_after_tag_is_dropped() {
        // 标签后的 // 行内注释不得变成剧情文本（保持旧解析器行为）
        let script = Script::parse("test", "[rt] // 注释 [rp]").unwrap();
        let tags: Vec<&str> = script.instructions.iter().map(|i| i.tag.as_str()).collect();
        assert_eq!(tags, vec!["rt"]);
    }

    #[test]
    fn test_parse_runs_autoinsert_pipeline() {
        // 预处理集成：blankline/linehead/lineend 注入后的指令流
        let content = "[&autoinsert target=\"blankline\" command=\"[onblankline]\"]\n[&autoinsert target=\"linehead\" command=\"[onlinehead]\"]\n[&autoinsert target=\"lineend\" command=\"[onlineend]\"]\n[スライム]\n「ぷるぷる」\n\nおわり";
        let script = Script::parse("test", content).unwrap();
        let tags: Vec<&str> = script.instructions.iter().map(|i| i.tag.as_str()).collect();
        assert_eq!(
            tags,
            vec![
                "スライム",
                "onlinehead",
                "__text",
                "onlineend",
                "onblankline",
                "onlinehead",
                "__text",
                "onlineend",
            ]
        );
        // 预处理一行进一行出：行号仍指向原始脚本
        assert_eq!(script.instructions[0].line, 4); // [スライム]
        assert_eq!(script.instructions[1].line, 5); // 「ぷるぷる」行
        assert_eq!(script.instructions[4].line, 6); // 空行展开
        assert_eq!(script.instructions[5].line, 7); // おわり行
    }

    #[test]
    fn test_parse_runs_scpsupport_pipeline() {
        // docs/tag/preprocessor/scpsupport.md 第二例
        let content = "[&scpsupport mode=\"init\"]\n[&scpsupport mode=\"add\" command=\"@\"]\n[&scpsupport mode=\"add\" command=\"rp\"]\n[&scpsupport mode=\"add\" char=\"】\" command=\"rt\"]\n【スライム】\n「ぷるぷる」";
        let script = Script::parse("test", content).unwrap();
        let tags: Vec<&str> = script.instructions.iter().map(|i| i.tag.as_str()).collect();
        assert_eq!(tags, vec!["__text", "rt", "__text", "rp"]);
        assert_eq!(script.instructions[0].get("text"), Some("【スライム】"));
        // 换行规则的 @ 并入文本段（与手写 「…」@[rp] 等价）
        assert_eq!(script.instructions[2].get("text"), Some("「ぷるぷる」@"));
    }

    #[test]
    fn test_parse_with_tag_ini_linetag() {
        let ini = preprocess::TagIni::from_pairs([("chara", vec!["file", "x", "y"])]);
        let content = "[&linetag allow=\"1\"]\nchara aya01,100,200\n「テキスト」";
        let script = Script::parse_with_tag_ini("test", content, Some(&ini)).unwrap();
        let inst = &script.instructions[0];
        assert_eq!(inst.tag, "chara");
        assert_eq!(inst.get("file"), Some("aya01"));
        assert_eq!(inst.get("x"), Some("100"));
        assert_eq!(inst.get("y"), Some("200"));
        assert_eq!(script.instructions[1].tag, "__text");
    }

    #[test]
    fn test_parse_uses_globally_installed_tag_ini() {
        // 全局注册路径：install_tag_ini 后 Script::parse 自动引用。
        // 注意：其它测试不依赖全局表（都用显式 API），并行下安全。
        let ini = preprocess::TagIni::from_pairs([("gtagx", vec!["p1", "p2"])]);
        preprocess::install_tag_ini(Some(ini));
        let content = "[&linetag allow=\"1\"]\ngtagx v1,v2";
        let script = Script::parse("test", content).unwrap();
        preprocess::install_tag_ini(None);

        let inst = &script.instructions[0];
        assert_eq!(inst.tag, "gtagx");
        assert_eq!(inst.get("p1"), Some("v1"));
        assert_eq!(inst.get("p2"), Some("v2"));
    }

    #[test]
    fn test_lua_block_still_parses_with_preprocessor_present() {
        // 预处理器不得改写 lua 块内部（含空行），也不影响块收集
        let content = "[&autoinsert target=\"blankline\" command=\"[rp]\"]\n[lua]\nlocal a = 1\n\nreturn a\n[/lua]\n[stop]";
        let script = Script::parse("test", content).unwrap();
        assert_eq!(script.instructions[0].tag, "__lua_block");
        assert_eq!(
            script.instructions[0].get("code"),
            Some("local a = 1\n\nreturn a")
        );
        assert_eq!(script.instructions[1].tag, "stop");
    }

    #[test]
    fn test_instruction_methods() {
        let inst = Instruction {
            tag: "test".to_string(),
            params: {
                let mut m = HashMap::new();
                m.insert("key1".to_string(), "value1".to_string());
                m.insert("0".to_string(), "default".to_string());
                m
            },
            line: 1,
        };

        assert_eq!(inst.get("key1"), Some("value1"));
        assert_eq!(inst.get("key2"), None);
        assert_eq!(inst.get_or("key2", "fallback"), "fallback");
        assert_eq!(inst.get_default(), Some("default"));
        assert!(inst.has("key1"));
        assert!(!inst.has("key2"));
    }
}
