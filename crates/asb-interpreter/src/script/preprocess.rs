//! 脚本预处理器：&autoinsert / &linetag / &scpsupport 三件套
//!
//! 在 `Script::parse` 真正解析指令之前，对脚本文本做一遍**有状态的逐行**
//! 变换（Artemis 预处理器语义，见 docs/tag/preprocessor/*.md）：
//!
//! - `[&autoinsert target=... command=...]`
//!   为其后的空行（blankline）、场景文本行头（linehead）、行尾（lineend）
//!   注入 command 字符串；command 可含 `[`/`]`（即可一次注入多个标签），
//!   command 缺省 = 取消该 target 的分配。
//! - `[&linetag allow=0/1 prefix=...]`
//!   行标签模式：把 `foo bar,hoge,fuga` 样式的行转换成 `[foo a="bar" ...]`
//!   普通标签；位置参数名顺序由 tag.ini 定义（见 [`TagIni`]）。
//! - `[&scpsupport mode=init/add char=... command=...]`
//!   脚本输入辅助：紧跟在场景文本后面的换行处按 add 顺序自动追加标签；
//!   char 指定时改为「行尾出现该字符时」追加；行尾不是场景文本（例如是
//!   标签）则不应用。
//!
//! 所有变换都是「一行进一行出」，因此预处理不改变行号，`Instruction::line`
//! 仍指向原始脚本行。
//!
//! tag.ini 的**文件加载入口**（从游戏资源读出文本）不在本模块职责内：宿主 /
//! 解释器侧读到内容后调用 [`TagIni::parse`] + [`install_tag_ini`] 注册即可，
//! 之后 `Script::parse` 会自动引用全局注册的表。

use std::collections::HashMap;
use std::sync::RwLock;

use super::{LineSegment, split_line_segments};

// ---------------------------------------------------------------------------
// tag.ini：行标签位置参数名表
// ---------------------------------------------------------------------------

/// tag.ini 解析结果：标签名 -> 位置参数名顺序表。
///
/// 行标签 `foo bar,hoge,fuga` 不写参数名，第 i 个逗号参数的参数名取
/// `param_names("foo")[i]`；表中查不到时退化为数字索引键（"0"、"1"…，
/// 与现有解析器的无名参数约定一致）。
///
/// 解析格式（宽容处理，逐行）：
/// - `;` / `#` / `//` 开头为注释行；
/// - `[section]` 段头行忽略；
/// - `标签名=参数1,参数2,...`（`=` 两侧空白忽略，参数名逗号分隔）。
#[derive(Debug, Clone, Default)]
pub struct TagIni {
    params: HashMap<String, Vec<String>>,
}

impl TagIni {
    /// 从 tag.ini 文本解析
    pub fn parse(content: &str) -> Self {
        let mut params = HashMap::new();
        for raw in content.lines() {
            let line = raw.trim();
            if line.is_empty()
                || line.starts_with(';')
                || line.starts_with('#')
                || line.starts_with("//")
            {
                continue;
            }
            // 段头行（如 [tags]）忽略
            if line.starts_with('[') {
                continue;
            }
            if let Some((name, rest)) = line.split_once('=') {
                let name = name.trim();
                if name.is_empty() {
                    continue;
                }
                let names: Vec<String> = rest
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                params.insert(name.to_string(), names);
            }
        }
        TagIni { params }
    }

    /// 程序化构造（宿主/测试用）
    pub fn from_pairs<I, S, T>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (S, Vec<T>)>,
        S: Into<String>,
        T: Into<String>,
    {
        let params = pairs
            .into_iter()
            .map(|(k, v)| (k.into(), v.into_iter().map(Into::into).collect()))
            .collect();
        TagIni { params }
    }

    /// 查询某标签的位置参数名顺序表
    pub fn param_names(&self, tag: &str) -> Option<&[String]> {
        self.params.get(tag).map(|v| v.as_slice())
    }

    /// 是否为空表
    pub fn is_empty(&self) -> bool {
        self.params.is_empty()
    }
}

