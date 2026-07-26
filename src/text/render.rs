//! 文本渲染抽象。
//!
//! 后端实现 [`TextRenderer`] trait 来把解释器的文本事件翻译成绘制命令。

use crate::compositor::anim::Easing;
use crate::render_pipeline::draw::{DrawCommand, TextureProvider};
use crate::text::backlog::{Backlog, BacklogTag};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// 字体描述
// ---------------------------------------------------------------------------

/// 逻辑字体描述。
///
/// 来自 `FontSettings` 事件的 raw map 会被解析为该结构。未被识别的键进 `custom`，
/// 后端可按需使用。
#[derive(Debug, Clone, Default)]
pub struct FontDesc {
    /// 字体文件路径
    pub face: Option<String>,
    /// 字号（像素）
    pub size: Option<f32>,
    /// 注音字号
    pub ruby_size: Option<f32>,
    /// 注音字体
    pub ruby_face: Option<String>,
    /// 字体颜色：`RRGGBB` 格式
    pub color: Option<String>,
    /// 描边色
    pub outline_color: Option<String>,
    /// 阴影色
    pub shadow_color: Option<String>,
    /// 行间距
    pub line_height: Option<f32>,
    /// 字符间距
    pub kerning: Option<f32>,
    /// 粗体
    pub bold: Option<bool>,
    /// 斜体
    pub italic: Option<bool>,
    /// 下划线
    pub underline: Option<bool>,
    /// 删除线
    pub strikeout: Option<bool>,
    /// 原始样式字符串（如 "outline,shadow,bold,italic,underline,strikeout"）
    pub style: Option<String>,
    /// 描边宽度（像素）
    pub outline_size: Option<f32>,
    /// 阴影偏移距离（像素）
    pub shadow_size: Option<f32>,
    /// 注音描边宽度（像素）
    pub ruby_outline_size: Option<f32>,
    /// 注音阴影偏移距离（像素）
    pub ruby_shadow_size: Option<f32>,
    /// 行顶到注音的间距
    pub spacetop: Option<f32>,
    /// 注音到正文的间距
    pub spacemiddle: Option<f32>,
    /// 正文到行底的间距
    pub spacebottom: Option<f32>,
    /// 注音字间距
    pub ruby_kerning: Option<f32>,
    /// 文本对齐
    pub align: Option<String>,
    /// 超出后是否截断或换行
    pub overflow: Option<String>,
    /// 是否竖排
    pub vertical: Option<bool>,
    /// 是否存储到字体栈（默认 1=true）
    pub stack: Option<bool>,
    /// 悬挂处理（禁止符处理）
    pub hung: Option<bool>,
    /// 每字符透明度 0-255
    pub alpha: Option<u8>,
    /// 每字符水平缩放（百分比）
    pub xscale: Option<f32>,
    /// 每字符垂直缩放（百分比）
    pub yscale: Option<f32>,
    /// 每字符旋转角度 0-359
    pub rotate: Option<f32>,
    /// 每字符图层混合模式
    pub layer_mode: Option<String>,
    /// 整个文本块的透明度 0-255
    pub entire_alpha: Option<u8>,
    /// 整个文本块的水平缩放（百分比）
    pub entire_xscale: Option<f32>,
    /// 整个文本块的垂直缩放（百分比）
    pub entire_yscale: Option<f32>,
    /// 整个文本块的旋转角度
    pub entire_rotate: Option<f32>,
    /// 整个文本块的锚点 X 坐标
    pub entire_anchorx: Option<f32>,
    /// 整个文本块的锚点 Y 坐标
    pub entire_anchory: Option<f32>,
    /// 锚点是否固定在页面中心
    pub anchorcenter: Option<bool>,
    /// 未被识别的属性，原样保留
    pub custom: HashMap<String, String>,
}

impl FontDesc {
    pub fn from_raw(raw: &HashMap<String, String>) -> Self {
        let mut desc = FontDesc::default();
        desc.merge_raw(raw);
        desc
    }

    pub fn merge_raw(&mut self, raw: &HashMap<String, String>) {
        for (key, value) in raw {
            let v = value.trim();
            match key.as_str() {
                "face" => self.face = Some(v.to_string()),
                "size" => self.size = v.parse().ok(),
                "rubyface" => self.ruby_face = Some(v.to_string()),
                "rubysize" => self.ruby_size = v.parse().ok(),
                "color" => self.color = Some(v.to_string()),
                "outlinecolor" => self.outline_color = Some(v.to_string()),
                "shadowcolor" => self.shadow_color = Some(v.to_string()),
                "height" => self.line_height = v.parse().ok(),
                "kerning" => self.kerning = v.parse().ok(),
                "shadow" => self.shadow_size = v.parse().ok(),
                "outline" => self.outline_size = v.parse().ok(),
                "rubyshadow" => self.ruby_shadow_size = v.parse().ok(),
                "rubyoutline" => self.ruby_outline_size = v.parse().ok(),
                "spacetop" => self.spacetop = v.parse().ok(),
                "spacemiddle" => self.spacemiddle = v.parse().ok(),
                "spacebottom" => self.spacebottom = v.parse().ok(),
                "rubykerning" => self.ruby_kerning = v.parse().ok(),
                "alpha" => self.alpha = v.parse::<i32>().ok().map(|n| n.clamp(0, 255) as u8),
                "xscale" => self.xscale = v.parse().ok(),
                "yscale" => self.yscale = v.parse().ok(),
                "rotate" => self.rotate = v.parse().ok(),
                "layermode" => self.layer_mode = Some(v.to_string()),
                "entirealpha" => {
                    self.entire_alpha = v.parse::<i32>().ok().map(|n| n.clamp(0, 255) as u8);
                }
                "entirexscale" => self.entire_xscale = v.parse().ok(),
                "entireyscale" => self.entire_yscale = v.parse().ok(),
                "entirerotate" => self.entire_rotate = v.parse().ok(),
                "entireanchorx" => self.entire_anchorx = v.parse().ok(),
                "entireanchory" => self.entire_anchory = v.parse().ok(),
                "anchorcenter" => {
                    self.anchorcenter = Some(matches!(v, "1" | "true"));
                }
                "stack" => {
                    self.stack = Some(matches!(v, "1" | "true"));
                }
                "align" => self.align = Some(v.to_string()),
                "overflow" => self.overflow = Some(v.to_string()),
                "vertical" => self.vertical = Some(matches!(v, "1" | "true")),
                "style" => {
                    self.style = Some(v.to_string());
                    for part in v.split(',') {
                        match part.trim() {
                            "bold" => self.bold = Some(true),
                            "italic" => self.italic = Some(true),
                            "underline" => self.underline = Some(true),
                            "strikeout" => self.strikeout = Some(true),
                            _ => {}
                        }
                    }
                }
                _ => {
                    self.custom.insert(key.clone(), v.to_string());
                }
            }
        }
    }

