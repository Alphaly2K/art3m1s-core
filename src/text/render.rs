//! 文本渲染抽象。
//!
//! 后端实现 [`TextRenderer`] trait 来把解释器的文本事件翻译成绘制命令。

use crate::compositor::renderer::{DrawCommand, TextureProvider};
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
    /// 字体文件路径（项目内相对路径，如 `"font/sourcehansans-medium.otf"`）
    pub face: Option<String>,
    /// 字号（像素）
    pub size: Option<f32>,
    /// 注音字号
    pub ruby_size: Option<f32>,
    /// 注音字体
    pub ruby_face: Option<String>,
    /// 字体颜色：`RRGGBB` 或 `AARRGGBB` 格式
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
    /// 原始样式字符串（如 "outline,shadow"），后端据此渲染描边/阴影
    pub style: Option<String>,
    /// 文本对齐：`"left"` / `"center"` / `"right"`
    pub align: Option<String>,
    /// 超出后是否截断或换行
    pub overflow: Option<String>,
    /// 是否竖排
    pub vertical: Option<bool>,
    /// 未被识别的属性，原样保留
    pub custom: HashMap<String, String>,
}

impl FontDesc {
    /// 从 `FontSettings` 事件的原始属性 map 解析。
    pub fn from_raw(raw: &HashMap<String, String>) -> Self {
        let mut desc = FontDesc::default();
        desc.merge_raw(raw);
        desc
    }

    /// 增量合并属性。
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
                "style" => {
                    self.style = Some(v.to_string());
                    for part in v.split(',') {
                        match part.trim() {
                            "bold" => self.bold = Some(true),
                            "italic" => self.italic = Some(true),
                            _ => {}
                        }
                    }
                }
                "align" => self.align = Some(v.to_string()),
                "overflow" => self.overflow = Some(v.to_string()),
                "vertical" => self.vertical = Some(matches!(v, "1" | "true")),
                _ => {
                    self.custom.insert(key.clone(), v.to_string());
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 字形信息
// ---------------------------------------------------------------------------

/// 单一字形的度量与纹理信息。
///
/// 文本渲染器为每个字符产出一条 `GlyphInfo`，宿主据此生成 `DrawCommand`。
#[derive(Debug, Clone)]
pub struct GlyphInfo {
    /// UTF-8 字符序列（可能是单个汉字或 ligature）
    pub character: String,
    /// 字形在 atlas 中的纹理 ID
    pub texture_id: crate::compositor::renderer::TextureId,
    /// 字形在 atlas 中的像素位置与尺寸
    pub atlas_x: f32,
    pub atlas_y: f32,
    pub atlas_w: f32,
    pub atlas_h: f32,
    /// 字形在文本行中的基线偏移（像素）
    pub offset_x: f32,
    pub offset_y: f32,
    /// 字形本身的像素尺寸（不包含间距）
    pub width: f32,
    pub height: f32,
    /// 该字形到下一个字形的步进距离（像素）
    pub advance_x: f32,
}

// ---------------------------------------------------------------------------
// 字体度量
// ---------------------------------------------------------------------------

/// 当前字体的度量信息。
#[derive(Debug, Clone, Default)]
pub struct FontMetrics {
    /// 行高
    pub line_height: f32,
    /// 基线
    pub baseline: f32,
    /// 上升高度
    pub ascent: f32,
    /// 下降高度
    pub descent: f32,
    /// 全角空格宽度
    pub em_width: f32,
}

// ---------------------------------------------------------------------------
// 消息层
// ---------------------------------------------------------------------------

/// 文本显示区域（消息层）的描述。
///
/// 对应 Artemis 的 chgmsg 体系：游戏可以定义多个消息层（adv、name、config 等），
/// 每个层有独立的位置、尺寸和字体状态。
#[derive(Debug, Clone)]
pub struct MessageLayer {
    /// 层名（如 `"adv01"`、`"fgname"`）
    pub id: String,
    /// 层在舞台上的左、上像素偏移
    pub left: f32,
    pub top: f32,
    /// 层的宽高（用于溢出判断与对齐）
    pub width: f32,
    pub height: f32,
    /// 该层的 z 序（数字越小越靠底）
    pub layer_index: i32,
    /// 该层是否可见
    pub visible: bool,
    /// 当前字体描述
    pub font: FontDesc,
    /// 字体栈（用于 font / font_close 的推入/弹出）
    pub font_stack: Vec<FontDesc>,
    /// 该层的文本缓存（按可见字符排列）
    pub text_buffer: Vec<GlyphInfo>,
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
        }
    }
}

