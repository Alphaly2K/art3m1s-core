//! 回溯日志（backlog）子系统。
//!
//! 按页存储剧情历史：每页 = 文本段落 + 再现该页所需的标签序列
//! （font / print / rt / ruby 等）。对应 Artemis 的以下标签与查询：
//! - `backlog`：allow / messagelayer / includefont / hide / layer / clear；
//! - `writebacklog`：mode=1 时换页把当前页存入历史（文档缺省 0 不存入）；
//! - `rp` 的 backlog 参数：0/1 逐次覆盖 writebacklog 的设置；
//! - `var system=get_backlog_size / get_backlog_tags / get_message_tags`：
//!   查询接口在 [`crate::text::render::FontState`] 上（`get_backlog_*`），
//!   数据来源即本模块的 [`Backlog`]。
//!
//! 页内标签的记录由 `GlyphTextRenderer` 在消费文本事件时同步进行
//! （见 `MessageLayer::page_tags`），换页（rp）时按配置搬入 [`Backlog`]。

use std::collections::{HashMap, VecDeque};

/// 历史页数上限的缺省值。超过后丢弃最旧的页。
pub const DEFAULT_BACKLOG_MAX_PAGES: usize = 100;

/// 转义标签参数值：反斜杠与双引号需要转义，才能安全放进 `key="value"`。
fn escape_attr(v: &str) -> String {
    v.replace('\\', "\\\\").replace('"', "\\\"")
}

/// 把 raw 参数表序列化为 `[name k="v" …]` 形式的标签字符串。
/// 键按字典序排序，保证输出确定（可测试、可比对存档）。
fn tag_with_params(name: &str, params: &HashMap<String, String>) -> String {
    let mut keys: Vec<&String> = params.keys().collect();
    keys.sort();
    let mut s = format!("[{name}");
    for k in keys {
        s.push_str(&format!(" {k}=\"{}\"", escape_attr(&params[k])));
    }
    s.push(']');
    s
}

/// 历史页里记录的单条「再现标签」。
///
/// `get_backlog_tags` / `get_message_tags` 把它们序列化为可交给
/// `tag` 标签逐条执行的标签字符串，从而重现该页文本。
#[derive(Debug, Clone, PartialEq)]
pub enum BacklogTag {
    /// 剧情文本段落。再现时以 `[print data="…"]` 执行（print 标签用于
    /// 把字符串显示为剧情文本，正是再现场景所需）。
    Text(String),
    /// 换行，再现为 `[rt]`。
    LineBreak,
    /// 字体设置，再现为 `[font k="v" …]`（参数表原样保留）。
    Font(HashMap<String, String>),
    /// 注音开始，再现为 `[ruby text="…"]`。
    RubyStart(String),
    /// 注音结束，再现为 `[/ruby]`。
    RubyEnd,
}

impl BacklogTag {
    /// 序列化为可交给 `tag` 标签执行的标签字符串。
    pub fn to_tag_string(&self) -> String {
        match self {
            BacklogTag::Text(t) => format!("[print data=\"{}\"]", escape_attr(t)),
            BacklogTag::LineBreak => "[rt]".to_string(),
            BacklogTag::Font(params) => tag_with_params("font", params),
            BacklogTag::RubyStart(t) => format!("[ruby text=\"{}\"]", escape_attr(t)),
            BacklogTag::RubyEnd => "[/ruby]".to_string(),
        }
    }
}

/// 一页历史：文本段落 + 再现所需的标签序列。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BacklogPage {
    /// 页首字体快照（页开始时当前字体的 raw 参数表）。
    /// `get_backlog_tags` 的 allfont=1 时作为开头的 `[font …]` 输出；
    /// `backlog includefont=0` 时不存（为 None）。
    pub page_font: Option<HashMap<String, String>>,
    /// 页内按发生顺序记录的再现标签序列。
    pub tags: Vec<BacklogTag>,
}

impl BacklogPage {
    /// 该页是否含有实际文本（无文本的页不值得入库）。
    pub fn has_text(&self) -> bool {
        self.tags.iter().any(|t| matches!(t, BacklogTag::Text(_)))
    }

