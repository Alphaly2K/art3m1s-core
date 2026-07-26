//! 基于 ab_glyph 的字形光栅化文本渲染器。

use crate::render_pipeline::draw::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, TextureId, TextureInfo, TextureProvider,
};
use crate::text::backlog::{BacklogPage, BacklogTag};
use crate::text::render::{
    ClickWaitIconPlacement, FontState, GlyphIconConfig, GlyphInfo, LinkHitArea, LinkRange,
    RubyRange, ScetweenConfig, TextLayoutConfig, TextRenderer,
};
use ab_glyph::{Font, FontRef, PxScale, PxScaleFont, ScaleFont};
use glam::{Affine2, Vec2};
use std::collections::HashMap;

const ATLAS_SZ: u32 = 1024;
/// 文本 atlas 的保留纹理名。runtime 的纹理保活名单也引用它，防止被 retain 驱逐。
pub const ATLAS_NAME: &str = ":text/atlas";
/// Artemis 消息层的缺省 id（脚本未显式切层时使用）。
pub(crate) const DEFAULT_MESSAGE_LAYER: &str = "adv01";
/// 未提供字号时的缺省字号（Artemis 默认值）。
const DEFAULT_FONT_SIZE: f32 = 40.0;
const OUTLINE_OFFSETS: [(f32, f32); 4] = [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)];

struct Atlas {
    rows: Vec<(u32, u32)>,
    cur: Vec<u32>,
    px: Vec<u8>,
    dirty: bool,
}
impl Atlas {
    fn new() -> Self {
        Self {
            rows: vec![],
            cur: vec![],
            px: vec![0; (ATLAS_SZ * ATLAS_SZ * 4) as usize],
            dirty: false,
        }
    }
    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        for (i, &(oy, oh)) in self.rows.iter().enumerate() {
            if oh >= h && self.cur[i] + w <= ATLAS_SZ {
                let x = self.cur[i];
                self.cur[i] += w;
                return Some((x, oy));
            }
        }
        let y: u32 = self.rows.last().map(|(y, h)| y + h).unwrap_or(0);
        if y + h > ATLAS_SZ {
            return None;
        }
        self.rows.push((y, h));
        self.cur.push(w);
        Some((0, y))
    }
    fn write(&mut self, x: u32, y: u32, w: u32, h: u32, rgba: &[u8]) {
        self.dirty = true;
        for r in 0..h as usize {
            let doff = ((y as usize + r) * ATLAS_SZ as usize + x as usize) * 4;
            let soff = r * w as usize * 4;
            let len = (w as usize * 4)
                .min(rgba.len() - soff)
                .min(self.px.len() - doff);
            self.px[doff..doff + len].copy_from_slice(&rgba[soff..soff + len]);
        }
    }
    fn flush(&mut self, p: &mut dyn TextureProvider) -> Option<(TextureId, TextureInfo)> {
        if self.dirty {
            if let Some(r) = p.upload_rgba(ATLAS_NAME, ATLAS_SZ, ATLAS_SZ, &self.px) {
                self.dirty = false;
                return Some(r);
            }
        }
        p.resolve(ATLAS_NAME)
    }
}

pub struct GlyphTextRenderer<'font> {
    state: FontState,
    font: Option<FontRef<'font>>,
    atlas: Atlas,
    cache: HashMap<(u16, u32), (u32, u32, u32, u32)>,
    /// atlas 里的纯白小块（link type=0 hover 强调的白色方形板用），惰性分配
    white_patch: Option<(u32, u32)>,
}

fn scaled<'a>(
    font: &'a Option<FontRef<'a>>,
    scale: PxScale,
) -> Option<PxScaleFont<&'a FontRef<'a>>> {
    font.as_ref().map(|f| f.as_scaled(scale))
}

fn parse(s: &str) -> [f32; 3] {
    let h = s.trim().trim_start_matches("0x").trim_start_matches('#');
    if h.len() >= 6 {
        [
            u8::from_str_radix(&h[0..2], 16).unwrap_or(255) as f32 / 255.0,
            u8::from_str_radix(&h[2..4], 16).unwrap_or(255) as f32 / 255.0,
            u8::from_str_radix(&h[4..6], 16).unwrap_or(255) as f32 / 255.0,
        ]
    } else {
        [1.0; 3]
    }
}

/// 单个字形的排版结果：行内 X 坐标与所在行号。
///
/// `x` 不含消息层 left 与字形自身 offset_x；`line` 从 0 起，
/// 行的 Y 坐标由调用方乘以行高得到。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct LaidGlyph {
    pub x: f32,
    pub line: usize,
}

/// 取字形的首个字符（GlyphInfo.character 是 UTF-8 字符串，正常只含一个字符）。
fn first_char(g: &GlyphInfo) -> Option<char> {
    g.character.chars().next()
}

/// 纯排版函数：对字形序列做逐字符排版，返回每个字形的行内位置。
///
/// 实现 Artemis 的三大排版机制：
/// - 禁则处理（prohibit）：行首禁则字符"上提"悬挂在上一行行尾（允许超宽）；
///   行尾禁则字符"下移"，连同后续字符一起换到下一行；
/// - wordparts：连续的单词组成字符视为不可分割单元，换行点回退到单词边界，
///   整行都是一个单词时才强制拦腰截断；
/// - indent：识别缩进开始/结束字符（pair 两两一组），缩进期间新起的行
///   （自动换行与显式换行皆是）从缩进 X 开始排。
///
/// `\n` 字形显式换行；宽度判定沿用 `x + width > line_width` 的字形包围盒逻辑。
///
/// `keep_ranges`：不可拆行的字符区间（`[start, end)`，如注音区间——
/// 带注音的文本中间不自动换行）。断点落在区间内部时整个区间回退到
/// 下一行；区间从行首开始（整行都装不下）时才强制拦腰截断。
pub(crate) fn layout_glyphs(
    glyphs: &[GlyphInfo],
    line_width: f32,
    cfg: &TextLayoutConfig,
    keep_ranges: &[(usize, usize)],
) -> Vec<LaidGlyph> {
    let mut out = vec![LaidGlyph::default(); glyphs.len()];
    let mut line = 0usize;
    let mut line_start = 0usize;
    // 当前生效的缩进 X；新行（含显式换行）从这里起排
    let mut indent_x = 0.0f32;
    // 缩进栈：(期待的结束字符, 进入该级缩进前的缩进 X)
    let mut indent_stack: Vec<(char, f32)> = Vec::new();

    while line_start < glyphs.len() {
        // ── 第一阶段：扫描本行的断点（本行含 line_start..break_at） ──
        let mut cx = indent_x;
        let mut break_at = glyphs.len();
        let mut i = line_start;
        while i < glyphs.len() {
            let g = &glyphs[i];
            if g.character == "\n" {
                // 显式换行：换行字形本身归属当前行
                break_at = i + 1;
                break;
            }
            if cx + g.width > line_width && i > line_start {
                let ch = first_char(g);
                // 行首禁则：该字符禁止出现在行首 → 上提，留在当前行（允许超宽），
                // 连续多个禁则字符会依次悬挂
                if ch.is_some_and(|c| cfg.is_prohibit_head(c)) {
                    cx += g.advance_x;
                    i += 1;
                    continue;
                }
                let mut b = i;
                // keep_ranges（注音区间等）：断点落在区间内部 → 整个区间
                // 回退到下一行；区间从行首开始（整行装不下）时强制截断
                if let Some(&(rs, _)) = keep_ranges
                    .iter()
                    .find(|&&(rs, re)| rs < b && b < re)
                    && rs > line_start
                {
                    b = rs;
                }
                // wordparts：不在单词中间断行 → 回退到单词边界；
                // 整行都是一个单词（回退到行首）时保持原断点强制截断
                if b == i && ch.is_some_and(|c| cfg.is_wordpart(c)) {
                    let mut j = i;
                    while j > line_start
                        && first_char(&glyphs[j - 1]).is_some_and(|c| cfg.is_wordpart(c))
                    {
                        j -= 1;
                    }
                    if j > line_start {
                        b = j;
                    }
                }
                // 行尾禁则：行尾不能是「『（等开括号 → 连同下移到下一行；
                // 至少给当前行留一个字符，避免空行死循环
                while b > line_start + 1
                    && first_char(&glyphs[b - 1]).is_some_and(|c| cfg.is_prohibit_foot(c))
                {
                    b -= 1;
                }
                break_at = b;
                break;
            }
            cx += g.advance_x;
            i += 1;
        }

        // ── 第二阶段：放置 line_start..break_at，并做缩进识别 ──
        let mut cx = indent_x;
        let mut chars_in_line = 0usize;
        for k in line_start..break_at {
            let g = &glyphs[k];
            out[k] = LaidGlyph { x: cx, line };
            if g.character == "\n" {
                continue;
            }
            if let Some(c) = first_char(g) {
                if let Some(close) = cfg.indent_close_for(c) {
                    // 缩进开始字符：range 限制行首前 N 个字符内才识别；
                    // nest=0 时已缩进则忽略后续开始字符
                    let within_range = cfg.indent_range.is_none_or(|r| chars_in_line < r);
                    if within_range && (indent_stack.is_empty() || cfg.indent_nest) {
                        indent_stack.push((close, indent_x));
                        // 缩进量 = 开始字符右端的 X（"留出一个「的空间"），
                        // 从下一行起生效
                        indent_x = cx + g.advance_x;
                    }
                } else if let Some(&(close, prev)) = indent_stack.last()
                    && c == close
                {
                    // 缩进结束字符：恢复上一级缩进
                    indent_x = prev;
                    indent_stack.pop();
                }
            }
            cx += g.advance_x;
            chars_in_line += 1;
        }

        line += 1;
        line_start = break_at;
    }
    out
}