// ---------------------------------------------------------------------------
// 字体状态
// ---------------------------------------------------------------------------

/// 字体渲染全局状态。
///
/// 维护所有消息层、当前活动层、默认字体设置及文本注入钩子。
#[derive(Debug, Default)]
pub struct FontState {
    /// 所有消息层
    pub layers: HashMap<String, MessageLayer>,
    /// 当前活动消息层 ID
    pub active_layer: Option<String>,
    /// 消息层栈（chgmsg / chgmsg_close）
    pub layer_stack: Vec<String>,
    /// 默认字体描述（由 `FontDefault` 事件设置）
    pub default_font: FontDesc,
    /// 是否启用注音
    pub ruby_enabled: bool,
    /// 当前是否在注音块中
    pub inside_ruby: bool,
    /// 文本对齐方式
    pub alignment: TextAlignment,
    /// 点击等待图标配置（glyph 事件）
    pub glyph_config: HashMap<String, String>,
    /// 未处理的自定义状态
    pub custom: HashMap<String, String>,
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
            custom: HashMap::new(),
        }
    }

    /// 获取当前活动层（不存在时自动创建）。
    pub fn active_layer_mut(&mut self) -> &mut MessageLayer {
        let id = self
            .active_layer
            .get_or_insert_with(|| "adv01".to_string())
            .clone();
        self.layers
            .entry(id.clone())
            .or_insert_with(|| MessageLayer::new(id.clone()));
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
}

impl From<&str> for TextAlignment {
    fn from(s: &str) -> Self {
        match s.trim() {
            "center" | "centre" => Self::Center,
            "right" => Self::Right,
            _ => Self::Left,
        }
    }
}

// ---------------------------------------------------------------------------
// TextRenderer trait
// ---------------------------------------------------------------------------

/// 字体渲染后端。
///
/// 实现方负责：
/// 1. 加载字体文件并构建字形 atlas（位图或 SDF）
/// 2. 根据当前字体状态对文本进行布局
/// 3. 产出可放入绘制列表的字形 `DrawCommand`
///
/// ## 基本用法
/// 宿主在帧循环中把解释器事件转发给 `TextRenderer`，然后调用
/// `build_draw_commands` 获取当前待绘制的字形。
pub trait TextRenderer {
    /// 应用来自 `FontSettings` 事件的字体属性。
    fn apply_font_settings(&mut self, settings: &HashMap<String, String>);

    /// 重置当前字体为默认值（对应 `FontInit`）。
    fn font_init(&mut self);

    /// 保存当前字体到字体栈（对应 `FontClose`）。
    fn font_pop(&mut self);

    /// 设置默认字体（对应 `FontDefault`）。
    fn font_default(&mut self, settings: &HashMap<String, String>);

    /// 切换活动消息层（对应 `MessageLayerSwitch`）。
    fn switch_message_layer(&mut self, id: Option<&str>);

    /// 弹出消息层（对应 `MessageLayerPop`）。
    fn pop_message_layer(&mut self);

    /// 设置点击等待图标的参数。
    fn set_glyph_config(&mut self, config: &HashMap<String, String>);

    /// 追加一段剧情文本到当前活动层（对应 `ScenarioText`）。
    ///
    /// 内部会经过注入链（见 [`crate::text::TextInject`]），
    /// 然后根据当前字体状态进行字形布局。
    fn push_text(&mut self, content: &str, inline: bool);

    /// 文本换行（对应 `LineBreak`）。
    fn push_line_break(&mut self);

    /// 文本分页（对应 `PageBreak`）。
    fn push_page_break(&mut self, backlog: Option<i32>);

    /// 获取当前活动消息层的可见字形列表。
    ///
    /// `provider` 用于上传字形 atlas 纹理。返回的 `DrawCommand` 可直接追加到帧的 `DrawList` 中。
    fn build_draw_commands(
        &mut self,
        provider: &mut dyn TextureProvider,
    ) -> Vec<DrawCommand>;

    /// 获取当前字体状态（只读）。
    fn font_state(&self) -> &FontState;

    /// 获取当前字体状态（可变）。
    fn font_state_mut(&mut self) -> &mut FontState;
}