    /// 该页的纯文本内容（换行为 `\n`），供历史界面直接显示。
    pub fn plain_text(&self) -> String {
        let mut s = String::new();
        for t in &self.tags {
            match t {
                BacklogTag::Text(t) => s.push_str(t),
                BacklogTag::LineBreak => s.push('\n'),
                _ => {}
            }
        }
        s
    }

    /// 再现该页所需的标签字符串序列。
    ///
    /// `allfont=true` 时在开头附上页首字体的 `[font …]` 标签
    /// （对应 get_backlog_tags 的 allfont=1；页首字体缺失时不附）。
    pub fn reproduction_tags(&self, allfont: bool) -> Vec<String> {
        let mut out = Vec::with_capacity(self.tags.len() + 1);
        if allfont
            && let Some(f) = &self.page_font
            && !f.is_empty()
        {
            out.push(tag_with_params("font", f));
        }
        out.extend(self.tags.iter().map(BacklogTag::to_tag_string));
        out
    }
}

/// `backlog` 标签的配置（allow 之外的参数解释器尚未透传，
/// 字段与缺省值先按文档备好，事件字段补齐后即可直接落值）。
#[derive(Debug, Clone, PartialEq)]
pub struct BacklogSettings {
    /// allow=0 禁止使用历史文本 / 1 允许（禁止时换页不入库）。
    pub allow: bool,
    /// 历史文本用的消息层 ID（文档默认 "backlog"）。
    pub message_layer: String,
    /// 是否把字体信息带入历史（文档默认 1）。
    /// 为 false 时入库的页不含页首字体与页内 font 标签。
    pub include_font: bool,
    /// 进入历史时临时隐藏的图层 ID 数组。
    pub hide: Vec<String>,
    /// 进入历史时自动显示且 visible 同步的图层 ID（缺省禁用）。
    pub layer: Option<String>,
}

impl Default for BacklogSettings {
    fn default() -> Self {
        Self {
            allow: true,
            message_layer: "backlog".to_string(),
            include_font: true,
            hide: Vec::new(),
            layer: None,
        }
    }
}

/// 历史存储本体：按页保存，超过上限丢最旧页。
#[derive(Debug, Default)]
pub struct Backlog {
    pages: VecDeque<BacklogPage>,
    /// 页数上限（0 视为不限制不合理，构造时给缺省值）。
    pub max_pages: usize,
    /// `writebacklog` 的 mode：true=换页存入历史。文档缺省 0（不存入）。
    pub write_mode: bool,
    /// `backlog` 标签的配置。
    pub settings: BacklogSettings,
}

impl Backlog {
    pub fn new() -> Self {
        Self {
            pages: VecDeque::new(),
            max_pages: DEFAULT_BACKLOG_MAX_PAGES,
            write_mode: false,
            settings: BacklogSettings::default(),
        }
    }

    /// 已存页数（get_backlog_size 的数据源）。
    pub fn size(&self) -> usize {
        self.pages.len()
    }

    /// 取第 `page` 页（0 起，0=最旧页）。
    pub fn page(&self, page: usize) -> Option<&BacklogPage> {
        self.pages.get(page)
    }

    /// `backlog clear=1`：清除当前存储的全部历史。
    pub fn clear(&mut self) {
        self.pages.clear();
    }

    /// `writebacklog mode=` 的消费入口。
    pub fn set_write_mode(&mut self, mode: bool) {
        self.write_mode = mode;
    }

    /// `rp` 换页时是否入库：backlog 参数 0/1 无视 writebacklog，
    /// 缺省（None 或其他值）按 writebacklog 的 mode。
    pub fn should_store(&self, rp_backlog: Option<i32>) -> bool {
        match rp_backlog {
            Some(1) => true,
            Some(0) => false,
            _ => self.write_mode,
        }
    }