impl<'font> GlyphTextRenderer<'font> {
    pub fn new() -> Self {
        Self {
            state: FontState::new(),
            font: None,
            atlas: Atlas::new(),
            cache: HashMap::new(),
            white_patch: None,
        }
    }
    pub fn set_font(&mut self, bytes: &'font [u8]) -> Result<(), String> {
        self.font = Some(FontRef::try_from_slice(bytes).map_err(|e| format!("{e}"))?);
        self.cache.clear();
        Ok(())
    }

    /// 光栅化单个字符（按字号 `sz`），写入 atlas 并返回字形信息。
    ///
    /// 正文与注音共用该路径（注音按 rubysize 光栅化）。
    /// 未加载字体或字体不含该字形时返回 None。
    fn rasterize_glyph(&mut self, c: char, sz: f32) -> Option<GlyphInfo> {
        let sf = scaled(&self.font, PxScale::from(sz))?;
        let q = sf.outline_glyph(sf.glyph_id(c).with_scale(sz))?;
        let b = q.px_bounds();
        let w = b.width().ceil() as u32;
        let h = b.height().ceil() as u32;
        let (ax, ay, aw, ah) = if w > 0 && h > 0 && w < ATLAS_SZ && h < ATLAS_SZ {
            let k = (sf.glyph_id(c).0, sz as u32);
            *self.cache.entry(k).or_insert_with(|| {
                if let Some((x, y)) = self.atlas.alloc(w + 1, h + 1) {
                    let mut g = vec![0u8; (w * h) as usize];
                    q.draw(|px, py, v| {
                        let ix = py as usize * w as usize + px as usize;
                        if ix < g.len() {
                            g[ix] = (v * 255.0) as u8;
                        }
                    });
                    let rgba: Vec<u8> = g.iter().flat_map(|&a| [255u8, 255, 255, a]).collect();
                    self.atlas.write(x, y, w, h, &rgba);
                    (x, y, w, h)
                } else {
                    // atlas 满：该字形本次以零尺寸落缓存（不再重试）。
                    crate::core_warn!("字形 atlas 已满，字符 {c:?} 无法光栅化");
                    (0, 0, 0, 0)
                }
            })
        } else {
            (0, 0, 0, 0)
        };
        Some(GlyphInfo {
            character: c.to_string(),
            texture_id: TextureId(0),
            atlas_x: ax as f32,
            atlas_y: ay as f32,
            atlas_w: aw as f32,
            atlas_h: ah as f32,
            offset_x: b.min.x,
            offset_y: sf.ascent() + b.min.y,
            width: w as f32,
            height: h as f32,
            advance_x: sf.h_advance(sf.glyph_id(c).with_scale(sz).id),
        })
    }

    /// 确保 atlas 里有一块纯白像素（返回其中心坐标），
    /// 供 link type=0 的白色方形板加法合成强调使用。
    fn ensure_white_patch(&mut self) -> Option<(u32, u32)> {
        if let Some(p) = self.white_patch {
            return Some(p);
        }
        let (x, y) = self.atlas.alloc(4, 4)?;
        self.atlas.write(x, y, 4, 4, &[255u8; 4 * 4 * 4]);
        // 取内缩 1px 的中心点，避免采样到邻近字形
        let p = (x + 1, y + 1);
        self.white_patch = Some(p);
        Some(p)
    }
}