    /// 把已设置的字段序列化回 raw 参数表（键名与 [`FontDesc::merge_raw`] 对应）。
    ///
    /// 用于 backlog 的「页首字体快照」：get_backlog_tags / get_message_tags 的
    /// allfont=1 时以 `[font …]` 输出，让历史页可以完整重现页首字体。
    /// stack 参数不输出（fontdefault/再现用途都不需要栈操作）。
    pub fn to_raw(&self) -> HashMap<String, String> {
        // f32 序列化：整数值不带小数点（40 而不是 40.0），与脚本书写习惯一致
        fn num(v: f32) -> String {
            if v.fract() == 0.0 {
                format!("{}", v as i64)
            } else {
                format!("{v}")
            }
        }
        fn boolean(v: bool) -> String {
            if v { "1".into() } else { "0".into() }
        }
        let mut m = HashMap::new();
        if let Some(v) = &self.face {
            m.insert("face".into(), v.clone());
        }
        if let Some(v) = self.size {
            m.insert("size".into(), num(v));
        }
        if let Some(v) = &self.ruby_face {
            m.insert("rubyface".into(), v.clone());
        }
        if let Some(v) = self.ruby_size {
            m.insert("rubysize".into(), num(v));
        }
        if let Some(v) = &self.color {
            m.insert("color".into(), v.clone());
        }
        if let Some(v) = &self.outline_color {
            m.insert("outlinecolor".into(), v.clone());
        }
        if let Some(v) = &self.shadow_color {
            m.insert("shadowcolor".into(), v.clone());
        }
        if let Some(v) = self.line_height {
            m.insert("height".into(), num(v));
        }
        if let Some(v) = self.kerning {
            m.insert("kerning".into(), num(v));
        }
        if let Some(v) = self.shadow_size {
            m.insert("shadow".into(), num(v));
        }
        if let Some(v) = self.outline_size {
            m.insert("outline".into(), num(v));
        }
        if let Some(v) = self.ruby_shadow_size {
            m.insert("rubyshadow".into(), num(v));
        }
        if let Some(v) = self.ruby_outline_size {
            m.insert("rubyoutline".into(), num(v));
        }
        if let Some(v) = self.spacetop {
            m.insert("spacetop".into(), num(v));
        }
        if let Some(v) = self.spacemiddle {
            m.insert("spacemiddle".into(), num(v));
        }
        if let Some(v) = self.spacebottom {
            m.insert("spacebottom".into(), num(v));
        }
        if let Some(v) = self.ruby_kerning {
            m.insert("rubykerning".into(), num(v));
        }
        if let Some(v) = self.alpha {
            m.insert("alpha".into(), v.to_string());
        }
        if let Some(v) = self.xscale {
            m.insert("xscale".into(), num(v));
        }
        if let Some(v) = self.yscale {
            m.insert("yscale".into(), num(v));
        }
        if let Some(v) = self.rotate {
            m.insert("rotate".into(), num(v));
        }
        if let Some(v) = &self.layer_mode {
            m.insert("layermode".into(), v.clone());
        }
        if let Some(v) = self.entire_alpha {
            m.insert("entirealpha".into(), v.to_string());
        }
        if let Some(v) = self.entire_xscale {
            m.insert("entirexscale".into(), num(v));
        }
        if let Some(v) = self.entire_yscale {
            m.insert("entireyscale".into(), num(v));
        }
        if let Some(v) = self.entire_rotate {
            m.insert("entirerotate".into(), num(v));
        }
        if let Some(v) = self.entire_anchorx {
            m.insert("entireanchorx".into(), num(v));
        }
        if let Some(v) = self.entire_anchory {
            m.insert("entireanchory".into(), num(v));
        }
        if let Some(v) = self.anchorcenter {
            m.insert("anchorcenter".into(), boolean(v));
        }
        if let Some(v) = &self.align {
            m.insert("align".into(), v.clone());
        }
        if let Some(v) = &self.overflow {
            m.insert("overflow".into(), v.clone());
        }
        if let Some(v) = self.vertical {
            m.insert("vertical".into(), boolean(v));
        }
        if let Some(v) = &self.style {
            m.insert("style".into(), v.clone());
        }
        for (k, v) in &self.custom {
            m.insert(k.clone(), v.clone());
        }
        m
    }