/// 全局注册的 tag.ini（`Script::parse` 缺省引用）。
///
/// 解释器/宿主在启动阶段读到 tag.ini 文本后调用 [`install_tag_ini`] 注册；
/// 传 `None` 可以卸载。测试或特殊场景可用 `Script::parse_with_tag_ini`
/// 显式传表，绕开全局状态。
static GLOBAL_TAG_INI: RwLock<Option<TagIni>> = RwLock::new(None);

/// 注册 / 卸载全局 tag.ini
pub fn install_tag_ini(ini: Option<TagIni>) {
    *GLOBAL_TAG_INI.write().expect("GLOBAL_TAG_INI 写锁中毒") = ini;
}

/// 在全局 tag.ini 上执行只读操作
pub fn with_global_tag_ini<R>(f: impl FnOnce(Option<&TagIni>) -> R) -> R {
    let guard = GLOBAL_TAG_INI.read().expect("GLOBAL_TAG_INI 读锁中毒");
    f(guard.as_ref())
}

// ---------------------------------------------------------------------------
// 预处理器状态
// ---------------------------------------------------------------------------

/// scpsupport 规则：char 缺省（None）= 分配给换行符本身
#[derive(Debug, Clone)]
struct ScpRule {
    /// 行尾字符（串）；None 表示换行规则
    suffix: Option<String>,
    /// 分配的标签名（不含 `[`/`]`，单个标签；"@" 表示点击等待本身）
    command: String,
}

/// 有状态逐行预处理器
#[derive(Debug, Default)]
struct Preprocessor {
    /// &autoinsert target=blankline 的当前 command（None=未分配/已取消）
    blankline: Option<String>,
    /// &autoinsert target=linehead 的当前 command
    linehead: Option<String>,
    /// &autoinsert target=lineend 的当前 command
    lineend: Option<String>,
    /// &linetag allow 状态
    linetag_allow: bool,
    /// &linetag prefix 状态（None=不使用前缀）
    linetag_prefix: Option<String>,
    /// &scpsupport 规则表（add 顺序）
    scp_rules: Vec<ScpRule>,
    /// 是否处于 [lua] 块内部（块内内容原样透传，不做任何变换）
    in_lua: bool,
}

/// 对脚本全文做预处理，输出行数与输入完全一致（保持行号映射）。
pub fn preprocess(content: &str, tag_ini: Option<&TagIni>) -> String {
    let mut pp = Preprocessor::default();
    let mut out = String::with_capacity(content.len());
    for (idx, line) in content.lines().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        out.push_str(&pp.process_line(line, tag_ini));
    }
    out
}

impl Preprocessor {
    /// 处理单行，返回变换后的行（不含换行符）
    fn process_line(&mut self, raw: &str, tag_ini: Option<&TagIni>) -> String {
        // [lua] 块内部：原样透传（块内空行/文本绝不能被预处理器改写），
        // 直到遇到独占一行的 [/lua]（与 Script::parse 的收集规则一致）。
        if self.in_lua {
            if raw.trim() == "[/lua]" {
                self.in_lua = false;
            }
            return raw.to_string();
        }

        // 含 [lua] 起始标签的行：进入 lua 模式并整行透传，不做注入。
        if raw.contains('[') {
            let has_lua = split_line_segments(raw)
                .iter()
                .any(|seg| matches!(seg, LineSegment::Tag(inner) if inner.trim() == "lua"));
            if has_lua {
                self.in_lua = true;
                return raw.to_string();
            }
        }

        // 剥离并应用预处理指令（[&autoinsert]/[&linetag]/[&scpsupport]）。
        // 指令按行内出现顺序生效（内联生效顺序语义）；指令之外的残余内容
        // 继续走后续管线（典型脚本中指令独占一行，残余为空）。
        let (remainder, had_directive) = self.strip_and_apply_directives(raw);
        let trimmed = remainder.trim();

        // 空行：&autoinsert blankline 展开。指令行剥离后的空行**不算**脚本
        // 空行（它不是作者写下的空行），不触发展开。
        if trimmed.is_empty() {
            if had_directive {
                return String::new();
            }
            if let Some(cmd) = &self.blankline {
                return cmd.clone();
            }
            return remainder;
        }

        // 注释行与标签定义行（*label）不做任何变换
        if trimmed.starts_with("//") || trimmed.starts_with(';') || trimmed.starts_with('*') {
            return remainder;
        }

        // &linetag 行标签转换：转换成功后即为标签行，不再做行头/行尾注入
        if let Some(converted) = self.try_convert_linetag(trimmed, tag_ini) {
            return converted;
        }

        // ------ 场景内容行：行头/行尾注入 ------
        let mut out = trimmed.to_string();

        // 行头是否为场景文本：非 '['（标签开头）且非 '@'（点击等待命令）
        let head_is_text = !matches!(out.chars().next(), Some('[') | Some('@'));

        // 行尾是否为场景文本：非 ']'（标签结尾）且非 '@'（点击等待命令）。
        // docs/tag/preprocessor/scpsupport.md：换行前不是场景文本（例如是
        // 标签）则不应用；autoinsert lineend 采用同一判定。
        fn end_is_text(s: &str) -> bool {
            !matches!(s.chars().next_back(), Some(']') | Some('@') | None)
        }

        // 先应用 scpsupport（输入辅助 = 模拟作者在行尾补写），再应用
        // autoinsert lineend（若行尾已被 scp 追加了标签则不再是场景文本，
        // lineend 按判定自然跳过）。两者组合行为文档未定义，此处采用
        // 顺序重判定的保守语义。
        if end_is_text(&out) && !self.scp_rules.is_empty() {
            self.apply_scp_rules(&mut out);
        }
        if let Some(cmd) = &self.lineend
            && end_is_text(&out)
        {
            out.push_str(cmd);
        }
        if head_is_text && let Some(cmd) = &self.linehead {
            out.insert_str(0, cmd);
        }
        out
    }