impl TextRenderer for GlyphTextRenderer<'_> {
    fn set_font_bytes(&mut self, bytes: &'static [u8]) -> Result<(), String> {
        self.set_font(bytes)
    }

    fn active_font_face(&self) -> Option<&str> {
        let id = self.state.active_layer.as_deref().unwrap_or(DEFAULT_MESSAGE_LAYER);
        self.state
            .layers
            .get(id)
            .and_then(|layer| layer.font.face.as_deref())
            .or(self.state.default_font.face.as_deref())
    }

    fn apply_font_settings(&mut self, s: &HashMap<String, String>) {
        let l = self.state.active_layer_mut();
        // 按 Artemis 约定，stack 参数默认为 1（true）：应用新样式前先把当前样式压栈，
        // 之后 [font_close] 可逐层恢复。
        let stacked = s
            .get("stack")
            .map(|v| matches!(v.as_str(), "1" | "true"))
            .unwrap_or(true);
        if stacked {
            l.font_stack.push(l.font.clone());
        }
        l.font.merge_raw(s);
        // 再现记录：font 标签参数原样入本页标签序列（get_message_tags/backlog 用）
        l.page_tags.push(BacklogTag::Font(s.clone()));
        if let Some(v) = s.get("left").and_then(|v| v.parse().ok()) {
            l.left = v;
        }
        if let Some(v) = s.get("top").and_then(|v| v.parse().ok()) {
            l.top = v;
        }
        if let Some(v) = s.get("width").and_then(|v| v.parse().ok()) {
            l.width = v;
        }
        if let Some(v) = s.get("height").and_then(|v| v.parse().ok()) {
            l.height = v;
        }
    }
    fn font_init(&mut self) {
        let d = self.state.default_font.clone();
        let l = self.state.active_layer_mut();
        l.font = d;
        // 文档：fontinit 除恢复默认字体外，字体堆栈也将被初始化（清空），
        // 否则之后的 [/font] 仍能弹出旧设置。
        l.font_stack.clear();
        // 再现记录：以恢复后的字体快照落一条 font 标签，重放时得到同样状态
        let snapshot = l.font.to_raw();
        l.page_tags.push(BacklogTag::Font(snapshot));
    }
    fn font_pop(&mut self) {
        let l = self.state.active_layer_mut();
        if let Some(v) = l.font_stack.pop() {
            l.font = v;
            // 再现记录：font_close 无法直接重放（重放流不建栈），
            // 记录弹栈后的字体快照等效重现
            let snapshot = l.font.to_raw();
            l.page_tags.push(BacklogTag::Font(snapshot));
        }
    }
    fn font_default(&mut self, s: &HashMap<String, String>) {
        self.state.default_font.merge_raw(s);
    }
    fn switch_message_layer(&mut self, id: Option<&str>, stack: bool) {
        let prev_state = self.state.active_layer.as_ref().and_then(|aid| {
            self.state
                .layers
                .get(aid)
                .map(|l| (l.left, l.top, l.width, l.height, l.font.clone()))
        });
        // stack=0（chgmsg）不压栈，避免存档中消息层堆栈膨胀。
        if stack
            && let Some(ref prev_id) = self.state.active_layer
        {
            self.state.layer_stack.push(prev_id.clone());
        }
        self.state.active_layer = id.map(|s| s.to_string());
        let layer = self.state.active_layer_mut();
        if let Some((left, top, width, height, font)) = prev_state {
            if layer.left == 0.0 && layer.top == 0.0 {
                layer.left = left;
                layer.top = top;
                layer.width = width;
                layer.height = height;
                layer.font = font;
            }
        }
        // 清缓冲的同时清 link/ruby 区间与页内再现标签（区间指向旧缓冲）
        layer.clear_page();
        layer.reveal_index = 0;
        layer.reveal_pending = false;
        layer.reveal_clock_ms = 0; // 切层时也要清时钟，避免旧动画时间残留
    }

    fn pop_message_layer(&mut self) {
        if let Some(prev) = self.state.layer_stack.pop() {
            self.state.active_layer = Some(prev);
        }
    }
    fn set_glyph_config(&mut self, c: &HashMap<String, String>) {
        self.state.glyph_config.clone_from(c);
        // 同步解析成结构化配置，供点击等待图标摆放查询使用
        self.state.glyph_icon = GlyphIconConfig::from_raw(c);
    }

    fn click_wait_icon_placement(&self, page_end: bool) -> Option<ClickWaitIconPlacement> {
        let cfg = &self.state.glyph_icon;
        // 行末图标只看 layer；页末图标优先 rplayer，缺省回退到 layer。
        // 两者都缺省则禁用点击等待图标。
        let layer_id = if page_end {
            cfg.rplayer.clone().or_else(|| cfg.layer.clone())
        } else {
            cfg.layer.clone()
        }?;
        let (dx, dy) = if page_end {
            (cfg.rpleft, cfg.rptop)
        } else {
            (cfg.left, cfg.top)
        };

        let lid = self
            .state
            .active_layer
            .as_deref()
            .unwrap_or(DEFAULT_MESSAGE_LAYER);
        let ly = self.state.layers.get(lid)?;
        // 最后一个非换行字形：图标摆在它的右端
        let (idx, last) = ly
            .text_buffer
            .iter()
            .enumerate()
            .rev()
            .find(|(_, g)| g.character != "\n")?;

        let sz = ly.font.size.unwrap_or(DEFAULT_FONT_SIZE);
        let line_height = scaled(&self.font, PxScale::from(sz))
            .map(|sf| sf.height())
            .unwrap_or(sz);
        let lw = if ly.width > 0.0 { ly.width } else { f32::MAX };
        // 与 build_text_commands 走同一套排版，保证图标位置与实际换行一致
        let laid = layout_glyphs(&ly.text_buffer, lw, &self.state.layout, &ly.keep_ranges());
        let pos = laid[idx];
        Some(ClickWaitIconPlacement {
            layer_id,
            left: ly.left + pos.x + last.advance_x + dx,
            top: ly.top + pos.line as f32 * line_height + dy,
            homing: cfg.homing,
        })
    }

    fn push_text(&mut self, content: &str, _inline: bool) {
        // 再现记录先于光栅化：即使字体尚未加载，本页的逻辑文本流
        // （get_message_tags / backlog 入库）也必须完整。
        self.state
            .active_layer_mut()
            .page_tags
            .push(BacklogTag::Text(content.to_string()));

        let sz = {
            let layer = self.state.active_layer_mut();
            layer.font.size.unwrap_or(DEFAULT_FONT_SIZE)
        };
        let glyphs: Vec<GlyphInfo> = content
            .chars()
            .filter_map(|c| self.rasterize_glyph(c, sz))
            .collect();
        if glyphs.is_empty() {
            return;
        }

        let layer = self.state.active_layer_mut();
        let was_empty = layer.text_buffer.is_empty();
        if was_empty {
            layer.reveal_pending = true;
            layer.reveal_clock_ms = 0;
            layer.reveal_index = 0;
        }
        layer.text_buffer.extend(glyphs);
    }

    fn push_line_break(&mut self) {
        // rt 标签 omitblankline：若最后一行为空行（缓冲为空或上一个字形已是换行）
        // 则跳过本次换行，防止意外空行。解释器尚未透传该参数，先内置默认行为 1。
        let omit = self.state.layout.rt_omit_blank_line;
        let layer = self.state.active_layer_mut();
        if omit && layer.text_buffer.last().is_none_or(|g| g.character == "\n") {
            return;
        }
        let sz = layer.font.size.unwrap_or(DEFAULT_FONT_SIZE);
        let scale = PxScale::from(sz);
        // 未加载字体时以字号近似行高，换行字形不依赖光栅化
        let line_height = scaled(&self.font, scale).map(|sf| sf.height()).unwrap_or(sz);
        // 再现记录：只记实际生效的换行（被 omitblankline 省略的重放时同样省略）
        layer.page_tags.push(BacklogTag::LineBreak);
        layer.text_buffer.push(GlyphInfo {
            character: "\n".into(),
            texture_id: TextureId(0),
            atlas_x: 0.0,
            atlas_y: 0.0,
            atlas_w: 0.0,
            atlas_h: 0.0,
            offset_x: 0.0,
            offset_y: line_height,
            width: 0.0,
            height: 0.0,
            advance_x: 0.0,
        });
    }

    fn push_page_break(&mut self, bl: Option<i32>) {
        let lid = self.state.active_layer.clone().unwrap_or_default();
        if let Some(l) = self.state.layers.get_mut(&lid) {
            // rp 的 backlog 参数：1 入库 / 0 不入库（均无视 writebacklog），
            // 缺省按 writebacklog 的 mode。入库判定与页装配在 Backlog 内完成
            // （allow=0、无文本、includefont 过滤、页数上限都在 push_page 里）。
            if self.state.backlog.should_store(bl) {
                let page = BacklogPage {
                    page_font: Some(l.page_font.clone()),
                    tags: std::mem::take(&mut l.page_tags),
                };
                self.state.backlog.push_page(page);
            }
            // 换页清缓冲，同时清 link/ruby 区间与页内标签，
            // 并以当前字体重置页首快照
            l.clear_page();
            l.reveal_index = 0;
            l.reveal_pending = false;
            l.reveal_clock_ms = 0;
        }
    }

    // ── ruby / link ──

    fn ruby_start(&mut self, text: &str) {
        let l = self.state.active_layer_mut();
        l.open_ruby = Some((l.text_buffer.len(), text.to_string()));
        l.page_tags.push(BacklogTag::RubyStart(text.to_string()));
    }

    fn ruby_end(&mut self) {
        let (start, text, ruby_sz) = {
            let l = self.state.active_layer_mut();
            let Some((start, text)) = l.open_ruby.take() else {
                return;
            };
            l.page_tags.push(BacklogTag::RubyEnd);
            // rubysize 缺省：文档未给缺省值，按 Artemis 惯例取正文字号的一半
            let base_sz = l.font.size.unwrap_or(DEFAULT_FONT_SIZE);
            (start, text, l.font.ruby_size.unwrap_or(base_sz * 0.5))
        };
        // 注音字形按 rubysize 光栅化（与正文共用 atlas）
        let glyphs: Vec<GlyphInfo> = text
            .chars()
            .filter_map(|c| self.rasterize_glyph(c, ruby_sz))
            .collect();
        let l = self.state.active_layer_mut();
        let end = l.text_buffer.len();
        if end > start {
            l.rubies.push(RubyRange {
                start,
                end,
                text,
                size: ruby_sz,
                glyphs,
            });
        }
    }

    fn link_start(
        &mut self,
        file: Option<&str>,
        label: Option<&str>,
        link_type: i32,
        color: Option<&str>,
        shadow_color: Option<&str>,
        outline_color: Option<&str>,
    ) {
        let l = self.state.active_layer_mut();
        let start = l.text_buffer.len();
        // 链接本身不进再现标签序列（历史页里的链接不需要可点击），
        // 链接内的正文照常由 push_text 记录。
        l.links.push(LinkRange {
            start,
            end: None,
            file: file.map(str::to_string),
            label: label.map(str::to_string),
            link_type,
            color: color.map(str::to_string),
            shadow_color: shadow_color.map(str::to_string),
            outline_color: outline_color.map(str::to_string),
            hovered: false,
        });
    }

    fn link_end(&mut self) {
        let l = self.state.active_layer_mut();
        let len = l.text_buffer.len();
        if let Some(link) = l.links.iter_mut().rev().find(|k| k.end.is_none()) {
            link.end = Some(len);
        }
    }

    fn set_links_enabled(&mut self, enabled: bool) {
        self.state.links_enabled = enabled;
        if !enabled {
            // linkdisable：不可点击也不强调 → 清掉所有 hover 状态
            for layer in self.state.layers.values_mut() {
                for link in &mut layer.links {
                    link.hovered = false;
                }
            }
        }
    }

    fn link_hit_areas(&self) -> Vec<LinkHitArea> {
        if !self.state.links_enabled {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (lid, ly) in &self.state.layers {
            if ly.links.is_empty() || ly.text_buffer.is_empty() {
                continue;
            }
            let sz = ly.font.size.unwrap_or(DEFAULT_FONT_SIZE);
            // 未加载字体时以字号近似行高（与 push_line_break 的近似一致）
            let lh = scaled(&self.font, PxScale::from(sz))
                .map(|sf| sf.height())
                .unwrap_or(sz);
            let lw = if ly.width > 0.0 { ly.width } else { f32::MAX };
            let laid = layout_glyphs(&ly.text_buffer, lw, &self.state.layout, &ly.keep_ranges());
            for (idx, link) in ly.links.iter().enumerate() {
                let end = link.end_or(ly.text_buffer.len());
                for (x, y, w, h) in link_line_rects(&ly.text_buffer, &laid, link.start, end, lh) {
                    out.push(LinkHitArea {
                        layer_id: lid.clone(),
                        link_index: idx,
                        left: ly.left + x,
                        top: ly.top + y,
                        width: w,
                        height: h,
                        file: link.file.clone(),
                        label: link.label.clone(),
                    });
                }
            }
        }
        out
    }

    fn update_link_hover(&mut self, x: f32, y: f32) -> bool {
        let areas = self.link_hit_areas(); // 禁用时为空 → hover 全灭
        let mut changed = false;
        for (lid, ly) in self.state.layers.iter_mut() {
            for (idx, link) in ly.links.iter_mut().enumerate() {
                let hovered = areas
                    .iter()
                    .any(|a| a.layer_id == *lid && a.link_index == idx && a.contains(x, y));
                if link.hovered != hovered {
                    link.hovered = hovered;
                    changed = true;
                }
            }
        }
        changed
    }

    fn active_layer_text_metrics(&self) -> Option<(f32, f32, f32)> {
        let id = self.state.active_layer.as_deref()?;
        let ly = self.state.layers.get(id)?;
        if ly.text_buffer.is_empty() {
            return Some((0.0, 0.0, 0.0));
        }
        let sz = ly.font.size.unwrap_or(DEFAULT_FONT_SIZE);
        let lh = scaled(&self.font, PxScale::from(sz))
            .map(|sf| sf.height())
            .unwrap_or(sz);
        let lw = if ly.width > 0.0 { ly.width } else { f32::MAX };
        let laid = layout_glyphs(&ly.text_buffer, lw, &self.state.layout, &ly.keep_ranges());

        let mut overall_width = 0.0f32;
        let mut last_line = 0usize;
        let mut last_line_width = 0.0f32;
        for (i, pos) in laid.iter().enumerate() {
            // 字形右边缘（换行符 advance 为 0，不影响宽度）。
            let right = pos.x + ly.text_buffer[i].advance_x;
            overall_width = overall_width.max(right);
            if pos.line > last_line {
                last_line = pos.line;
                last_line_width = 0.0;
            }
            if pos.line == last_line {
                last_line_width = last_line_width.max(right);
            }
        }
        let total_height = (last_line as f32 + 1.0) * lh;
        Some((overall_width, total_height, last_line_width))
    }

    fn build_text_commands(
        &mut self,
        p: &mut dyn TextureProvider,
    ) -> HashMap<String, Vec<DrawCommand>> {
        // link type=0 hover 需要白色方形板：必须在 flush 之前写入 atlas，
        // 否则当帧分配的白块要到下一帧才被上传。
        let links_enabled = self.state.links_enabled;
        let any_type0_hover = links_enabled
            && self
                .state
                .layers
                .values()
                .any(|l| l.links.iter().any(|k| k.hovered && k.link_type == 0));
        if any_type0_hover {
            self.ensure_white_patch();
        }
        let Some((tex, _)) = self.atlas.flush(p) else {
            crate::core_warn!("文本 atlas 纹理不可用，本帧文本不绘制");
            return HashMap::new();
        };
        let mut out: HashMap<String, Vec<DrawCommand>> = HashMap::new();

        let lids: Vec<String> = self.state.layers.keys().cloned().collect();
        for lid in &lids {
            let ly = match self.state.layers.get(lid) {
                Some(l) => l.clone(),
                None => continue,
            };
            if ly.text_buffer.is_empty() {
                continue;
            }

            // fixed_count: 无 scetween 全量; 有 scetween 按 reveal_index
            let visible_count = if !ly.scetween.is_empty() {
                ly.reveal_index.min(ly.text_buffer.len())
            } else {
                ly.text_buffer.len()
            };
            let scethweens = &ly.scetween;
            let text_hidden = ly.text_hidden;

            let sz = ly.font.size.unwrap_or(DEFAULT_FONT_SIZE);
            let scale = PxScale::from(sz);
            let sf = scaled(&self.font, scale);
            let sf = match sf {
                Some(s) => s,
                None => continue,
            };
            let lh = sf.height();
            let lw = if ly.width > 0.0 { ly.width } else { f32::MAX };

            let color = ly.font.color.as_deref().map(parse).unwrap_or([1.0; 3]);
            let oc = ly
                .font
                .outline_color
                .as_deref()
                .map(parse)
                .unwrap_or([0.0, 0.0, 0.0]);
            let st = ly.font.style.as_deref().unwrap_or("");
            let has_outline = st.contains("outline");
            let has_shadow = st.contains("shadow");

            // 统一走排版函数：禁则 / wordparts / 缩进 / 注音不可拆行都在这里生效
            let laid = layout_glyphs(&ly.text_buffer, lw, &self.state.layout, &ly.keep_ranges());
            // randomdelay：字符按随机顺序揭示。取相关配置里的随机顺序表，
            // 字符 i 可见当且仅当它的随机槽位 < 已揭示数量
            let random_order = scethweens
                .iter()
                .filter(|c| c.mode.is_entrance() != text_hidden)
                .find_map(|c| c.random_order.as_ref());

            let mut v = Vec::new();
            for (i, g) in ly.text_buffer.iter().enumerate() {
                let reveal_slot = random_order
                    .and_then(|order| order.get(i).copied())
                    .unwrap_or(i);
                if reveal_slot >= visible_count {
                    // 尚未揭示的字符：跳过
                    continue;
                }

                if g.character == "\n" {
                    continue;
                }

                let fx = ly.left + laid[i].x + g.offset_x;
                let fy = ly.top + g.offset_y + laid[i].line as f32 * lh;

                // 计算每字符的 scetween 动画偏移
                let anim_offset =
                    scetween_char_offset(scethweens, i, ly.reveal_clock_ms, text_hidden);

                // link type=1 hover 强调：更改该区间文字/阴影/轮廓颜色
                // （各颜色缺省 0x000000，见 link 标签文档）
                let mut g_color = color;
                let mut g_shadow_c = oc;
                let mut g_outline_c = oc;
                if links_enabled
                    && let Some(link) = ly.links.iter().find(|k| {
                        k.hovered && k.link_type == 1 && k.contains_index(i, ly.text_buffer.len())
                    })
                {
                    let black = [0.0, 0.0, 0.0];
                    g_color = link.color.as_deref().map(parse).unwrap_or(black);
                    g_shadow_c = link.shadow_color.as_deref().map(parse).unwrap_or(black);
                    g_outline_c = link.outline_color.as_deref().map(parse).unwrap_or(black);
                }

                if g.atlas_w > 0.0 && g.atlas_h > 0.0 {
                    let clip = ClipRect {
                        uv_offset: [g.atlas_x / ATLAS_SZ as f32, g.atlas_y / ATLAS_SZ as f32],
                        uv_scale: [g.atlas_w / ATLAS_SZ as f32, g.atlas_h / ATLAS_SZ as f32],
                        quad_size: [g.atlas_w, g.atlas_h],
                    };

                    // 带 scetween 动画偏移的位置
                    let pos_x = fx + anim_offset.0;
                    let pos_y = fy + anim_offset.1;
                    // 每字符缩放
                    let char_scale_x = anim_offset.2;
                    let char_scale_y = anim_offset.3;
                    let char_rotate = anim_offset.4;
                    let char_alpha = anim_offset.5;

                    let mut base_transform = Affine2::from_translation(Vec2::new(pos_x, pos_y));

                    // 如果每字符有缩放或旋转，围绕字形中心变换
                    if (char_scale_x - 1.0).abs() > 1e-6
                        || (char_scale_y - 1.0).abs() > 1e-6
                        || char_rotate.abs() > 1e-6
                    {
                        let cx_center = g.width * 0.5;
                        let cy_center = g.height * 0.5;
                        let to_center = Affine2::from_translation(Vec2::new(cx_center, cy_center));
                        let from_center =
                            Affine2::from_translation(Vec2::new(-cx_center, -cy_center));
                        let rot = Affine2::from_angle(char_rotate.to_radians());
                        let scl = Affine2::from_scale(Vec2::new(char_scale_x, char_scale_y));
                        base_transform = Affine2::from_translation(Vec2::new(pos_x, pos_y))
                            * to_center
                            * rot
                            * scl
                            * from_center;
                    }

                    let base = DrawCommand {
                        texture: tex,
                        size: TextureInfo {
                            width: ATLAS_SZ,
                            height: ATLAS_SZ,
                        },
                        transform: base_transform,
                        opacity: char_alpha,
                        blend: BlendMode::Alpha,
                        color: ColorFilter {
                            multiply: g_color,
                            grayscale: false,
                            negative: false,
                        },
                        clip: clip.clone(),
                        clip_bounds: None,
                        shader: None,
                        mesh: None,
                        stencil: None,
                    };
                    if has_shadow {
                        let mut sc = base.clone();
                        let sd = ly.font.shadow_size.unwrap_or(2.0);
                        sc.color.multiply = g_shadow_c;
                        sc.transform = Affine2::from_translation(Vec2::new(pos_x + sd, pos_y + sd));
                        v.push(sc);
                    }
                    if has_outline {
                        let os = ly.font.outline_size.unwrap_or(1.0);
                        for &(ox, oy) in &OUTLINE_OFFSETS {
                            let mut ocp = base.clone();
                            ocp.color.multiply = g_outline_c;
                            ocp.transform = Affine2::from_translation(Vec2::new(
                                pos_x + ox * os,
                                pos_y + oy * os,
                            ));
                            v.push(ocp);
                        }
                    }
                    v.push(base);
                }
            }

            // ── 注音（ruby）绘制：居中排在正文区间上方 ──
            for r in &ly.rubies {
                if r.glyphs.is_empty() || r.end > laid.len() || r.start >= r.end {
                    continue;
                }
                // 揭示门槛：正文区间首字符揭示后注音整体出现
                let first_slot = random_order
                    .and_then(|order| order.get(r.start).copied())
                    .unwrap_or(r.start);
                if first_slot >= visible_count {
                    continue;
                }
                // 注音区间不可拆行（keep_ranges），因此整段在同一行
                let line = laid[r.start].line;
                let base_x0 = laid[r.start].x;
                let last = r.end - 1;
                let base_x1 = laid[last].x + ly.text_buffer[last].advance_x;
                let advances: Vec<f32> = r.glyphs.iter().map(|g| g.advance_x).collect();
                let rk = ly.font.ruby_kerning.unwrap_or(0.0);
                let xs = ruby_positions(base_x0, base_x1, &advances, rk);
                // 垂直位置：注音底部贴正文行顶再按 spacetop（行顶到注音）与
                // spacemiddle（注音到正文）微调。当前行高固定为正文行高，
                // 注音画在行框上沿之上（Artemis 的行内预留空间尚未实现）。
                let spacetop = ly.font.spacetop.unwrap_or(0.0);
                let spacemiddle = ly.font.spacemiddle.unwrap_or(0.0);
                let ruby_top = ly.top + line as f32 * lh - (r.size + spacemiddle) + spacetop;
                for (g, gx) in r.glyphs.iter().zip(&xs) {
                    if g.atlas_w <= 0.0 || g.atlas_h <= 0.0 {
                        continue;
                    }
                    v.push(DrawCommand {
                        texture: tex,
                        size: TextureInfo {
                            width: ATLAS_SZ,
                            height: ATLAS_SZ,
                        },
                        transform: Affine2::from_translation(Vec2::new(
                            ly.left + gx + g.offset_x,
                            ruby_top + g.offset_y,
                        )),
                        opacity: 1.0,
                        blend: BlendMode::Alpha,
                        color: ColorFilter {
                            multiply: color,
                            grayscale: false,
                            negative: false,
                        },
                        clip: ClipRect {
                            uv_offset: [g.atlas_x / ATLAS_SZ as f32, g.atlas_y / ATLAS_SZ as f32],
                            uv_scale: [g.atlas_w / ATLAS_SZ as f32, g.atlas_h / ATLAS_SZ as f32],
                            quad_size: [g.atlas_w, g.atlas_h],
                        },
                        clip_bounds: None,
                        shader: None,
                        mesh: None,
                        stencil: None,
                    });
                }
            }

            // ── link type=0 hover 强调：白色方形板加法合成叠加 ──
            if links_enabled && let Some((wx, wy)) = self.white_patch {
                for link in ly.links.iter().filter(|k| k.hovered && k.link_type == 0) {
                    let end = link.end_or(ly.text_buffer.len());
                    for (x, y, w, h) in link_line_rects(&ly.text_buffer, &laid, link.start, end, lh)
                    {
                        v.push(DrawCommand {
                            texture: tex,
                            size: TextureInfo {
                                width: ATLAS_SZ,
                                height: ATLAS_SZ,
                            },
                            transform: Affine2::from_translation(Vec2::new(
                                ly.left + x,
                                ly.top + y,
                            )),
                            // 文档为"渐变叠加"；这里先以固定半透明近似，
                            // 呼吸式渐变需要接入帧时钟后再补
                            opacity: 0.5,
                            blend: BlendMode::Add,
                            color: ColorFilter {
                                multiply: [1.0, 1.0, 1.0],
                                grayscale: false,
                                negative: false,
                            },
                            clip: ClipRect {
                                // 采样 atlas 白块中心 1px，拉伸成整块矩形
                                uv_offset: [
                                    wx as f32 / ATLAS_SZ as f32,
                                    wy as f32 / ATLAS_SZ as f32,
                                ],
                                uv_scale: [1.0 / ATLAS_SZ as f32, 1.0 / ATLAS_SZ as f32],
                                quad_size: [w, h],
                            },
                            clip_bounds: None,
                            shader: None,
                            mesh: None,
                            stencil: None,
                        });
                    }
                }
            }

            if !v.is_empty() {
                out.insert(lid.clone(), v);
            }
        }
        out
    }

    // ── 逐字显示（Scetween） ──

    fn set_scetween(&mut self, config: ScetweenConfig) {
        let layer = self.state.active_layer_mut();
        match config.set_mode {
            crate::text::render::ScetweenSetMode::Init => {
                // init：替换同类型（同 ScetweenMode）的现有配置。
                // 如果层里已有 type=in 的配置，再来一个 type=in init，旧的被替换。
                // 不同类型（show/hide/in）互不影响，可同时存在。
                layer.scetween.retain(|c| c.mode != config.mode);
                layer.scetween.push(config);
            }
            crate::text::render::ScetweenSetMode::Add => {
                // add：追加配置，不替换现有的。
                layer.scetween.push(config);
            }
        }
    }

    fn reset_reveal(&mut self) {
        let layer = self.state.active_layer_mut();
        layer.reveal_index = 0;
        layer.reveal_pending = true;
        layer.reveal_clock_ms = 0;
    }

    fn advance_reveal(&mut self, delta_ms: u64) {
        let lids: Vec<String> = self.state.layers.keys().cloned().collect();
        for lid in &lids {
            let layer = match self.state.layers.get_mut(lid) {
                Some(l) => l,
                None => continue,
            };
            if !layer.reveal_pending || layer.text_buffer.is_empty() {
                continue;
            }
            layer.reveal_clock_ms = layer.reveal_clock_ms.saturating_add(delta_ms);

            let char_count = layer.text_buffer.len();

            // randomdelay=1 的配置：按当前页字符数生成随机揭示顺序表
            // （字符下标 → 延迟槽位的置换）。文本追加后长度变化时重新生成。
            for cfg in layer.scetween.iter_mut() {
                if cfg.random_delay
                    && cfg
                        .random_order
                        .as_ref()
                        .is_none_or(|order| order.len() != char_count)
                {
                    cfg.random_order = Some(shuffled_order(char_count));
                }
            }

            // 无 scetween：立即全量
            if layer.scetween.is_empty() {
                layer.reveal_index = char_count;
                layer.reveal_pending = false;
                continue;
            }

            // 根据 text_hidden 选取相关配置：
            // - 未隐藏 → 入场配置驱动揭示（is_entrance=true）
            // - 已隐藏 → 退场配置驱动揭示（is_entrance=false）
            let relevant: Vec<&ScetweenConfig> = layer
                .scetween
                .iter()
                .filter(|c| c.mode.is_entrance() != layer.text_hidden)
                .collect();

            // 没有相关配置 → 立即全量
            if relevant.is_empty() {
                layer.reveal_index = char_count;
                layer.reveal_pending = false;
                continue;
            }

            // 用相关配置中"最长"的总时长决定揭示进度
            let max_delay = relevant.iter().map(|c| c.delay_per_char).max().unwrap_or(0);
            let max_total: u64 = relevant
                .iter()
                .map(|c| {
                    (char_count.saturating_sub(1) as u64)
                        .saturating_mul(c.delay_per_char)
                        .saturating_add(c.time_per_char)
                })
                .max()
                .unwrap_or(0);

            // delay=0 且 time=0：无动画，一次性全揭示
            if max_delay == 0 && max_total == 0 {
                layer.reveal_index = char_count;
                layer.reveal_pending = false;
                continue;
            }

            // delay=0：所有字符同时开始动画，reveal_index 直接置满
            if max_delay == 0 {
                layer.reveal_index = char_count;
                if layer.reveal_clock_ms >= max_total {
                    layer.reveal_pending = false;
                }
                continue;
            }

            // 有 delay：按时间逐步增加 reveal_index（只增不减）
            let chars_revealed = (layer.reveal_clock_ms / max_delay) as usize + 1;
            let new_index = chars_revealed.min(char_count);
            if new_index > layer.reveal_index {
                layer.reveal_index = new_index;
            }
            if layer.reveal_index >= char_count && layer.reveal_clock_ms >= max_total {
                layer.reveal_pending = false;
            }
        }
    }

    fn reveal_all(&mut self) {
        for (_lid, layer) in self.state.layers.iter_mut() {
            if layer.text_buffer.is_empty() {
                continue;
            }
            layer.reveal_index = layer.text_buffer.len();
            layer.reveal_pending = false;
            // Skip/click-to-reveal 必须把 scetween 时钟推到动画结束，
            // 否则 delay=0 & time>0 的场景里所有字符的 alpha 都还停在
            // 起始值（通常为 0），视觉上就是"只看到两三个字"或全透明。
            if !layer.scetween.is_empty() {
                let char_count = layer.text_buffer.len();
                // 用相关配置（入场 or 退场）中"最长"的总时长
                let max_total: u64 = layer
                    .scetween
                    .iter()
                    .filter(|c| c.mode.is_entrance() != layer.text_hidden)
                    .map(|c| {
                        (char_count.saturating_sub(1) as u64)
                            .saturating_mul(c.delay_per_char)
                            .saturating_add(c.time_per_char)
                    })
                    .max()
                    .unwrap_or(0);
                if layer.reveal_clock_ms < max_total {
                    layer.reveal_clock_ms = max_total;
                }
            }
        }
    }

    fn hide_text(&mut self) {
        let layer = self.state.active_layer_mut();
        layer.text_hidden = true;
        layer.reveal_index = 0;
        layer.reveal_clock_ms = 0;
        layer.reveal_pending = true;
    }

    fn show_text(&mut self) {
        let layer = self.state.active_layer_mut();
        layer.text_hidden = false;
        layer.reveal_index = 0;
        layer.reveal_clock_ms = 0;
        layer.reveal_pending = true;
    }

    fn is_reveal_complete(&self) -> bool {
        self.state.layers.values().all(|layer| {
            layer.text_buffer.is_empty()
                || (!layer.reveal_pending && layer.reveal_index >= layer.text_buffer.len())
        })
    }

    fn font_state(&self) -> &FontState {
        &self.state
    }
    fn font_state_mut(&mut self) -> &mut FontState {
        &mut self.state
    }
}