    /// 获取每字符的透明度（归一化 0.0-1.0）。
    pub fn char_alpha(&self) -> f32 {
        self.alpha.unwrap_or(255) as f32 / 255.0
    }

    /// 获取整个文本块的透明度（归一化 0.0-1.0）。
    pub fn entire_alpha(&self) -> f32 {
        self.entire_alpha.unwrap_or(255) as f32 / 255.0
    }
}

// ---------------------------------------------------------------------------
// 文本排版配置（prohibit / wordparts / indent / rt）
// ---------------------------------------------------------------------------

/// Artemis 默认行首禁则字符（`prohibit` 标签 head 参数的缺省值）。
/// 这些字符不能出现在行首，自动换行时会"上提"悬挂在上一行行尾。
pub const DEFAULT_PROHIBIT_HEAD: &str = "!?%)]},.:;、。，．・：；！？」』）｝〕］】";

/// Artemis 默认行尾禁则字符（`prohibit` 标签 foot 参数的缺省值）。
/// 这些字符不能出现在行尾，自动换行时会"下移"到下一行行首。
pub const DEFAULT_PROHIBIT_FOOT: &str = "([{「『（｛〔［【";

/// Artemis 默认单词组成字符（`wordparts` 标签 parts 参数的缺省值）。
/// 连续的这些字符视为一个单词，自动换行时不在单词中间断开。
pub const DEFAULT_WORDPARTS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// 文本排版配置：禁则处理 / 单词整体换行 / 对话缩进 / rt 空行省略。
///
/// 对应 Artemis 的 `prohibit`、`wordparts`、`indent`、`rt` 标签。
/// 解释器事件到来时通过 [`FontState::set_prohibit`] 等入口覆盖；
/// 在参数透传打通前，先以内置默认集生效。
#[derive(Debug, Clone)]
pub struct TextLayoutConfig {
    /// 行首禁则字符集合（连续字符串，无分隔符）
    pub prohibit_head: String,
    /// 行尾禁则字符集合（连续字符串，无分隔符）
    pub prohibit_foot: String,
    /// 视为单词组成部分的字符集合（连续字符串，无分隔符）
    pub wordparts: String,
    /// 缩进开始/结束字符，每两个字符一组交替列出（如 "「」『』"）。
    /// 文档未给出缺省值，缺省为空即禁用缩进，等待 `indent` 标签配置。
    pub indent_pair: String,
    /// 从行首数 N 个字符之后出现的缩进开始字符不识别；None=任意位置都识别
    pub indent_range: Option<usize>,
    /// true=已处于缩进状态时重复嵌套缩进；false（缺省）=忽略后续开始字符
    pub indent_nest: bool,
    /// `rt` 标签 omitblankline：true 时若最后一行为空行则不换行。
    /// 解释器尚未透传该参数，按任务约定先内置默认行为 1（true）。
    pub rt_omit_blank_line: bool,
}

impl Default for TextLayoutConfig {
    fn default() -> Self {
        Self {
            prohibit_head: DEFAULT_PROHIBIT_HEAD.to_string(),
            prohibit_foot: DEFAULT_PROHIBIT_FOOT.to_string(),
            wordparts: DEFAULT_WORDPARTS.to_string(),
            indent_pair: String::new(),
            indent_range: None,
            indent_nest: false,
            rt_omit_blank_line: true,
        }
    }
}

impl TextLayoutConfig {
    /// 判断字符是否为行首禁则字符。
    pub fn is_prohibit_head(&self, c: char) -> bool {
        self.prohibit_head.contains(c)
    }

    /// 判断字符是否为行尾禁则字符。
    pub fn is_prohibit_foot(&self, c: char) -> bool {
        self.prohibit_foot.contains(c)
    }

    /// 判断字符是否为单词组成部分。
    pub fn is_wordpart(&self, c: char) -> bool {
        self.wordparts.contains(c)
    }

    /// 若 `c` 是缩进开始字符，返回其对应的缩进结束字符。
    ///
    /// `indent_pair` 每两个字符一组：偶数位是开始字符，其后一位是结束字符。
    pub fn indent_close_for(&self, c: char) -> Option<char> {
        let chars: Vec<char> = self.indent_pair.chars().collect();
        chars
            .chunks_exact(2)
            .find(|pair| pair[0] == c)
            .map(|pair| pair[1])
    }
}

// ---------------------------------------------------------------------------
// 点击等待图标配置（glyph 标签）
// ---------------------------------------------------------------------------

/// `glyph` 标签的解析结果：点击等待图标的图层与偏移配置。
#[derive(Debug, Clone, Default)]
pub struct GlyphIconConfig {
    /// 行末图标的图层 ID；缺省禁用行末图标
    pub layer: Option<String>,
    /// 页末图标的图层 ID；缺省与行末同图层，两者都缺省则禁用点击等待图标
    pub rplayer: Option<String>,
    /// 行末图标相对最后一个字符右端的 X 偏移（可负）
    pub left: f32,
    /// 行末图标相对最后一个字符顶部的 Y 偏移（可负）
    pub top: f32,
    /// 页末图标的 X 偏移
    pub rpleft: f32,
    /// 页末图标的 Y 偏移
    pub rptop: f32,
    /// true=图标跟随文本末尾移动；false（缺省）=不改图层 left/top
    pub homing: bool,
}

