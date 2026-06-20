//! 文本渲染抽象。
//!
//! 后端实现 [`TextRenderer`] trait 来把解释器的文本事件翻译成绘制命令。

use crate::compositor::anim::Easing;
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
// 字形信息
// ---------------------------------------------------------------------------

/// 单一字形的度量与纹理信息。
#[derive(Debug, Clone)]
pub struct GlyphInfo {
    /// UTF-8 字符序列
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
    /// 字形本身的像素尺寸
    pub width: f32,
    pub height: f32,
    /// 该字形到下一个字形的步进距离
    pub advance_x: f32,
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
    /// 逐字显示：当前已揭示的字符数
    pub reveal_index: usize,
    /// 逐字显示：本层的新文本是否正在等待揭示
    pub reveal_pending: bool,
    /// 逐字显示配置（仅此层的 scetween）
    pub scetween: Option<ScetweenConfig>,
    /// 逐字显示内部时钟（毫秒），追踪自 reveal 开始以来的时间
    pub reveal_clock_ms: u64,
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
            reveal_index: 0,
            reveal_pending: false,
            scetween: None,
            reveal_clock_ms: 0,
        }
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
        matches!(self, Self::In | Self::Show | Self::BacklogDownIn | Self::BacklogUpIn)
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

#[derive(Debug, Default)]
pub struct FontState {
    pub layers: HashMap<String, MessageLayer>,
    pub active_layer: Option<String>,
    pub layer_stack: Vec<String>,
    pub default_font: FontDesc,
    pub ruby_enabled: bool,
    pub inside_ruby: bool,
    pub alignment: TextAlignment,
    pub glyph_config: HashMap<String, String>,
    pub custom: HashMap<String, String>,
    pub layers_dirtied_this_frame: Vec<String>,
    /// 逐字显示：全局 reveal 时钟（毫秒）
    pub reveal_clock_ms: u64,
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
            layers_dirtied_this_frame: Vec::new(),
            reveal_clock_ms: 0,
        }
    }

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
    /// 应用字体属性。
    fn apply_font_settings(&mut self, settings: &HashMap<String, String>);

    /// 重置当前字体为默认值。
    fn font_init(&mut self);

    /// 保存当前字体到栈。
    fn font_pop(&mut self);

    /// 设置默认字体。
    fn font_default(&mut self, settings: &HashMap<String, String>);

    /// 切换活动消息层。
    fn switch_message_layer(&mut self, id: Option<&str>);

    /// 弹出消息层。
    fn pop_message_layer(&mut self);

    /// 设置点击等待图标参数。
    fn set_glyph_config(&mut self, config: &HashMap<String, String>);

    /// 追加一段剧情文本。
    fn push_text(&mut self, content: &str, inline: bool);

    /// 文本换行。
    fn push_line_break(&mut self);

    /// 文本分页。
    fn push_page_break(&mut self, backlog: Option<i32>);

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