#[cfg(test)]
mod tests {
    use super::{
        layout_glyphs, link_line_rects, ruby_positions, scetween_char_offset, shuffled_order,
        GlyphTextRenderer,
    };
    use crate::render_pipeline::draw::TextureId;
    use crate::text::backlog::BacklogTag;
    use crate::text::render::{GlyphInfo, ScetweenConfig, TextLayoutConfig, TextRenderer};
    use std::collections::HashMap;

    /// 构造一个测试字形：宽度与步进均可指定。
    fn glyph(c: char, width: f32, advance: f32) -> GlyphInfo {
        GlyphInfo {
            character: c.to_string(),
            texture_id: TextureId(0),
            atlas_x: 0.0,
            atlas_y: 0.0,
            atlas_w: 0.0,
            atlas_h: 0.0,
            offset_x: 0.0,
            offset_y: 0.0,
            width,
            height: 0.0,
            advance_x: advance,
        }
    }

    /// 把字符串转成等宽字形序列（宽度/步进均为 10，换行符零宽）。
    fn glyphs(s: &str) -> Vec<GlyphInfo> {
        s.chars()
            .map(|c| {
                if c == '\n' {
                    glyph('\n', 0.0, 0.0)
                } else {
                    glyph(c, 10.0, 10.0)
                }
            })
            .collect()
    }