impl GlyphIconConfig {
    /// 从 `glyph` 标签的 raw 参数表解析。
    pub fn from_raw(raw: &HashMap<String, String>) -> Self {
        let non_empty = |key: &str| raw.get(key).map(|v| v.trim()).filter(|v| !v.is_empty());
        Self {
            layer: non_empty("layer").map(str::to_string),
            rplayer: non_empty("rplayer").map(str::to_string),
            left: non_empty("left").and_then(|v| v.parse().ok()).unwrap_or(0.0),
            top: non_empty("top").and_then(|v| v.parse().ok()).unwrap_or(0.0),
            rpleft: non_empty("rpleft")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0),
            rptop: non_empty("rptop")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0),
            homing: non_empty("homing").map(|v| v == "1").unwrap_or(false),
        }
    }
}

/// 点击等待图标的摆放信息。
///
/// 由 [`TextRenderer::click_wait_icon_placement`] 计算；runtime 在进入
/// 点击等待/换页等待时据此把 `layer_id` 图层移动到位并置为可见。
#[derive(Debug, Clone, PartialEq)]
pub struct ClickWaitIconPlacement {
    /// 图标图层 ID（行末用 layer，页末用 rplayer，页末缺省回退到 layer）
    pub layer_id: String,
    /// 图标左端目标 X（= 消息层 left + 最后字符右端 + glyph 标签偏移）
    pub left: f32,
    /// 图标顶端目标 Y（= 消息层 top + 最后字符所在行顶部 + glyph 标签偏移）
    pub top: f32,
    /// homing=1 时 runtime 才应移动图层的 left/top；0 时只切换可见性
    pub homing: bool,
}

// ---------------------------------------------------------------------------
// 字形信息
// ---------------------------------------------------------------------------

/// 单一字形的度量与纹理信息。
#[derive(Debug, Clone)]
pub struct GlyphInfo {
    /// UTF-8 字符序列
    pub character: String,
    /// 字形在 atlas 中的纹理 ID
    pub texture_id: crate::render_pipeline::draw::TextureId,
    /// 字形在 atlas 中的像素位置与尺寸
    pub atlas_x: f32,
    pub atlas_y: f32,
    pub atlas_w: f32,
    pub atlas_h: f32,
    /// 字形在文本行中的基线偏移（像素）
    pub offset_x: f32,
    pub offset_y: f32,
    /// 字形本身的像素尺寸
    pub width: f32,
    pub height: f32,
    /// 该字形到下一个字形的步进距离
    pub advance_x: f32,
}

/// 一段已经写入消息层的文本位置，用于后台翻译完成后的非阻塞替换。
///
/// `generation` 随消息层清页递增，因此网络请求迟到时不会把译文写入下一页。
#[derive(Debug, Clone, PartialEq)]
pub struct TextSpanToken {
    pub layer_id: String,
    pub generation: u64,
    pub start: usize,
    pub end: usize,
    pub page_tag_index: usize,
    pub font_size: f32,
    pub font_face: Option<String>,
}

// ---------------------------------------------------------------------------
// 字体度量
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct FontMetrics {
    pub line_height: f32,
    pub baseline: f32,
    pub ascent: f32,
    pub descent: f32,
    pub em_width: f32,
}

// ---------------------------------------------------------------------------
// link / ruby 区间标记
// ---------------------------------------------------------------------------

/// `link`~`/link` 标记的可点击链接区间。
///
/// 记录字符范围（text_buffer 下标）与跳转参数；点击命中后 runtime
/// 应以 file/label 触发解释器 jump（等价于 jump 标签）。
#[derive(Debug, Clone, PartialEq)]
pub struct LinkRange {
    /// 起始字符下标（含）
    pub start: usize,
    /// 结束字符下标（不含）；None=尚未闭合（延伸到缓冲末尾）
    pub end: Option<usize>,
    /// jump 标签的 file 参数
    pub file: Option<String>,
    /// jump 标签的 label 参数
    pub label: Option<String>,
    /// 强调显示类型：0=白色方形板加法合成渐变叠加（缺省）/ 1=更改文字颜色
    pub link_type: i32,
    /// type=1 时强调显示的文字颜色 RRGGBB（缺省 0x000000）
    pub color: Option<String>,
    /// type=1 时强调显示的文字阴影颜色 RRGGBB（缺省 0x000000）
    pub shadow_color: Option<String>,
    /// type=1 时强调显示的文字轮廓颜色 RRGGBB（缺省 0x000000）
    pub outline_color: Option<String>,
    /// 当前是否处于鼠标 hover 强调状态
    pub hovered: bool,
}

impl LinkRange {
    /// 实际结束下标：未闭合的链接延伸到缓冲末尾。
    pub fn end_or(&self, buffer_len: usize) -> usize {
        self.end.unwrap_or(buffer_len).min(buffer_len)
    }

    /// 字符下标 `i` 是否落在该链接区间内。
    pub fn contains_index(&self, i: usize, buffer_len: usize) -> bool {
        i >= self.start && i < self.end_or(buffer_len)
    }
}

/// `ruby`~`/ruby` 标记的注音区间。
///
/// 注音字形在 `/ruby` 闭合时按 rubysize 光栅化并存于 `glyphs`；
/// 排版时该区间视为不可分割单元（带注音的文本中间不自动换行），
/// 注音串水平居中排在正文区间上方。
#[derive(Debug, Clone)]
pub struct RubyRange {
    /// 正文起始字符下标（含）
    pub start: usize,
    /// 正文结束字符下标（不含）
    pub end: usize,
    /// 注音字符串
    pub text: String,
    /// 光栅化注音时使用的字号（rubysize，缺省为正文字号的一半）
    pub size: f32,
    /// 已按 `size` 光栅化的注音字形
    pub glyphs: Vec<GlyphInfo>,
}

