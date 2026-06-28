//! 基于 ab_glyph 的字形光栅化文本渲染器。

use crate::render_pipeline::draw::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, TextureId, TextureInfo, TextureProvider,
};
use crate::text::render::{FontState, GlyphInfo, ScetweenConfig, TextRenderer};
use ab_glyph::{Font, FontRef, PxScale, PxScaleFont, ScaleFont};
use glam::{Affine2, Vec2};
use std::collections::HashMap;

const ATLAS_SZ: u32 = 1024;
const ATLAS_NAME: &str = ":text/atlas";
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
    fn flush(&mut self, p: &mut dyn TextureProvider) -> (TextureId, TextureInfo) {
        if self.dirty {
            if let Some(r) = p.upload_rgba(ATLAS_NAME, ATLAS_SZ, ATLAS_SZ, &self.px) {
                self.dirty = false;
                return r;
            }
        }
        p.resolve(ATLAS_NAME).unwrap()
    }
}

pub struct GlyphTextRenderer<'font> {
    state: FontState,
    font: Option<FontRef<'font>>,
    atlas: Atlas,
    cache: HashMap<(u16, u32), (u32, u32, u32, u32)>,
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

impl<'font> GlyphTextRenderer<'font> {
    pub fn new() -> Self {
        Self {
            state: FontState::new(),
            font: None,
            atlas: Atlas::new(),
            cache: HashMap::new(),
        }
    }
    pub fn set_font(&mut self, bytes: &'font [u8]) -> Result<(), String> {
        self.font = Some(FontRef::try_from_slice(bytes).map_err(|e| format!("{e}"))?);
        Ok(())
    }
}