    #[test]
    fn popping_message_layer_restores_its_logical_font_face() {
        let mut renderer = GlyphTextRenderer::new();
        renderer.switch_message_layer(Some("adv"), true);
        renderer.apply_font_settings(&HashMap::from([(
            "face".to_string(),
            "font/story.ttf".to_string(),
        )]));
        renderer.switch_message_layer(Some("save_slot"), true);
        renderer.apply_font_settings(&HashMap::from([(
            "face".to_string(),
            "font/ui.ttf".to_string(),
        )]));
        assert_eq!(renderer.active_font_face(), Some("font/ui.ttf"));

        renderer.pop_message_layer();
        assert_eq!(renderer.active_font_face(), Some("font/story.ttf"));
    }

    #[test]
    fn chgmsg_stack_zero_does_not_push_layer_stack() {
        // stack=1 压栈，stack=0 不压（chgmsg stack=0 防存档膨胀）。
        let mut r = GlyphTextRenderer::new();
        r.switch_message_layer(Some("a"), true);
        r.switch_message_layer(Some("b"), true); // 压入 a
        r.switch_message_layer(Some("c"), false); // 不压入 b
        // 弹栈应回到 a（b 未入栈），而不是 b。
        r.pop_message_layer();
        assert_eq!(r.state.active_layer.as_deref(), Some("a"));
    }

    // ── 禁则处理（prohibit） ──

    #[test]
    fn prohibit_head_char_hangs_on_previous_line() {
        // 行宽 25 只装得下 2 个字符；第 3 个字符是行首禁则的「。」→ 上提悬挂
        let gs = glyphs("ああ。あ");
        let laid = layout_glyphs(&gs, 25.0, &TextLayoutConfig::default(), &[]);
        assert_eq!(laid[2].line, 0, "行首禁则字符应上提留在第一行");
        assert_eq!(laid[2].x, 20.0);
        assert_eq!(laid[3].line, 1, "后续普通字符正常换行");
        assert_eq!(laid[3].x, 0.0);
    }

    #[test]
    fn prohibit_foot_char_moves_to_next_line() {
        // 行宽 25：第 3 个字符触发换行，但行尾是禁则的「「」→ 连同下移
        let gs = glyphs("あ「あ");
        let laid = layout_glyphs(&gs, 25.0, &TextLayoutConfig::default(), &[]);
        assert_eq!(laid[0].line, 0);
        assert_eq!(laid[1].line, 1, "行尾禁则字符应下移到下一行");
        assert_eq!(laid[1].x, 0.0);
        assert_eq!(laid[2].line, 1);
        assert_eq!(laid[2].x, 10.0);
    }