/// 链接命中区域：一段链接在某一行上的屏幕矩形。
///
/// 由 [`TextRenderer::link_hit_areas`] 计算；runtime 用它做鼠标命中检测：
/// 命中后点击 → 以 file/label 触发 jump；移动 → 调
/// [`TextRenderer::update_link_hover`] 刷新 hover 强调。
#[derive(Debug, Clone, PartialEq)]
pub struct LinkHitArea {
    /// 所在消息层 ID
    pub layer_id: String,
    /// 该链接在层内 links 列表中的下标（同一链接跨行时多个区域共享）
    pub link_index: usize,
    /// 矩形左上角 X（已含消息层 left）
    pub left: f32,
    /// 矩形左上角 Y（已含消息层 top）
    pub top: f32,
    pub width: f32,
    pub height: f32,
    /// 跳转目标（jump 的 file/label）
    pub file: Option<String>,
    pub label: Option<String>,
}

impl LinkHitArea {
    /// 点 (x, y) 是否落在该命中区域内。
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.left && x < self.left + self.width && y >= self.top && y < self.top + self.height
    }
}

// ---------------------------------------------------------------------------
// 消息层
// ---------------------------------------------------------------------------

/// 文本显示区域（消息层）的描述。
#[derive(Debug, Clone)]
pub struct MessageLayer {
    pub id: String,
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    pub layer_index: i32,
    pub visible: bool,
    /// 当前字体描述
    pub font: FontDesc,
    /// 字体栈
    pub font_stack: Vec<FontDesc>,
    /// 该层的文本缓存
    pub text_buffer: Vec<GlyphInfo>,
    /// 当前页面代次；每次清页递增，用于拒绝迟到的异步文本更新。
    pub generation: u64,
    /// 逐字显示：当前已揭示的字符数
    pub reveal_index: usize,
    /// 逐字显示：本层的新文本是否正在等待揭示
    pub reveal_pending: bool,
    /// 逐字显示配置（仅本层生效）
    ///
    /// 一个层可同时拥有多个 scetween 配置（如 show + hide + in），
    /// 每个配置控制不同的动画属性（alpha / left / top / ...）。
    /// 入场动画由 `ScetweenMode::is_entrance()` 为 true 的配置驱动；
    /// 退场动画由非入场的配置驱动，在 `hide_text()` 后播放。
    pub scetween: Vec<ScetweenConfig>,
    /// 逐字显示内部时钟（毫秒），追踪自 reveal 开始以来的时间
    pub reveal_clock_ms: u64,
    /// 文本是否处于"隐藏"状态（sceout 之后）——决定播放入场还是退场动画
    pub text_hidden: bool,
    /// 本页的链接区间（含未闭合的最后一项，end=None）
    pub links: Vec<LinkRange>,
    /// 本页已闭合的注音区间
    pub rubies: Vec<RubyRange>,
    /// 进行中的注音：`[ruby]` 已见、`[/ruby]` 未到（起始下标, 注音文本）
    pub open_ruby: Option<(usize, String)>,
    /// 本页已执行的再现标签序列（font/print/rt/ruby …）。
    /// get_message_tags 直接读它；rp 换页时按配置搬入 backlog。
    pub page_tags: Vec<BacklogTag>,
    /// 页首字体快照（页开始时当前字体的 raw 参数表），allfont=1 时输出
    pub page_font: HashMap<String, String>,
}

impl MessageLayer {
    pub fn new(id: String) -> Self {
        Self {
            id,
            left: 0.0,
            top: 0.0,
            width: 0.0,
            height: 0.0,
            layer_index: 0,
            visible: true,
            font: FontDesc::default(),
            font_stack: Vec::new(),
            text_buffer: Vec::new(),
            generation: 0,
            reveal_index: 0,
            reveal_pending: false,
            scetween: Vec::new(),
            reveal_clock_ms: 0,
            text_hidden: false,
            links: Vec::new(),
            rubies: Vec::new(),
            open_ruby: None,
            page_tags: Vec::new(),
            page_font: HashMap::new(),
        }
    }

    /// 清空本页的文本与页内标记（换页/切层清缓冲时同步调用，
    /// 否则 link/ruby 区间与再现标签会指向已清空的缓冲）。
    pub fn clear_page(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.text_buffer.clear();
        self.links.clear();
        self.rubies.clear();
        self.open_ruby = None;
        self.page_tags.clear();
        self.page_font = self.font.to_raw();
    }

    /// 排版时不可拆行的区间集合（注音区间 + 进行中的注音）。
    pub fn keep_ranges(&self) -> Vec<(usize, usize)> {
        let mut v: Vec<(usize, usize)> = self.rubies.iter().map(|r| (r.start, r.end)).collect();
        if let Some((start, _)) = &self.open_ruby {
            v.push((*start, self.text_buffer.len()));
        }
        v
    }
}

// ---------------------------------------------------------------------------
// 逐字显示配置（Scetween）
// ---------------------------------------------------------------------------

/// 逐字显示动画模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScetweenMode {
    /// 出现（文本逐字出现）
    In,
    /// 退场（文本逐字消失）
    Out,
    /// 通过 scein 显示
    Show,
    /// 通过 sceout 隐藏
    Hide,
    /// 向过去的后台中逐页出现
    BacklogDownIn,
    /// 向过去的后台中逐页退场
    BacklogDownOut,
    /// 向现在的后台中逐页出现
    BacklogUpIn,
    /// 向现在的后台中逐页退场
    BacklogUpOut,
}

impl ScetweenMode {
    pub fn from_str(s: &str) -> Self {
        match s.trim() {
            "in" => Self::In,
            "out" => Self::Out,
            "show" => Self::Show,
            "hide" => Self::Hide,
            "backlog_down_in" => Self::BacklogDownIn,
            "backlog_down_out" => Self::BacklogDownOut,
            "backlog_up_in" => Self::BacklogUpIn,
            "backlog_up_out" => Self::BacklogUpOut,
            _ => Self::In,
        }
    }