    /// 从一行中剥离预处理指令并按出现顺序更新状态。
    /// 返回（剥离后的残余行, 是否含指令）。
    fn strip_and_apply_directives(&mut self, raw: &str) -> (String, bool) {
        if !raw.contains("[&") {
            return (raw.to_string(), false);
        }
        let mut had = false;
        let mut rebuilt = String::with_capacity(raw.len());
        for seg in split_line_segments(raw) {
            match seg {
                LineSegment::Tag(inner) => {
                    // 指令识别：紧贴 '[' 的 '&'（Artemis 书写惯例 [&xxx ...]）
                    if inner.starts_with('&') && self.apply_directive(&inner) {
                        had = true;
                        continue; // 指令从输出中移除
                    }
                    rebuilt.push('[');
                    rebuilt.push_str(&inner);
                    rebuilt.push(']');
                }
                LineSegment::Text(text) => rebuilt.push_str(&text),
            }
        }
        (rebuilt, had)
    }

    /// 应用单条预处理指令；返回是否为本模块认识的指令（认识才剥离）。
    fn apply_directive(&mut self, inner: &str) -> bool {
        let Ok(inst) = super::parse_instruction(inner, 0) else {
            return false;
        };
        match inst.tag.as_str() {
            "&autoinsert" => {
                // command 缺省=取消该 target 的分配（docs/tag/preprocessor/autoinsert.md）
                let cmd = inst.get("command").map(|s| s.to_string());
                match inst.get("target") {
                    Some("blankline") => self.blankline = cmd,
                    Some("linehead") => self.linehead = cmd,
                    Some("lineend") => self.lineend = cmd,
                    _ => {} // target 缺省/未知：no-op
                }
                true
            }
            "&linetag" => {
                if let Some(allow) = inst.get("allow") {
                    self.linetag_allow = allow.trim() == "1";
                }
                // prefix 缺省=不使用前缀（docs/tag/preprocessor/linetag.md），
                // 即每次指令都整体重设前缀状态。
                self.linetag_prefix = inst
                    .get("prefix")
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string());
                true
            }
            "&scpsupport" => {
                match inst.get("mode") {
                    Some("init") => self.scp_rules.clear(),
                    // addsp 为向后兼容保留（文档标注勿用），按 add 处理
                    Some("add") | Some("addsp") => {
                        if let Some(cmd) = inst.get("command") {
                            // command 不能包含 [ 或 ]，只能指定一个标签；
                            // 违反约束的规则直接忽略。
                            if !cmd.is_empty() && !cmd.contains('[') && !cmd.contains(']') {
                                self.scp_rules.push(ScpRule {
                                    suffix: inst
                                        .get("char")
                                        .filter(|s| !s.is_empty())
                                        .map(|s| s.to_string()),
                                    command: cmd.to_string(),
                                });
                            }
                        }
                    }
                    _ => {} // mode 缺省/未知：no-op
                }
                true
            }
            _ => false, // 其它 &xxx 交给后续解析器（保持现状）
        }
    }

    /// 在场景文本行尾应用 scpsupport 规则。
    ///
    /// 语义（由 docs/tag/preprocessor/scpsupport.md 的示例反推）：
    /// 若有 char 规则命中行尾字符，则**只**应用命中的 char 规则（按 add
    /// 顺序）；否则应用全部换行规则（char 缺省的规则，按 add 顺序）。
    /// 示例中『【スライム】』只得到 [rt] 而非 @[rp][rt]，即 char 命中时
    /// 换行规则不再追加。
    fn apply_scp_rules(&self, out: &mut String) {
        let char_hits: Vec<&ScpRule> = self
            .scp_rules
            .iter()
            .filter(|r| r.suffix.as_deref().is_some_and(|s| out.ends_with(s)))
            .collect();
        if !char_hits.is_empty() {
            for rule in char_hits {
                out.push_str(&format_scp_command(&rule.command));
            }
        } else {
            for rule in self.scp_rules.iter().filter(|r| r.suffix.is_none()) {
                out.push_str(&format_scp_command(&rule.command));
            }
        }
    }

    /// &linetag 行标签转换。
    ///
    /// - 前缀模式：仅带前缀的行视为行标签（代价：行首无法再写点击等待 @）；
    /// - 无前缀模式：以半角英数字开头的行视为行标签（行首空白已被忽略）。
    ///
    /// 转换 `foo bar,hoge,fuga` -> `[foo n0="bar" n1="hoge" n2="fuga"]`，
    /// 参数名 n{i} 取 tag.ini 中该标签的位置参数名表；查不到则退化为
    /// 数字索引键（与解析器无名参数约定一致）。
    fn try_convert_linetag(&self, trimmed: &str, tag_ini: Option<&TagIni>) -> Option<String> {
        if !self.linetag_allow {
            return None;
        }
        let body = match &self.linetag_prefix {
            Some(prefix) => trimmed.strip_prefix(prefix.as_str())?.trim_start(),
            None => trimmed,
        };
        // 行标签名以半角英数字开头；前缀后若不是（例如 @[rt]），按场景
        // 内容原样保留。
        if !body.chars().next()?.is_ascii_alphanumeric() {
            return None;
        }
        let (name, rest) = match body.split_once(char::is_whitespace) {
            Some((n, r)) => (n, r.trim()),
            None => (body, ""),
        };
        let mut out = String::from("[");
        out.push_str(name);
        if !rest.is_empty() {
            let names = tag_ini.and_then(|t| t.param_names(name));
            for (i, raw_arg) in rest.split(',').enumerate() {
                let mut value = raw_arg.trim();
                // 已带引号的参数去掉引号（组装时统一重新加引号）
                if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
                    value = &value[1..value.len() - 1];
                }
                let key = names
                    .and_then(|ns| ns.get(i))
                    .cloned()
                    .unwrap_or_else(|| i.to_string());
                out.push(' ');
                out.push_str(&key);
                out.push_str("=\"");
                out.push_str(value);
                out.push('"');
            }
        }
        out.push(']');
        Some(out)
    }
}