    #[test]
    fn prohibit_sets_can_be_overridden() {
        // 覆盖禁则表后，默认禁则字符不再生效
        let mut cfg = TextLayoutConfig::default();
        cfg.prohibit_head = "".into();
        cfg.prohibit_foot = "".into();
        let gs = glyphs("ああ。あ");
        let laid = layout_glyphs(&gs, 25.0, &cfg, &[]);
        assert_eq!(laid[2].line, 1, "清空行首禁则后「。」按纯宽度换行");
    }

    // ── wordparts ──

    #[test]
    fn wordparts_wraps_whole_word_to_next_line() {
        // "あword"：断点落在单词内部 → 整个单词回退到下一行
        let gs = glyphs("あword");
        let laid = layout_glyphs(&gs, 45.0, &TextLayoutConfig::default(), &[]);
        assert_eq!(laid[0].line, 0);
        for k in 1..5 {
            assert_eq!(laid[k].line, 1, "单词不应被拦腰截断（下标 {k}）");
        }
        assert_eq!(laid[1].x, 0.0);
        assert_eq!(laid[4].x, 30.0);
    }

    #[test]
    fn wordparts_forces_break_when_line_is_single_word() {
        // 整行都是一个单词：无法回退 → 强制在原断点截断
        let gs = glyphs("words");
        let laid = layout_glyphs(&gs, 35.0, &TextLayoutConfig::default(), &[]);
        assert_eq!(laid[2].line, 0);
        assert_eq!(laid[3].line, 1, "整行单词只能强制拦腰截断");
        assert_eq!(laid[3].x, 0.0);
    }

    // ── indent ──

    #[test]
    fn indent_open_char_indents_following_lines_until_closed() {
        let mut cfg = TextLayoutConfig::default();
        cfg.indent_pair = "「」".into();
        // 行宽 35：「あああ」换行后的续行应缩进一个「的宽度；
        // 」闭合后显式换行的新行回到 0
        let gs = glyphs("「ああああ」\nあ");
        let laid = layout_glyphs(&gs, 35.0, &cfg, &[]);
        assert_eq!(laid[0].x, 0.0);
        assert_eq!(laid[3].line, 1, "第 4 个字符触发自动换行");
        assert_eq!(laid[3].x, 10.0, "续行应从「的右端缩进起排");
        assert_eq!(laid[7].line, 2);
        assert_eq!(laid[7].x, 0.0, "」闭合后缩进应恢复");
    }

    #[test]
    fn indent_range_limits_open_char_recognition() {
        let base = "あ「あああ";
        // range=1：只识别行首第 1 个字符 → 位置 1 的「不识别，续行不缩进
        let mut cfg = TextLayoutConfig::default();
        cfg.indent_pair = "「」".into();
        cfg.indent_range = Some(1);
        let laid = layout_glyphs(&glyphs(base), 45.0, &cfg, &[]);
        assert_eq!(laid[4].line, 1);
        assert_eq!(laid[4].x, 0.0, "range 之外的开始字符不应触发缩进");

        // range=2：位置 1 的「在识别范围内 → 续行缩进到「右端（x=20）
        cfg.indent_range = Some(2);
        let laid = layout_glyphs(&glyphs(base), 45.0, &cfg, &[]);
        assert_eq!(laid[4].x, 20.0, "range 之内的开始字符应触发缩进");
    }

    #[test]
    fn indent_nest_controls_repeated_indentation() {
        let base = "「『ああああ";
        // nest=0（缺省）：已缩进时忽略第二个开始字符
        let mut cfg = TextLayoutConfig::default();
        cfg.indent_pair = "「」『』".into();
        let laid = layout_glyphs(&glyphs(base), 45.0, &cfg, &[]);
        assert_eq!(laid[4].line, 1);
        assert_eq!(laid[4].x, 10.0, "nest=0 时第二个开始字符应被忽略");

        // nest=1：重复嵌套缩进 → 缩进推进到『右端
        cfg.indent_nest = true;
        let laid = layout_glyphs(&glyphs(base), 45.0, &cfg, &[]);
        assert_eq!(laid[4].x, 20.0, "nest=1 时应嵌套缩进");
    }

    // ── rt omitblankline ──

    #[test]
    fn line_break_is_omitted_when_last_line_is_blank() {
        let mut renderer = GlyphTextRenderer::new();
        // 空缓冲 + 默认 omit=1 → 跳过换行
        renderer.push_line_break();
        assert!(renderer.font_state().layers[super::DEFAULT_MESSAGE_LAYER]
            .text_buffer
            .is_empty());

        // 有文本时正常换行；紧接着的第二次换行（末行为空）被省略
        renderer
            .font_state_mut()
            .active_layer_mut()
            .text_buffer
            .push(glyph('あ', 10.0, 10.0));
        renderer.push_line_break();
        assert_eq!(
            renderer.font_state().layers[super::DEFAULT_MESSAGE_LAYER]
                .text_buffer
                .len(),
            2
        );
        renderer.push_line_break();
        assert_eq!(
            renderer.font_state().layers[super::DEFAULT_MESSAGE_LAYER]
                .text_buffer
                .len(),
            2,
            "末行为空时应省略换行"
        );

        // omitblankline=0：始终换行
        renderer.font_state_mut().set_rt_omit_blank_line(false);
        renderer.push_line_break();
        assert_eq!(
            renderer.font_state().layers[super::DEFAULT_MESSAGE_LAYER]
                .text_buffer
                .len(),
            3,
            "omit 关闭后应始终换行"
        );
    }

    // ── glyph 点击等待图标 ──

    #[test]
    fn click_wait_icon_placement_follows_last_char() {
        let mut renderer = GlyphTextRenderer::new();
        // 未配置图层 → 禁用
        assert!(renderer.click_wait_icon_placement(false).is_none());

        renderer.set_glyph_config(&HashMap::from([
            ("layer".to_string(), "icon1".to_string()),
            ("left".to_string(), "5".to_string()),
            ("top".to_string(), "-2".to_string()),
            ("rptop".to_string(), "3".to_string()),
            ("homing".to_string(), "1".to_string()),
        ]));
        // 已配置但无文本 → None
        assert!(renderer.click_wait_icon_placement(false).is_none());

        {
            let layer = renderer.font_state_mut().active_layer_mut();
            layer.left = 100.0;
            layer.top = 200.0;
            layer.text_buffer = glyphs("ああ\n");
        }
        // 行末图标：最后一个非换行字符（下标 1，x=10）右端 + left 偏移
        let p = renderer.click_wait_icon_placement(false).unwrap();
        assert_eq!(p.layer_id, "icon1");
        assert_eq!(p.left, 100.0 + 10.0 + 10.0 + 5.0);
        assert_eq!(p.top, 200.0 - 2.0);
        assert!(p.homing);

        // 页末图标：rplayer 缺省回退到 layer，偏移用 rpleft/rptop
        let rp = renderer.click_wait_icon_placement(true).unwrap();
        assert_eq!(rp.layer_id, "icon1");
        assert_eq!(rp.left, 100.0 + 10.0 + 10.0);
        assert_eq!(rp.top, 200.0 + 3.0);
    }

    #[test]
    fn click_wait_icon_page_end_uses_rplayer_when_set() {
        let mut renderer = GlyphTextRenderer::new();
        renderer.set_glyph_config(&HashMap::from([
            ("layer".to_string(), "icon1".to_string()),
            ("rplayer".to_string(), "icon2".to_string()),
        ]));
        renderer
            .font_state_mut()
            .active_layer_mut()
            .text_buffer
            .push(glyph('あ', 10.0, 10.0));
        assert_eq!(
            renderer.click_wait_icon_placement(true).unwrap().layer_id,
            "icon2"
        );
        assert_eq!(
            renderer.click_wait_icon_placement(false).unwrap().layer_id,
            "icon1"
        );
    }

    // ── scetween 整页属性（entire*） ──

    #[test]
    fn scetween_entire_param_animates_all_chars_in_sync() {
        let entire = ScetweenConfig {
            param: Some("entireleft".to_string()),
            diff: Some(-100.0),
            delay_per_char: 50,
            time_per_char: 100,
            ..Default::default()
        };
        // 整页属性无逐字符延迟：clock=100 ≥ time → 第 5 个字符也已到终点
        let off = scetween_char_offset(&[entire], 5, 100, false);
        assert_eq!(off.0, 0.0, "entireleft 应全体同步完成");

        // 对照：每字符 left 参数在同一时刻仍处于起点（start=5*50=250 > 100）
        let per_char = ScetweenConfig {
            param: Some("left".to_string()),
            diff: Some(-100.0),
            delay_per_char: 50,
            time_per_char: 100,
            ..Default::default()
        };
        let off = scetween_char_offset(&[per_char], 5, 100, false);
        assert_eq!(off.0, -100.0, "每字符 left 应仍按下标延迟");
    }

    #[test]
    fn scetween_entirealpha_starts_faded() {
        let cfg = ScetweenConfig {
            param: Some("entirealpha".to_string()),
            diff: Some(-255.0),
            delay_per_char: 0,
            time_per_char: 100,
            ..Default::default()
        };
        let off = scetween_char_offset(&[cfg.clone()], 3, 0, false);
        assert_eq!(off.5, 0.0, "entirealpha 动画开始时应全透明");
        let off = scetween_char_offset(&[cfg], 3, 100, false);
        assert_eq!(off.5, 1.0, "entirealpha 动画结束后应完全不透明");
    }

    // ── scetween randomdelay ──