    /// 是否为"出现"类动画（reveal 递增而非递减）。
    pub fn is_entrance(&self) -> bool {
        matches!(
            self,
            Self::In | Self::Show | Self::BacklogDownIn | Self::BacklogUpIn
        )
    }
}

/// 逐字显示的动画参数配置。
///
/// 对应 Artemis 的 `scetween` 标签，控制每个字符出现/消失时的缓动效果。
#[derive(Debug, Clone)]
pub struct ScetweenConfig {
    /// 动画模式
    pub mode: ScetweenMode,
    /// 设置模式：init（替换）或 add（添加）
    pub set_mode: ScetweenSetMode,
    /// 动画目标属性（如 "alpha"、"left"、"top"、"xscale"、"yscale"、"rotate"）
    pub param: Option<String>,
    /// 缓动函数
    pub ease: Easing,
    /// 属性值与正常值之间的差值
    pub diff: Option<f32>,
    /// 每个字符延迟时间（毫秒）
    pub delay_per_char: u64,
    /// 单个字符的动画时长（毫秒）
    pub time_per_char: u64,
    /// 是否随机顺序显示
    pub random_delay: bool,
    /// 随机显示时使用的字符顺序
    pub random_order: Option<Vec<usize>>,
}

impl Default for ScetweenConfig {
    fn default() -> Self {
        Self {
            mode: ScetweenMode::In,
            set_mode: ScetweenSetMode::Init,
            param: None,
            ease: Easing::default(),
            diff: None,
            delay_per_char: 0,
            time_per_char: 0,
            random_delay: false,
            random_order: None,
        }
    }
}

impl ScetweenConfig {
    /// 从 `TextAnimation` 事件参数构建逐字动画配置。
    pub fn from_params(params: &HashMap<String, String>) -> Self {
        let mode = params
            .get("type")
            .map(|s| ScetweenMode::from_str(s))
            .unwrap_or(ScetweenMode::In);

        let set_mode = match params.get("mode").map(|s| s.as_str()) {
            Some("add") => ScetweenSetMode::Add,
            _ => ScetweenSetMode::Init,
        };

        Self {
            mode,
            set_mode,
            param: params.get("param").cloned(),
            ease: Easing::parse(params.get("ease").map(|s| s.as_str()).unwrap_or("")),
            diff: params.get("diff").and_then(|v| v.parse().ok()),
            delay_per_char: params
                .get("delay")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            time_per_char: params.get("time").and_then(|v| v.parse().ok()).unwrap_or(0),
            random_delay: params.get("randomdelay").map(|v| v == "1").unwrap_or(false),
            random_order: None,
        }
    }

    /// param 是否为 entire*（整页属性，如 entireleft/entirealpha）。
    ///
    /// 整页动画作用于全部字符，不做逐字符延迟：所有字符同步插值。
    pub fn is_entire_param(&self) -> bool {
        self.param
            .as_deref()
            .is_some_and(|p| p.starts_with("entire"))
    }
}

/// Scetween 设置模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScetweenSetMode {
    /// 替换指定 type 的动画设置
    Init,
    /// 添加指定 type 的动画设置
    Add,
}

// ---------------------------------------------------------------------------
// 字体状态
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct FontState {
    pub layers: HashMap<String, MessageLayer>,
    pub active_layer: Option<String>,
    pub layer_stack: Vec<String>,
    pub default_font: FontDesc,
    pub ruby_enabled: bool,
    pub inside_ruby: bool,
    pub alignment: TextAlignment,
    pub glyph_config: HashMap<String, String>,
    /// `glyph` 标签的解析结果（点击等待图标）
    pub glyph_icon: GlyphIconConfig,
    /// 排版配置：禁则 / wordparts / 缩进 / rt 空行省略
    pub layout: TextLayoutConfig,
    pub custom: HashMap<String, String>,
    pub layers_dirtied_this_frame: Vec<String>,
    /// 逐字显示：全局 reveal 时钟（毫秒）
    pub reveal_clock_ms: u64,
    /// 链接总开关：linkdisable=false / linkenable=true（缺省启用）。
    /// 禁用时链接不可点击（命中检测返回空）也不做 hover 强调。
    pub links_enabled: bool,
    /// 回溯日志（backlog）历史存储与相关配置
    pub backlog: Backlog,
}

impl Default for FontState {
    fn default() -> Self {
        Self::new()
    }
}

impl FontState {
    pub fn new() -> Self {
        Self {
            layers: HashMap::new(),
            active_layer: None,
            layer_stack: Vec::new(),
            default_font: FontDesc::default(),
            ruby_enabled: false,
            inside_ruby: false,
            alignment: TextAlignment::default(),
            glyph_config: HashMap::new(),
            glyph_icon: GlyphIconConfig::default(),
            layout: TextLayoutConfig::default(),
            custom: HashMap::new(),
            layers_dirtied_this_frame: Vec::new(),
            reveal_clock_ms: 0,
            links_enabled: true,
            backlog: Backlog::new(),
        }
    }

    // -------------------------------------------------------------------
    // backlog / 再现标签查询接口
    //
    // 对应 var system=get_backlog_size / get_backlog_tags / get_message_tags。
    // runtime 桥接（解释器 var 路径回调落值成伪数组）不在本层；解释器侧
    // 通过回调调用这三个方法后，把返回的字符串序列写成 name.0..N + name.size
    // 即可（伪数组约定见 implode 标签文档）。
    // -------------------------------------------------------------------