/// scpsupport 追加文本：命令 "@"（点击等待）按 Artemis 写法不带括号，
/// 其余命令包成 `[cmd]` 标签。
fn format_scp_command(command: &str) -> String {
    if command == "@" {
        "@".to_string()
    } else {
        format!("[{command}]")
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn pp(content: &str) -> String {
        preprocess(content, None)
    }

    fn lines(s: &str) -> Vec<&str> {
        s.lines().collect()
    }

    // ---- 通用 ----

    #[test]
    fn preprocess_keeps_line_count() {
        let src = "[&autoinsert target=\"blankline\" command=\"[rp]\"]\n\ntext\n\n[tag]";
        let out = pp(src);
        assert_eq!(out.lines().count(), src.lines().count());
    }

    #[test]
    fn no_directive_passthrough() {
        let src = "*main\n「テキスト」\n[jump label=\"x\"]";
        assert_eq!(pp(src), src);
    }

    #[test]
    fn unknown_amp_directive_is_kept() {
        // 未知的 &xxx 不剥离，交给后续解析器
        let src = "[&unknownpp foo=\"1\"]";
        assert_eq!(pp(src), src);
    }

    #[test]
    fn lua_block_is_passthrough() {
        // lua 块内的空行/文本不得被预处理改写
        let src = "[&autoinsert target=\"blankline\" command=\"[rp]\"]\n[lua]\nlocal a = 1\n\nreturn a\n[/lua]\n";
        let out = pp(src);
        let ls = lines(&out);
        assert_eq!(ls[1], "[lua]");
        assert_eq!(ls[2], "local a = 1");
        assert_eq!(ls[3], ""); // lua 块内空行不展开
        assert_eq!(ls[4], "return a");
        assert_eq!(ls[5], "[/lua]");
    }

    // ---- &autoinsert ----

    #[test]
    fn autoinsert_blankline_expands_and_cancels() {
        let src = "\n[&autoinsert target=\"blankline\" command=\"[onblankline]\"]\n\ntext\n[&autoinsert target=\"blankline\"]\n\nend";
        let out = pp(src);
        let ls = lines(&out);
        assert_eq!(ls[0], ""); // 指令之前的空行不展开
        assert_eq!(ls[1], ""); // 指令行剥离后为空行（且不触发展开）
        assert_eq!(ls[2], "[onblankline]"); // 指令之后空行展开
        assert_eq!(ls[4], ""); // 取消指令行
        assert_eq!(ls[5], ""); // 取消后不再展开
        assert_eq!(ls[6], "end");
    }

    #[test]
    fn autoinsert_linehead_and_lineend_on_scene_text() {
        let src = "[&autoinsert target=\"linehead\" command=\"[onlinehead]\"]\n[&autoinsert target=\"lineend\" command=\"[onlineend]\"]\n「ぷるぷる」\n[スライム]\n";
        let out = pp(src);
        let ls = lines(&out);
        // 场景文本行：行头 + 行尾均注入
        assert_eq!(ls[2], "[onlinehead]「ぷるぷる」[onlineend]");
        // 标签行（宏调用行）：不注入
        assert_eq!(ls[3], "[スライム]");
    }

    #[test]
    fn autoinsert_lineend_skips_tag_or_clickwait_end() {
        let src =
            "[&autoinsert target=\"lineend\" command=\"@[rt]\"]\ntext@[rp]\ntext@\nプレーン\n";
        let out = pp(src);
        let ls = lines(&out);
        assert_eq!(ls[1], "text@[rp]"); // 行尾是标签：不注入
        assert_eq!(ls[2], "text@"); // 行尾是点击等待：不注入
        assert_eq!(ls[3], "プレーン@[rt]"); // 行尾是场景文本：注入字面 command
    }

    #[test]
    fn autoinsert_linehead_skips_tag_label_comment() {
        let src = "[&autoinsert target=\"linehead\" command=\"[h]\"]\n[rt]text\n*label\n// comment\n@text\n";
        let out = pp(src);
        let ls = lines(&out);
        assert_eq!(ls[1], "[rt]text"); // 行头是标签：不注入
        assert_eq!(ls[2], "*label"); // 标签定义行：不变换
        assert_eq!(ls[3], "// comment"); // 注释行：不变换
        assert_eq!(ls[4], "@text"); // 行头是点击等待命令：不注入
    }

    #[test]
    fn autoinsert_doc_example_shape() {
        // docs/tag/preprocessor/autoinsert.md 简化脚本
        let src = "[&autoinsert target=\"blankline\" command=\"[onblankline]\"]\n[&autoinsert target=\"linehead\"  command=\"[onlinehead]\" ]\n[&autoinsert target=\"lineend\"   command=\"[onlineend]\"  ]\n[スライム]\n「ぷるぷる」\n\nスライムはぷるぷるしているようだ。\n";
        let out = pp(src);
        let ls = lines(&out);
        assert_eq!(ls[3], "[スライム]");
        assert_eq!(ls[4], "[onlinehead]「ぷるぷる」[onlineend]");
        assert_eq!(ls[5], "[onblankline]");
        assert_eq!(
            ls[6],
            "[onlinehead]スライムはぷるぷるしているようだ。[onlineend]"
        );
    }

    // ---- &scpsupport ----

    #[test]
    fn scpsupport_newline_rules_append_in_add_order() {
        // docs/tag/preprocessor/scpsupport.md 首例：每行自动 @ + [rp]
        let src = "[&scpsupport mode=\"init\"]\n[&scpsupport mode=\"add\" command=\"@\"]\n[&scpsupport mode=\"add\" command=\"rp\"]\nシナリオです。\n２ページ目です。\n";
        let out = pp(src);
        let ls = lines(&out);
        assert_eq!(ls[3], "シナリオです。@[rp]");
        assert_eq!(ls[4], "２ページ目です。@[rp]");
    }

    #[test]
    fn scpsupport_char_rule_overrides_newline_rules() {
        // docs 第二例：char="】" 命中时只追加 [rt]，不追加 @[rp]
        let src = "[&scpsupport mode=\"init\"]\n[&scpsupport mode=\"add\" command=\"@\"]\n[&scpsupport mode=\"add\" command=\"rp\"]\n[&scpsupport mode=\"add\" char=\"】\" command=\"rt\"]\n【スライム】\n「ぷるぷる」\n";
        let out = pp(src);
        let ls = lines(&out);
        assert_eq!(ls[4], "【スライム】[rt]");
        assert_eq!(ls[5], "「ぷるぷる」@[rp]");
    }

    #[test]
    fn scpsupport_not_applied_after_tag_end() {
        // 换行前不是场景文本（是标签/点击等待）则不应用
        let src = "[&scpsupport mode=\"init\"]\n[&scpsupport mode=\"add\" command=\"rp\"]\n[bg file=\"x\"]\nテキスト@\n空行不管\n";
        let out = pp(src);
        let ls = lines(&out);
        assert_eq!(ls[2], "[bg file=\"x\"]");
        assert_eq!(ls[3], "テキスト@");
        assert_eq!(ls[4], "空行不管[rp]");
    }

    #[test]
    fn scpsupport_init_clears_stacked_rules() {
        // 规则叠加后 init 清空
        let src = "[&scpsupport mode=\"add\" command=\"@\"]\n[&scpsupport mode=\"add\" command=\"rp\"]\nいち\n[&scpsupport mode=\"init\"]\nに\n";
        let out = pp(src);
        let ls = lines(&out);
        assert_eq!(ls[2], "いち@[rp]");
        assert_eq!(ls[4], "に"); // init 之后规则清空
    }

    #[test]
    fn scpsupport_rejects_bracketed_command() {
        // command 不能包含 [ 或 ]（docs），违规规则忽略
        let src = "[&scpsupport mode=\"add\" command=\"[rp]\"]\nテキスト\n";
        let out = pp(src);
        assert_eq!(lines(&out)[1], "テキスト");
    }

    #[test]
    fn scpsupport_addsp_treated_as_add() {
        let src = "[&scpsupport mode=\"addsp\" command=\"rp\"]\nテキスト\n";
        let out = pp(src);
        assert_eq!(lines(&out)[1], "テキスト[rp]");
    }

    // ---- &linetag ----

    #[test]
    fn linetag_no_prefix_converts_alnum_head_lines() {
        let ini = TagIni::from_pairs([("foo", vec!["a", "b", "c"])]);
        let src = "[&linetag allow=\"1\"]\n  foo bar,hoge,fuga\n「場面テキスト」\n";
        let out = preprocess(src, Some(&ini));
        let ls = lines(&out);
        // 行首空白忽略；位置参数按 tag.ini 命名
        assert_eq!(ls[1], "[foo a=\"bar\" b=\"hoge\" c=\"fuga\"]");
        // 非半角英数字开头：仍是场景文本
        assert_eq!(ls[2], "「場面テキスト」");
    }

    #[test]
    fn linetag_disabled_keeps_lines_as_text() {
        let ini = TagIni::from_pairs([("foo", vec!["a"])]);
        let src = "foo bar\n[&linetag allow=\"1\"]\nfoo bar\n[&linetag allow=\"0\"]\nfoo bar\n";
        let out = preprocess(src, Some(&ini));
        let ls = lines(&out);
        assert_eq!(ls[0], "foo bar"); // 启用前
        assert_eq!(ls[2], "[foo a=\"bar\"]"); // 启用后
        assert_eq!(ls[4], "foo bar"); // 禁用后
    }

    #[test]
    fn linetag_prefix_mode_and_scene_text_conflict_rule() {
        let ini = TagIni::from_pairs([("foo", vec!["a", "b"])]);
        let src = "[&linetag allow=\"1\" prefix=\"@\"]\n@foo bar,hoge\nfoo bar,hoge\n@[rt]\n";
        let out = preprocess(src, Some(&ini));
        let ls = lines(&out);
        // 带前缀 -> 行标签
        assert_eq!(ls[1], "[foo a=\"bar\" b=\"hoge\"]");
        // 不带前缀 -> 场景文本（即使以半角英数字开头）
        assert_eq!(ls[2], "foo bar,hoge");
        // 前缀后不是英数字（点击等待写法）：原样保留
        assert_eq!(ls[3], "@[rt]");
    }

    #[test]
    fn linetag_prefix_reset_when_param_omitted() {
        // prefix 缺省=不使用前缀：后一条指令未带 prefix 即整体重设
        let ini = TagIni::from_pairs([("foo", vec!["a"])]);
        let src = "[&linetag allow=\"1\" prefix=\"@\"]\n[&linetag allow=\"1\"]\nfoo bar\n";
        let out = preprocess(src, Some(&ini));
        assert_eq!(lines(&out)[2], "[foo a=\"bar\"]");
    }

    #[test]
    fn linetag_without_tag_ini_falls_back_to_index_keys() {
        let src = "[&linetag allow=\"1\"]\nfoo bar,hoge\n";
        let out = preprocess(src, None);
        assert_eq!(lines(&out)[1], "[foo 0=\"bar\" 1=\"hoge\"]");
    }

    #[test]
    fn linetag_no_args_and_quoted_args() {
        let ini = TagIni::from_pairs([("foo", vec!["a"])]);
        let src = "[&linetag allow=\"1\"]\nfoo\nfoo \"b a r\"\n";
        let out = preprocess(src, Some(&ini));
        let ls = lines(&out);
        assert_eq!(ls[1], "[foo]");
        assert_eq!(ls[2], "[foo a=\"b a r\"]");
    }

    #[test]
    fn linetag_converted_line_skips_injection() {
        // 行标签转换后的行是标签行，不做行尾注入
        let ini = TagIni::from_pairs([("foo", vec!["a"])]);
        let src = "[&linetag allow=\"1\"]\n[&scpsupport mode=\"add\" command=\"rp\"]\n[&autoinsert target=\"lineend\" command=\"[e]\"]\nfoo bar\n「テキスト」\n";
        let out = preprocess(src, Some(&ini));
        let ls = lines(&out);
        assert_eq!(ls[3], "[foo a=\"bar\"]");
        // 场景文本行：scp 先追加（行尾变为标签），lineend 不再追加
        assert_eq!(ls[4], "「テキスト」[rp]");
    }

    // ---- tag.ini ----

    #[test]
    fn tag_ini_parse_format() {
        let src =
            "; 注释\n# 注释\n// 注释\n[tags]\nfoo=a,b,c\nbar = x , y\n\nbroken_line_without_eq\n";
        let ini = TagIni::parse(src);
        assert_eq!(
            ini.param_names("foo"),
            Some(&["a".to_string(), "b".to_string(), "c".to_string()][..])
        );
        assert_eq!(
            ini.param_names("bar"),
            Some(&["x".to_string(), "y".to_string()][..])
        );
        assert_eq!(ini.param_names("broken_line_without_eq"), None);
        assert_eq!(ini.param_names("missing"), None);
    }
}