    #[test]
    fn shuffled_order_is_deterministic_permutation() {
        let order = shuffled_order(32);
        assert_eq!(order, shuffled_order(32), "同长度应生成相同顺序");
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..32).collect::<Vec<_>>(), "必须是 0..n 的置换");
        assert_ne!(order, (0..32).collect::<Vec<_>>(), "应真的被打乱");
    }

    #[test]
    fn advance_reveal_generates_random_order_for_randomdelay() {
        let mut renderer = GlyphTextRenderer::new();
        {
            let layer = renderer.font_state_mut().active_layer_mut();
            layer.text_buffer = glyphs("abcdefgh");
            layer.reveal_pending = true;
            layer.scetween.push(ScetweenConfig {
                random_delay: true,
                delay_per_char: 10,
                ..Default::default()
            });
        }
        renderer.advance_reveal(0);
        let order = renderer.font_state().layers[super::DEFAULT_MESSAGE_LAYER].scetween[0]
            .random_order
            .clone()
            .expect("randomdelay 配置应生成随机顺序表");
        assert_eq!(order.len(), 8);
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..8).collect::<Vec<_>>());
    }

    #[test]
    fn scetween_uses_random_order_as_delay_slot() {
        // 字符 0 的槽位是 3（start=300ms）、字符 1 的槽位是 0（start=0ms）
        let cfg = ScetweenConfig {
            param: Some("left".to_string()),
            diff: Some(-100.0),
            delay_per_char: 100,
            time_per_char: 0,
            random_delay: true,
            random_order: Some(vec![3, 0]),
            ..Default::default()
        };
        let off = scetween_char_offset(&[cfg.clone()], 0, 50, false);
        assert_eq!(off.0, -100.0, "槽位 3 的字符在 50ms 时还未开始");
        let off = scetween_char_offset(&[cfg], 1, 50, false);
        assert_eq!(off.0, 0.0, "槽位 0 的字符在 50ms 时已完成");
    }

    // ── ruby ──

    #[test]
    fn ruby_range_is_not_split_by_wrap() {
        // 行宽 35：普通排版在第 4 个字符（下标 3）处换行；
        // 注音区间 [2,5) 不可拆 → 整体回退到下一行
        let gs = glyphs("あいうえおか");
        let laid = layout_glyphs(&gs, 35.0, &TextLayoutConfig::default(), &[(2, 5)]);
        assert_eq!(laid[1].line, 0);
        assert_eq!(laid[2].line, 1, "注音区间应整体移到下一行");
        assert_eq!(laid[2].x, 0.0);
        assert_eq!(laid[4].line, 1, "区间尾仍与区间头同行");

        // 区间从行首开始（整行装不下）时只能强制拦腰截断
        let laid = layout_glyphs(&gs, 35.0, &TextLayoutConfig::default(), &[(0, 6)]);
        assert_eq!(laid[3].line, 1, "行首起的超宽区间强制截断");
    }

    #[test]
    fn ruby_positions_center_over_base_span() {
        // 正文区间 X ∈ [10, 50)，注音两个字形各宽 8、kerning 2 → 总宽 18
        let xs = ruby_positions(10.0, 50.0, &[8.0, 8.0], 2.0);
        assert_eq!(xs, vec![21.0, 31.0], "注音应居中于正文区间上方");
        assert!(ruby_positions(0.0, 10.0, &[], 0.0).is_empty());
    }

    #[test]
    fn ruby_marks_range_and_reproduction_tags() {
        let mut r = GlyphTextRenderer::new();
        {
            let l = r.font_state_mut().active_layer_mut();
            l.text_buffer = glyphs("あい");
        }
        r.ruby_start("カピバラ");
        {
            // 无字体环境：注音正文手工放入缓冲
            let l = r.font_state_mut().active_layer_mut();
            l.text_buffer.push(glyph('鬼', 10.0, 10.0));
            l.text_buffer.push(glyph('鼠', 10.0, 10.0));
            // 未闭合的注音也是不可拆行区间（延伸到缓冲末尾）
            assert_eq!(l.keep_ranges(), vec![(2, 4)]);
        }
        r.ruby_end();
        let l = r.font_state_mut().active_layer_mut();
        assert_eq!(l.rubies.len(), 1);
        assert_eq!((l.rubies[0].start, l.rubies[0].end), (2, 4));
        assert_eq!(l.rubies[0].text, "カピバラ");
        assert_eq!(l.rubies[0].size, 20.0, "rubysize 缺省 = 正文字号 40 的一半");
        assert_eq!(l.keep_ranges(), vec![(2, 4)]);
        assert!(l
            .page_tags
            .contains(&BacklogTag::RubyStart("カピバラ".into())));
        assert!(l.page_tags.contains(&BacklogTag::RubyEnd));
    }

    // ── link ──

    #[test]
    fn link_line_rects_split_per_line() {
        let gs = glyphs("あいう\nえお");
        let laid = layout_glyphs(&gs, f32::MAX, &TextLayoutConfig::default(), &[]);
        // 链接覆盖 [1,6)（含换行字形），跨两行 → 每行一个矩形
        let rects = link_line_rects(&gs, &laid, 1, 6, 40.0);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0], (10.0, 0.0, 20.0, 40.0));
        assert_eq!(rects[1], (0.0, 40.0, 20.0, 40.0));
    }

    #[test]
    fn link_ranges_produce_hit_areas_and_hover_toggles() {
        let mut r = GlyphTextRenderer::new();
        {
            let l = r.font_state_mut().active_layer_mut();
            l.left = 100.0;
            l.top = 50.0;
        }
        r.link_start(Some("sel.ast"), Some("*top"), 1, Some("FFFFFF"), None, None);
        {
            let l = r.font_state_mut().active_layer_mut();
            l.text_buffer = glyphs("あいう");
        }
        r.link_end();
        {
            let l = &r.font_state().layers[super::DEFAULT_MESSAGE_LAYER];
            assert_eq!(l.links.len(), 1);
            assert_eq!((l.links[0].start, l.links[0].end), (0, Some(3)));
            assert_eq!(l.links[0].file.as_deref(), Some("sel.ast"));
            assert_eq!(l.links[0].link_type, 1);
        }

        // 命中区域：消息层偏移 + 3 字符 × 宽 10；无字体时行高=缺省字号 40
        let areas = r.link_hit_areas();
        assert_eq!(areas.len(), 1);
        assert_eq!(
            (areas[0].left, areas[0].top, areas[0].width, areas[0].height),
            (100.0, 50.0, 30.0, 40.0)
        );
        assert_eq!(areas[0].label.as_deref(), Some("*top"));

        // hover：进入区域置位并报告变化；重复移动不报变化；移出取消
        assert!(r.update_link_hover(110.0, 60.0));
        assert!(r.font_state().layers[super::DEFAULT_MESSAGE_LAYER].links[0].hovered);
        assert!(!r.update_link_hover(111.0, 61.0), "hover 状态未变不应报变化");
        assert!(r.update_link_hover(0.0, 0.0));
        assert!(!r.font_state().layers[super::DEFAULT_MESSAGE_LAYER].links[0].hovered);

        // linkdisable：不可点击（区域为空）且 hover 清除；linkenable 恢复
        assert!(r.update_link_hover(110.0, 60.0));
        r.set_links_enabled(false);
        assert!(r.link_hit_areas().is_empty());
        assert!(r.link_hit_test(110.0, 60.0).is_none());
        assert!(!r.font_state().layers[super::DEFAULT_MESSAGE_LAYER].links[0].hovered);
        r.set_links_enabled(true);
        assert_eq!(r.link_hit_areas().len(), 1);
        assert!(r.link_hit_test(110.0, 60.0).is_some());
    }

    // ── fontinit / fontdefault ──

    #[test]
    fn fontinit_restores_default_and_clears_font_stack() {
        let mut r = GlyphTextRenderer::new();
        r.font_default(&HashMap::from([("size".to_string(), "30".to_string())]));
        // stack 缺省 1 → 应用前压栈
        r.apply_font_settings(&HashMap::from([("size".to_string(), "50".to_string())]));
        {
            let l = r.font_state_mut().active_layer_mut();
            assert_eq!(l.font.size, Some(50.0));
            assert_eq!(l.font_stack.len(), 1);
        }
        r.font_init();
        let l = r.font_state_mut().active_layer_mut();
        assert_eq!(l.font.size, Some(30.0), "fontinit 应恢复 fontdefault 默认");
        assert!(l.font_stack.is_empty(), "文档：字体堆栈将被初始化（清空）");
    }

    #[test]
    fn fontdefault_applies_to_newly_created_message_layer() {
        let mut r = GlyphTextRenderer::new();
        r.font_default(&HashMap::from([
            ("size".to_string(), "22".to_string()),
            ("color".to_string(), "AABBCC".to_string()),
        ]));
        r.switch_message_layer(Some("fresh"), true);
        let l = r.font_state_mut().active_layer_mut();
        assert_eq!(l.font.size, Some(22.0), "首次使用的消息层应用默认字号");
        assert_eq!(l.font.color.as_deref(), Some("AABBCC"));
    }

    // ── backlog ──

    #[test]
    fn page_break_stores_history_per_writebacklog_and_rp() {
        let mut r = GlyphTextRenderer::new();
        r.push_text("第一页", false);
        r.push_page_break(None); // writebacklog 缺省 0 → 不入库
        assert_eq!(r.font_state().get_backlog_size(), 0);

        r.font_state_mut().backlog.set_write_mode(true);
        r.push_text("第二页", false);
        r.push_page_break(None); // mode=1 → 入库
        assert_eq!(r.font_state().get_backlog_size(), 1);

        r.push_text("第三页", false);
        r.push_page_break(Some(0)); // rp backlog=0 无视 writebacklog=1
        assert_eq!(r.font_state().get_backlog_size(), 1);

        r.font_state_mut().backlog.set_write_mode(false);
        r.push_text("第四页", false);
        r.push_page_break(Some(1)); // rp backlog=1 无视 writebacklog=0
        assert_eq!(r.font_state().get_backlog_size(), 2);

        assert_eq!(
            r.font_state().get_backlog_tags(0, false).unwrap(),
            vec!["[print data=\"第二页\"]"]
        );
        assert_eq!(
            r.font_state().get_backlog_tags(1, false).unwrap(),
            vec!["[print data=\"第四页\"]"]
        );
        assert!(r.font_state().get_backlog_tags(2, false).is_none());
    }

    #[test]
    fn message_tags_reproduce_current_page() {
        let mut r = GlyphTextRenderer::new();
        r.font_default(&HashMap::from([("size".to_string(), "40".to_string())]));
        r.font_state_mut().set_rt_omit_blank_line(false);
        r.push_text("あい", false);
        r.push_line_break();
        r.apply_font_settings(&HashMap::from([(
            "color".to_string(),
            "FF0000".to_string(),
        )]));
        r.push_text("うえ", false);

        let tags = r
            .font_state()
            .get_message_tags(super::DEFAULT_MESSAGE_LAYER, false)
            .unwrap();
        assert_eq!(
            tags,
            vec![
                "[print data=\"あい\"]",
                "[rt]",
                "[font color=\"FF0000\"]",
                "[print data=\"うえ\"]",
            ]
        );
        // allfont=1：附页首字体快照（fontdefault 设置的 size=40）
        let with_font = r
            .font_state()
            .get_message_tags(super::DEFAULT_MESSAGE_LAYER, true)
            .unwrap();
        assert_eq!(with_font[0], "[font size=\"40\"]");
        assert!(r.font_state().get_message_tags("nope", false).is_none());

        // 换页后本页再现标签清空
        r.push_page_break(None);
        assert!(r
            .font_state()
            .get_message_tags(super::DEFAULT_MESSAGE_LAYER, false)
            .unwrap()
            .is_empty());
    }
}