    /// 内置回溯日志已存页数（var system=get_backlog_size）。
    pub fn get_backlog_size(&self) -> usize {
        self.backlog.size()
    }

    /// 取回溯日志第 `page` 页（0 起，0=最旧页）的再现标签集
    /// （var system=get_backlog_tags）。
    ///
    /// `allfont=true` 时在开头附上再现页首字体的 `[font …]` 标签。
    /// 页码越界返回 None。
    pub fn get_backlog_tags(&self, page: usize, allfont: bool) -> Option<Vec<String>> {
        self.backlog
            .page(page)
            .map(|p| p.reproduction_tags(allfont))
    }

    /// 取消息层 `id` 当前显示文本的再现标签集（var system=get_message_tags）。
    ///
    /// `allfont=true` 时在开头附上页首字体的 `[font …]` 标签。
    /// 目标层不存在返回 None。
    pub fn get_message_tags(&self, id: &str, allfont: bool) -> Option<Vec<String>> {
        let layer = self.layers.get(id)?;
        let mut out = Vec::with_capacity(layer.page_tags.len() + 1);
        if allfont && !layer.page_font.is_empty() {
            let font_tag = BacklogTag::Font(layer.page_font.clone());
            out.push(font_tag.to_tag_string());
        }
        out.extend(layer.page_tags.iter().map(BacklogTag::to_tag_string));
        Some(out)
    }

    /// `prohibit` 标签的配置入口：覆盖行首/行尾禁则字符集。
    ///
    /// None 表示该参数缺省，保持当前值（内置默认或此前配置）。
    pub fn set_prohibit(&mut self, head: Option<&str>, foot: Option<&str>) {
        if let Some(head) = head {
            self.layout.prohibit_head = head.to_string();
        }
        if let Some(foot) = foot {
            self.layout.prohibit_foot = foot.to_string();
        }
    }

    /// `wordparts` 标签的配置入口：覆盖单词组成字符集。
    pub fn set_wordparts(&mut self, parts: &str) {
        self.layout.wordparts = parts.to_string();
    }

    /// `indent` 标签的配置入口：设置对话缩进的字符对 / 识别范围 / 嵌套行为。
    ///
    /// None 表示该参数缺省：pair 保持当前值；range 为任意位置识别；nest 为不嵌套。
    pub fn set_indent(&mut self, pair: Option<&str>, range: Option<usize>, nest: Option<bool>) {
        if let Some(pair) = pair {
            self.layout.indent_pair = pair.to_string();
        }
        self.layout.indent_range = range;
        self.layout.indent_nest = nest.unwrap_or(false);
    }

    /// `rt` 标签 omitblankline 参数的配置入口。
    pub fn set_rt_omit_blank_line(&mut self, omit: bool) {
        self.layout.rt_omit_blank_line = omit;
    }

    pub fn active_layer_mut(&mut self) -> &mut MessageLayer {
        let id = self
            .active_layer
            .get_or_insert_with(|| crate::text::glyph::DEFAULT_MESSAGE_LAYER.to_string())
            .clone();
        // fontdefault：首次使用（新建）的消息层自动应用默认字体设置，
        // 而不是空白的 FontDesc::default()（fontinit 时也恢复到该默认）。
        let default_font = &self.default_font;
        self.layers.entry(id.clone()).or_insert_with(|| {
            let mut layer = MessageLayer::new(id.clone());
            layer.font = default_font.clone();
            layer.page_font = default_font.to_raw();
            layer
        });
        self.layers.get_mut(&id).unwrap()
    }
}

// ---------------------------------------------------------------------------
// 文本对齐
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextAlignment {
    #[default]
    Left,
    Center,
    Right,
    /// 两端对齐（均等分配字符间距）
    Equalize,
}

impl From<&str> for TextAlignment {
    fn from(s: &str) -> Self {
        match s.trim() {
            "center" | "centre" => Self::Center,
            "right" => Self::Right,
            "equalize" | "justify" => Self::Equalize,
            _ => Self::Left,
        }
    }
}

// ---------------------------------------------------------------------------
// TextRenderer trait
// ---------------------------------------------------------------------------