    /// 把一页存入历史：
    /// - allow=false 或页内无文本时丢弃；
    /// - includefont=false 时剥掉页首字体与页内 font 标签；
    /// - 超过 max_pages 丢最旧页。
    pub fn push_page(&mut self, mut page: BacklogPage) {
        if !self.settings.allow || !page.has_text() {
            return;
        }
        if !self.settings.include_font {
            page.page_font = None;
            page.tags.retain(|t| !matches!(t, BacklogTag::Font(_)));
        }
        self.pages.push_back(page);
        while self.max_pages > 0 && self.pages.len() > self.max_pages {
            self.pages.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_page(s: &str) -> BacklogPage {
        BacklogPage {
            page_font: Some(HashMap::from([("size".to_string(), "40".to_string())])),
            tags: vec![BacklogTag::Text(s.to_string())],
        }
    }

    #[test]
    fn tag_serialization_matches_document_forms() {
        assert_eq!(
            BacklogTag::Text("こんにちは".into()).to_tag_string(),
            "[print data=\"こんにちは\"]"
        );
        assert_eq!(BacklogTag::LineBreak.to_tag_string(), "[rt]");
        assert_eq!(
            BacklogTag::RubyStart("カピバラ".into()).to_tag_string(),
            "[ruby text=\"カピバラ\"]"
        );
        assert_eq!(BacklogTag::RubyEnd.to_tag_string(), "[/ruby]");
        // font 参数按键排序，输出确定
        let font = BacklogTag::Font(HashMap::from([
            ("size".to_string(), "40".to_string()),
            ("color".to_string(), "FFFFFF".to_string()),
        ]));
        assert_eq!(font.to_tag_string(), "[font color=\"FFFFFF\" size=\"40\"]");
        // 引号与反斜杠转义
        assert_eq!(
            BacklogTag::Text("a\"b\\c".into()).to_tag_string(),
            "[print data=\"a\\\"b\\\\c\"]"
        );
    }

    #[test]
    fn should_store_follows_rp_override_then_write_mode() {
        let mut b = Backlog::new();
        // writebacklog 缺省 0：rp 缺省不入库
        assert!(!b.should_store(None));
        // rp backlog=1 无视 writebacklog
        assert!(b.should_store(Some(1)));
        b.set_write_mode(true);
        assert!(b.should_store(None));
        // rp backlog=0 无视 writebacklog
        assert!(!b.should_store(Some(0)));
    }

    #[test]
    fn push_page_respects_allow_and_caps_pages() {
        let mut b = Backlog::new();
        b.max_pages = 2;
        b.push_page(text_page("p1"));
        b.push_page(text_page("p2"));
        b.push_page(text_page("p3"));
        assert_eq!(b.size(), 2, "超过上限应丢最旧页");
        assert_eq!(b.page(0).unwrap().plain_text(), "p2");
        assert_eq!(b.page(1).unwrap().plain_text(), "p3");

        // 无文本的页不入库
        b.push_page(BacklogPage::default());
        assert_eq!(b.size(), 2);

        // allow=0 禁止后不再入库
        b.settings.allow = false;
        b.push_page(text_page("p4"));
        assert_eq!(b.size(), 2);

        b.clear();
        assert_eq!(b.size(), 0);
    }

    #[test]
    fn include_font_zero_strips_font_info() {
        let mut b = Backlog::new();
        b.settings.include_font = false;
        let mut page = text_page("hello");
        page.tags.insert(
            0,
            BacklogTag::Font(HashMap::from([("size".to_string(), "20".to_string())])),
        );
        b.push_page(page);
        let stored = b.page(0).unwrap();
        assert!(stored.page_font.is_none(), "includefont=0 不存页首字体");
        assert!(
            !stored.tags.iter().any(|t| matches!(t, BacklogTag::Font(_))),
            "includefont=0 应剥掉页内 font 标签"
        );
    }

    #[test]
    fn reproduction_tags_allfont_controls_page_head_font() {
        let page = BacklogPage {
            page_font: Some(HashMap::from([("size".to_string(), "40".to_string())])),
            tags: vec![
                BacklogTag::Text("あい".into()),
                BacklogTag::LineBreak,
                BacklogTag::Text("うえ".into()),
            ],
        };
        assert_eq!(
            page.reproduction_tags(false),
            vec!["[print data=\"あい\"]", "[rt]", "[print data=\"うえ\"]"]
        );
        assert_eq!(
            page.reproduction_tags(true)[0],
            "[font size=\"40\"]",
            "allfont=1 应附页首 font 标签"
        );
        assert_eq!(page.plain_text(), "あい\nうえ");
    }
}