/// 计算单个字符在所有 scetween 配置共同作用下的动画偏移量。
///
/// 根据 `text_hidden` 选取相关配置（入场 or 退场），把各配置的贡献叠加。
/// 返回 `(offset_x, offset_y, scale_x, scale_y, rotate_degrees, alpha)`，
/// 其中位置偏移为像素值，缩放为倍数（1.0=无缩放），alpha 为 0.0-1.0。
fn scetween_char_offset(
    configs: &[ScetweenConfig],
    char_index: usize,
    reveal_clock_ms: u64,
    text_hidden: bool,
) -> (f32, f32, f32, f32, f32, f32) {
    if configs.is_empty() {
        return (0.0, 0.0, 1.0, 1.0, 0.0, 1.0);
    }

    let mut ox = 0.0f32;
    let mut oy = 0.0f32;
    let mut sx = 1.0f32;
    let mut sy = 1.0f32;
    let mut rot = 0.0f32;
    let mut alpha = 1.0f32;

    for cfg in configs {
        // 根据隐藏状态选取相关配置：
        // - 未隐藏 → 入场配置（is_entrance=true）
        // - 已隐藏 → 退场配置（is_entrance=false）
        if cfg.mode.is_entrance() == text_hidden {
            continue;
        }

        // 延迟槽位：entire* 整页属性全体字符同步（无逐字符延迟）；
        // randomdelay 按随机顺序表映射；否则按字符下标顺序延迟
        let delay_slot = if cfg.is_entire_param() {
            0
        } else {
            cfg.random_order
                .as_ref()
                .and_then(|order| order.get(char_index).copied())
                .unwrap_or(char_index)
        };
        let char_start_ms = delay_slot as u64 * cfg.delay_per_char;
        let (start_x, start_y, start_sx, start_sy, start_r, start_a) = scetween_start_value(cfg);

        let (t_start, t_end) = if cfg.mode.is_entrance() {
            // 入场：从 start → normal
            (
                (start_x, start_y, start_sx, start_sy, start_r, start_a),
                (0.0, 0.0, 1.0, 1.0, 0.0, 1.0),
            )
        } else {
            // 退场：从 normal → start
            (
                (0.0, 0.0, 1.0, 1.0, 0.0, 1.0),
                (start_x, start_y, start_sx, start_sy, start_r, start_a),
            )
        };

        if reveal_clock_ms < char_start_ms {
            // 尚未到达该字符的动画开始时间 → 显示起点
            ox += t_start.0;
            oy += t_start.1;
            sx *= t_start.2;
            sy *= t_start.3;
            rot += t_start.4;
            alpha *= t_start.5;
            continue;
        }

        let elapsed = reveal_clock_ms - char_start_ms;
        if cfg.time_per_char == 0 || elapsed >= cfg.time_per_char {
            // 动画已结束 → 显示终点
            ox += t_end.0;
            oy += t_end.1;
            sx *= t_end.2;
            sy *= t_end.3;
            rot += t_end.4;
            alpha *= t_end.5;
            continue;
        }

        // 动画进行中 → 按缓动插值
        let t = elapsed as f32 / cfg.time_per_char as f32;
        let progress = cfg.ease.apply(t);

        ox += t_start.0 + (t_end.0 - t_start.0) * progress;
        oy += t_start.1 + (t_end.1 - t_start.1) * progress;
        sx *= t_start.2 + (t_end.2 - t_start.2) * progress;
        sy *= t_start.3 + (t_end.3 - t_start.3) * progress;
        rot += t_start.4 + (t_end.4 - t_start.4) * progress;
        alpha *= t_start.5 + (t_end.5 - t_start.5) * progress;
    }

    (ox, oy, sx, sy, rot, alpha.clamp(0.0, 1.0))
}

/// 根据 scetween 配置计算动画的"起点"值。
///
/// 注意：`cfg.diff` 对于 alpha 参数使用 Artemis 的 0-255 范围，
/// 这里需要转换到 0-1 的归一化范围；其余参数使用原始值（像素/百分比/度）。
///
/// entire* 参数为整页属性：取值与对应的每字符参数相同，区别在于
/// `scetween_char_offset` 里整页动画不做逐字符延迟（全体字符同步插值）。
/// 局限：entirexscale/entireyscale/entirerotate 目前仍以每个字形自身中心
/// 为轴变换，尚未实现整页统一锚点。
fn scetween_start_value(cfg: &ScetweenConfig) -> (f32, f32, f32, f32, f32, f32) {
    let diff = cfg.diff.unwrap_or(0.0);
    match cfg.param.as_deref() {
        Some("left") | Some("entireleft") => (diff, 0.0, 1.0, 1.0, 0.0, 1.0),
        Some("top") | Some("entiretop") => (0.0, diff, 1.0, 1.0, 0.0, 1.0),
        Some("alpha") | Some("entirealpha") => {
            // Artemis 用 0-255 的 diff；转换到 0-1。这里只计算"淡出端"的
            // alpha（如 diff=-255 → 0.0）；入场从这端渐入、退场向这端渐出，
            // 方向由调用方交换起终点实现，因此两种模式取值相同。
            let faded_a = (255.0 + diff).clamp(0.0, 255.0) / 255.0;
            (0.0, 0.0, 1.0, 1.0, 0.0, faded_a)
        }
        Some("xscale") | Some("entirexscale") => {
            let start_s = 1.0 + diff / 100.0;
            (0.0, 0.0, start_s, 1.0, 0.0, 1.0)
        }
        Some("yscale") | Some("entireyscale") => {
            let start_s = 1.0 + diff / 100.0;
            (0.0, 0.0, 1.0, start_s, 0.0, 1.0)
        }
        Some("rotate") | Some("entirerotate") => (0.0, 0.0, 1.0, 1.0, diff, 1.0),
        _ => (0.0, 0.0, 1.0, 1.0, 0.0, 1.0),
    }
}

/// 计算链接区间 `[start, end)` 在排版结果中的逐行矩形。
///
/// 返回 `(x, y, w, h)` 列表（相对消息层原点，未加 layer.left/top）：
/// 链接跨行时每行一个矩形，矩形高为行高。换行字形（零宽）不参与。
pub(crate) fn link_line_rects(
    glyphs: &[GlyphInfo],
    laid: &[LaidGlyph],
    start: usize,
    end: usize,
    line_height: f32,
) -> Vec<(f32, f32, f32, f32)> {
    let mut out: Vec<(f32, f32, f32, f32)> = Vec::new();
    let end = end.min(glyphs.len()).min(laid.len());
    if start >= end {
        return out;
    }
    // (行号, 行内最左 X, 行内最右 X)
    let mut cur: Option<(usize, f32, f32)> = None;
    for i in start..end {
        let g = &glyphs[i];
        if g.character == "\n" {
            continue;
        }
        let line = laid[i].line;
        let x0 = laid[i].x;
        let x1 = laid[i].x + g.advance_x;
        cur = match cur {
            Some((l, cx0, cx1)) if l == line => Some((l, cx0.min(x0), cx1.max(x1))),
            Some((l, cx0, cx1)) => {
                out.push((cx0, l as f32 * line_height, cx1 - cx0, line_height));
                Some((line, x0, x1))
            }
            None => Some((line, x0, x1)),
        };
    }
    if let Some((l, cx0, cx1)) = cur {
        out.push((cx0, l as f32 * line_height, cx1 - cx0, line_height));
    }
    out
}

/// 注音串水平居中于正文区间上方时，每个注音字形的 X 起点。
///
/// `base_x0`/`base_x1` 为正文区间的左右端 X（行内坐标）；
/// `advances` 为各注音字形的步进；`kerning` 为 rubykerning。
pub(crate) fn ruby_positions(
    base_x0: f32,
    base_x1: f32,
    advances: &[f32],
    kerning: f32,
) -> Vec<f32> {
    if advances.is_empty() {
        return Vec::new();
    }
    let total: f32 = advances.iter().sum::<f32>() + kerning * (advances.len() - 1) as f32;
    let mut x = base_x0 + ((base_x1 - base_x0) - total) * 0.5;
    advances
        .iter()
        .map(|a| {
            let p = x;
            x += a + kerning;
            p
        })
        .collect()
}

/// 生成 0..n 的确定性伪随机置换，用于 randomdelay 的字符揭示顺序。
///
/// 用固定种子的 LCG 做 Fisher-Yates 洗牌：同一页字符数相同则顺序相同，
/// 保证可测试且逐帧稳定（不依赖外部随机源）。
fn shuffled_order(n: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..n).collect();
    // 以字符数扰动种子，不同长度的页得到不同顺序
    let mut seed: u64 = (n as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x5DEE_CE66_D5AA_55AA;
    for i in (1..n).rev() {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let j = ((seed >> 33) as usize) % (i + 1);
        order.swap(i, j);
    }
    order
}