pub trait TextRenderer {
    /// 切换当前用于光栅化的字体文件。
    fn set_font_bytes(&mut self, bytes: &'static [u8]) -> Result<(), String>;

    /// 当前消息层的逻辑字体文件，用于消息层/字体栈恢复后同步光栅化字体。
    fn active_font_face(&self) -> Option<&str>;

    /// 应用字体属性。
    fn apply_font_settings(&mut self, settings: &HashMap<String, String>);

    /// 重置当前字体为默认值。
    fn font_init(&mut self);

    /// 保存当前字体到栈。
    fn font_pop(&mut self);

    /// 设置默认字体。
    fn font_default(&mut self, settings: &HashMap<String, String>);

    /// 切换活动消息层。
    /// 切换当前消息层。`stack=false`（chgmsg stack=0）时不把前一层压入堆栈，
    /// 用于防止存档中消息层堆栈无限膨胀。
    fn switch_message_layer(&mut self, id: Option<&str>, stack: bool);

    /// 弹出消息层。
    fn pop_message_layer(&mut self);

    /// 设置点击等待图标参数。
    fn set_glyph_config(&mut self, config: &HashMap<String, String>);

    /// 查询点击等待图标应摆放的位置。
    ///
    /// `page_end`=false 为行末等待（用 layer + left/top），true 为页末等待
    /// （用 rplayer + rpleft/rptop，rplayer 缺省回退到 layer）。
    /// 返回 None 表示未配置图标图层或当前层无文本。
    /// runtime 在进入点击等待时调用，把返回的图层移动到位并显示。
    fn click_wait_icon_placement(&self, _page_end: bool) -> Option<ClickWaitIconPlacement> {
        None
    }

    /// 追加一段剧情文本。
    fn push_text(&mut self, content: &str, inline: bool);

    /// 追加文本并返回可供异步替换的稳定位置。非字形后端可只追加并返回 None。
    fn push_text_tracked(&mut self, content: &str, inline: bool) -> Option<TextSpanToken> {
        self.push_text(content, inline);
        None
    }

    /// 替换仍位于当前页面的文本片段，返回字形数量变化量。
    fn replace_text_span(&mut self, _span: &TextSpanToken, _content: &str) -> Option<isize> {
        None
    }

    /// 文本换行。
    fn push_line_break(&mut self);

    /// 文本分页。
    ///
    /// `backlog`：rp 标签的同名参数——Some(1) 换页前文本存历史（无视
    /// writebacklog）、Some(0) 不存（无视 writebacklog）、None 按
    /// writebacklog 的 mode 设置处理。
    fn push_page_break(&mut self, backlog: Option<i32>);

    // -------------------------------------------------------------------
    // ruby / link 接口
    // -------------------------------------------------------------------

    /// `[ruby text=…]`：开始注音区间（此后追加的文本为注音的正文）。
    fn ruby_start(&mut self, _text: &str) {}

    /// `[/ruby]`：闭合注音区间，按 rubysize 光栅化注音字形。
    fn ruby_end(&mut self) {}

    /// `[link]`：开始链接区间。
    ///
    /// `link_type`：0=白色方形板加法合成渐变叠加（缺省）/ 1=更改文字颜色；
    /// color/shadow_color/outline_color 为 type=1 强调时的 RRGGBB（缺省 000000）。
    /// 解释器事件目前只带 color，shadow/outline 颜色字段补齐后直接传入即可。
    fn link_start(
        &mut self,
        _file: Option<&str>,
        _label: Option<&str>,
        _link_type: i32,
        _color: Option<&str>,
        _shadow_color: Option<&str>,
        _outline_color: Option<&str>,
    ) {
    }

    /// `[/link]`：闭合链接区间。
    fn link_end(&mut self) {}

    /// linkenable(true) / linkdisable(false)：链接总开关。
    /// 禁用时不可点击（命中区域为空）也不做 hover 强调。
    fn set_links_enabled(&mut self, _enabled: bool) {}

    /// 全部已启用链接的命中区域（每行一个矩形，坐标已含消息层偏移）。
    ///
    /// runtime 做鼠标命中检测用：点击命中后以区域里的 file/label 触发 jump。
    fn link_hit_areas(&self) -> Vec<LinkHitArea> {
        Vec::new()
    }

    /// 命中测试：返回包含点 (x, y) 的第一个链接区域。
    fn link_hit_test(&self, x: f32, y: f32) -> Option<LinkHitArea> {
        self.link_hit_areas().into_iter().find(|a| a.contains(x, y))
    }

    /// 按鼠标位置刷新链接 hover 强调状态；返回是否有链接的 hover 状态改变
    /// （runtime 据此决定是否需要重绘）。
    fn update_link_hover(&mut self, _x: f32, _y: f32) -> bool {
        false
    }

    /// 当前消息层已存文本的度量：`(整体宽度, 总高度, 最后一行宽度)`。
    /// 供 var system=get_message_layer_width/height/line_width 查询。
    /// 无文本/无渲染器时返回 `None`（查询落 0）。
    fn active_layer_text_metrics(&self) -> Option<(f32, f32, f32)> {
        None
    }

    /// 获取字形绘制命令，按层 ID 分组。
    ///
    /// 逐字显示模式下，只返回 `reveal_index` 之前（含）的字形；
    /// 每个可见字形根据 [`ScetweenConfig`] 计算其当前动画状态。
    fn build_text_commands(
        &mut self,
        provider: &mut dyn TextureProvider,
    ) -> HashMap<String, Vec<DrawCommand>>;

    // -------------------------------------------------------------------
    // 逐字显示（Scetween）接口
    // -------------------------------------------------------------------

    /// 在当前活动层上设置 scetween 配置。
    ///
    /// 对应 Artemis 的 `scetween` 标签。`set_mode` 为 `init` 时替换同 type 的设置，
    /// 为 `add` 时添加新设置。
    fn set_scetween(&mut self, config: ScetweenConfig);

    /// 重置当前活动层的逐字显示进度：将 reveal 归零并标记为待揭示。
    ///
    /// 在 push_text / push_line_break 后自动调用。
    fn reset_reveal(&mut self);

    /// 推进逐字显示时钟。宿主每帧调用一次。
    ///
    /// 根据各层的 [`ScetweenConfig`] 中的 `delay_per_char` 参数，
    /// 逐步增加 `reveal_index` 以逐字显示文本。
    fn advance_reveal(&mut self, delta_ms: u64);

    /// 立即揭示当前活动层的全部文本（跳过逐字动画）。
    fn reveal_all(&mut self);

    /// 隐藏当前活动层的文本（用于 sceout 效果）。
    fn hide_text(&mut self);

    /// 显示当前活动层已隐藏的文本（用于 scein 效果）。
    fn show_text(&mut self);

    /// 查询当前活动层是否已完成逐字揭示。
    fn is_reveal_complete(&self) -> bool;

    /// 获取字体状态（只读）。
    fn font_state(&self) -> &FontState;

    /// 获取字体状态（可变）。
    fn font_state_mut(&mut self) -> &mut FontState;
}