impl TextRenderer for GlyphTextRenderer<'_> {
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
        self.state.active_layer_mut().font = d;
    }
    fn font_pop(&mut self) {
        let l = self.state.active_layer_mut();
        if let Some(v) = l.font_stack.pop() {
            l.font = v;
        }
    }
    fn font_default(&mut self, s: &HashMap<String, String>) {
        self.state.default_font.merge_raw(s);
    }
    fn switch_message_layer(&mut self, id: Option<&str>) {
        let prev_state = self.state.active_layer.as_ref().and_then(|aid| {
            self.state
                .layers
                .get(aid)
                .map(|l| (l.left, l.top, l.width, l.height, l.font.clone()))
        });
        if let Some(ref prev_id) = self.state.active_layer {
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
        layer.text_buffer.clear();
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
    }

    fn push_text(&mut self, content: &str, _inline: bool) {
        let layer = self.state.active_layer_mut();
        let sz = layer.font.size.unwrap_or(40.0);
        let scale = PxScale::from(sz);
        let sf = scaled(&self.font, scale);
        let sf = match sf {
            Some(s) => s,
            None => return,
        };
        let was_empty = layer.text_buffer.is_empty();
        if was_empty {
            layer.reveal_pending = true;
            layer.reveal_clock_ms = 0;
            layer.reveal_index = 0;
        }

        for c in content.chars() {
            let q = sf.outline_glyph(sf.glyph_id(c).with_scale(sz));
            if let Some(q) = q {
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
                            let rgba: Vec<u8> =
                                g.iter().flat_map(|&a| [255u8, 255, 255, a]).collect();
                            self.atlas.write(x, y, w, h, &rgba);
                            (x, y, w, h)
                        } else {
                            (0, 0, 0, 0)
                        }
                    })
                } else {
                    (0, 0, 0, 0)
                };
                layer.text_buffer.push(GlyphInfo {
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
                });
            }
        }
    }

    fn push_line_break(&mut self) {
        let layer = self.state.active_layer_mut();
        let sz = layer.font.size.unwrap_or(40.0);
        let scale = PxScale::from(sz);
        let sf = scaled(&self.font, scale);
        let sf = match sf {
            Some(s) => s,
            None => return,
        };
        layer.text_buffer.push(GlyphInfo {
            character: "\n".into(),
            texture_id: TextureId(0),
            atlas_x: 0.0,
            atlas_y: 0.0,
            atlas_w: 0.0,
            atlas_h: 0.0,
            offset_x: 0.0,
            offset_y: sf.height(),
            width: 0.0,
            height: 0.0,
            advance_x: 0.0,
        });
    }

    fn push_page_break(&mut self, _bl: Option<i32>) {
        let lid = self.state.active_layer.clone().unwrap_or_default();
        if let Some(l) = self.state.layers.get_mut(&lid) {
            l.text_buffer.clear();
            l.reveal_index = 0;
            l.reveal_pending = false;
            l.reveal_clock_ms = 0;
        }
    }

    fn build_text_commands(
        &mut self,
        p: &mut dyn TextureProvider,
    ) -> HashMap<String, Vec<DrawCommand>> {
        let (tex, _) = self.atlas.flush(p);
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

            let sz = ly.font.size.unwrap_or(40.0);
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

            let mut cx: f32 = 0.0;
            let mut line_y: f32 = 0.0;
            let mut ls: usize = 0;
            let mut v = Vec::new();
            for (i, g) in ly.text_buffer.iter().enumerate() {
                if i >= visible_count {
                    // 尚未揭示的字符：跳过
                    continue;
                }

                if g.character == "\n" {
                    cx = 0.0;
                    line_y += lh;
                    ls = i + 1;
                    continue;
                }
                if cx + g.width > lw && i > ls {
                    cx = 0.0;
                    line_y += lh;
                    ls = i;
                }

                let fx = ly.left + cx + g.offset_x;
                let fy = ly.top + g.offset_y + line_y;

                // 计算每字符的 scetween 动画偏移
                let anim_offset =
                    scetween_char_offset(scethweens, i, ly.reveal_clock_ms, text_hidden);

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
                            multiply: color,
                            grayscale: false,
                            negative: false,
                        },
                        clip: clip.clone(),
                    };
                    if has_shadow {
                        let mut sc = base;
                        let sd = ly.font.shadow_size.unwrap_or(2.0);
                        sc.color.multiply = oc;
                        sc.transform = Affine2::from_translation(Vec2::new(pos_x + sd, pos_y + sd));
                        v.push(sc);
                    }
                    if has_outline {
                        let os = ly.font.outline_size.unwrap_or(1.0);
                        for &(ox, oy) in &OUTLINE_OFFSETS {
                            let mut ocp = base;
                            ocp.color.multiply = oc;
                            ocp.transform = Affine2::from_translation(Vec2::new(
                                pos_x + ox * os,
                                pos_y + oy * os,
                            ));
                            v.push(ocp);
                        }
                    }
                    v.push(base);
                }
                cx += g.advance_x;
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

        let char_start_ms = char_index as u64 * cfg.delay_per_char;
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
fn scetween_start_value(cfg: &ScetweenConfig) -> (f32, f32, f32, f32, f32, f32) {
    let diff = cfg.diff.unwrap_or(0.0);
    match cfg.param.as_deref() {
        Some("left") => (diff, 0.0, 1.0, 1.0, 0.0, 1.0),
        Some("top") => (0.0, diff, 1.0, 1.0, 0.0, 1.0),
        Some("alpha") => {
            // Artemis 用 0-255 的 diff；转换到 0-1
            let start_a = if cfg.mode.is_entrance() {
                // 入场：alpha 从 0 渐入到 1（或从 (255+diff)/255 开始）
                (255.0 + diff).clamp(0.0, 255.0) / 255.0
            } else {
                // 退场：alpha 从 1 渐出到 0（或从 (255+diff)/255 开始）
                (255.0 + diff).clamp(0.0, 255.0) / 255.0
            };
            (0.0, 0.0, 1.0, 1.0, 0.0, start_a)
        }
        Some("xscale") => {
            let start_s = 1.0 + diff / 100.0;
            (0.0, 0.0, start_s, 1.0, 0.0, 1.0)
        }
        Some("yscale") => {
            let start_s = 1.0 + diff / 100.0;
            (0.0, 0.0, 1.0, start_s, 0.0, 1.0)
        }
        Some("rotate") => (0.0, 0.0, 1.0, 1.0, diff, 1.0),
        _ => (0.0, 0.0, 1.0, 1.0, 0.0, 1.0),
    }
}
