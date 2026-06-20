//! 基于 ab_glyph 的字形光栅化文本渲染器。

use crate::compositor::renderer::{
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
    fn new() -> Self { Self { rows: vec![], cur: vec![], px: vec![0; (ATLAS_SZ*ATLAS_SZ*4) as usize], dirty: false } }
    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        for (i, &(oy, oh)) in self.rows.iter().enumerate() {
            if oh >= h && self.cur[i] + w <= ATLAS_SZ { let x = self.cur[i]; self.cur[i] += w; return Some((x, oy)); }
        }
        let y: u32 = self.rows.last().map(|(y,h)| y+h).unwrap_or(0);
        if y + h > ATLAS_SZ { return None; }
        self.rows.push((y, h)); self.cur.push(w);
        Some((0, y))
    }
    fn write(&mut self, x: u32, y: u32, w: u32, h: u32, rgba: &[u8]) {
        self.dirty = true;
        for r in 0..h as usize {
            let doff = ((y as usize+r)*ATLAS_SZ as usize + x as usize)*4;
            let soff = r * w as usize * 4;
            let len = (w as usize*4).min(rgba.len()-soff).min(self.px.len()-doff);
            self.px[doff..doff+len].copy_from_slice(&rgba[soff..soff+len]);
        }
    }
    fn flush(&mut self, p: &mut dyn TextureProvider) -> (TextureId, TextureInfo) {
        if self.dirty {
            if let Some(r) = p.upload_rgba(ATLAS_NAME, ATLAS_SZ, ATLAS_SZ, &self.px) { self.dirty = false; return r; }
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

fn scaled<'a>(font: &'a Option<FontRef<'a>>, scale: PxScale) -> Option<PxScaleFont<&'a FontRef<'a>>> {
    font.as_ref().map(|f| f.as_scaled(scale))
}

fn parse(s: &str) -> [f32; 3] {
    let h = s.trim().trim_start_matches("0x").trim_start_matches('#');
    if h.len() >= 6 {
        [u8::from_str_radix(&h[0..2],16).unwrap_or(255) as f32/255.0,
         u8::from_str_radix(&h[2..4],16).unwrap_or(255) as f32/255.0,
         u8::from_str_radix(&h[4..6],16).unwrap_or(255) as f32/255.0]
    } else { [1.0; 3] }
}

impl<'font> GlyphTextRenderer<'font> {
    pub fn new() -> Self {
        Self { state: FontState::new(), font: None, atlas: Atlas::new(), cache: HashMap::new() }
    }
    pub fn set_font(&mut self, bytes: &'font [u8]) -> Result<(), String> {
        self.font = Some(FontRef::try_from_slice(bytes).map_err(|e| format!("{e}"))?);
        Ok(())
    }
}

impl TextRenderer for GlyphTextRenderer<'_> {
    fn apply_font_settings(&mut self, s: &HashMap<String, String>) {
        let l = self.state.active_layer_mut();
        l.font.merge_raw(s);
        if let Some(v) = s.get("left").and_then(|v| v.parse().ok()) { l.left = v; }
        if let Some(v) = s.get("top").and_then(|v| v.parse().ok()) { l.top = v; }
        if let Some(v) = s.get("width").and_then(|v| v.parse().ok()) { l.width = v; }
        if let Some(v) = s.get("height").and_then(|v| v.parse().ok()) { l.height = v; }
    }
    fn font_init(&mut self) {
        let d = self.state.default_font.clone();
        self.state.active_layer_mut().font = d;
    }
    fn font_pop(&mut self) {
        let l = self.state.active_layer_mut();
        if let Some(v) = l.font_stack.pop() { l.font = v; }
    }
    fn font_default(&mut self, s: &HashMap<String, String>) { self.state.default_font.merge_raw(s); }
    fn switch_message_layer(&mut self, id: Option<&str>) {
        let prev_state = self.state.active_layer.as_ref().and_then(|aid| {
            self.state.layers.get(aid).map(|l| (l.left, l.top, l.width, l.height, l.font.clone()))
        });
        if let Some(ref prev_id) = self.state.active_layer {
            self.state.layer_stack.push(prev_id.clone());
        }
        self.state.active_layer = id.map(|s| s.to_string());
        let layer = self.state.active_layer_mut();
        if let Some((left, top, width, height, font)) = prev_state {
            if layer.left == 0.0 && layer.top == 0.0 {
                layer.left = left; layer.top = top;
                layer.width = width; layer.height = height;
                layer.font = font;
            }
        }
        layer.text_buffer.clear();
    }

    fn pop_message_layer(&mut self) {
        if let Some(prev) = self.state.layer_stack.pop() {
            self.state.active_layer = Some(prev);
        }
    }
    fn set_glyph_config(&mut self, c: &HashMap<String, String>) { self.state.glyph_config.clone_from(c); }

    fn push_text(&mut self, content: &str, _inline: bool) {
        let layer = self.state.active_layer_mut();
        let sz = layer.font.size.unwrap_or(40.0);
        let scale = PxScale::from(sz);
        let sf = scaled(&self.font, scale);
        let sf = match sf { Some(s) => s, None => return };
        // 新文本到来时标记待揭示
        layer.reveal_pending = true;
        layer.reveal_clock_ms = 0;

        for c in content.chars() {
            let q = sf.outline_glyph(sf.glyph_id(c).with_scale(sz));
            if let Some(q) = q {
                let b = q.px_bounds();
                let w = b.width().ceil() as u32;
                let h = b.height().ceil() as u32;
                let (ax, ay, aw, ah) = if w > 0 && h > 0 && w < ATLAS_SZ && h < ATLAS_SZ {
                    let k = (sf.glyph_id(c).0, sz as u32);
                    *self.cache.entry(k).or_insert_with(|| {
                        if let Some((x, y)) = self.atlas.alloc(w+1, h+1) {
                            let mut g = vec![0u8; (w*h) as usize];
                            q.draw(|px, py, v| { let ix = py as usize * w as usize + px as usize; if ix < g.len() { g[ix] = (v*255.0) as u8; } });
                            let rgba: Vec<u8> = g.iter().flat_map(|&a| [255u8,255,255,a]).collect();
                            self.atlas.write(x, y, w, h, &rgba);
                            (x, y, w, h)
                        } else { (0, 0, 0, 0) }
                    })
                } else { (0, 0, 0, 0) };
                layer.text_buffer.push(GlyphInfo {
                    character: c.to_string(), texture_id: TextureId(0),
                    atlas_x: ax as f32, atlas_y: ay as f32, atlas_w: aw as f32, atlas_h: ah as f32,
                    offset_x: b.min.x, offset_y: sf.ascent() + b.min.y,
                    width: w as f32, height: h as f32,
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
        let sf = match sf { Some(s) => s, None => return };
        layer.text_buffer.push(GlyphInfo {
            character: "\n".into(), texture_id: TextureId(0),
            atlas_x: 0.0, atlas_y: 0.0, atlas_w: 0.0, atlas_h: 0.0,
            offset_x: 0.0, offset_y: sf.height(), width: 0.0, height: 0.0, advance_x: 0.0,
        });
    }

    fn push_page_break(&mut self, _bl: Option<i32>) {
        let lid = self.state.active_layer.clone().unwrap_or_default();
        if let Some(l) = self.state.layers.get_mut(&lid) {
            l.text_buffer.clear();
            l.reveal_index = 0;
            l.reveal_pending = false;
        }
    }

    fn build_text_commands(&mut self, p: &mut dyn TextureProvider) -> HashMap<String, Vec<DrawCommand>> {
        let (tex, _) = self.atlas.flush(p);
        let mut out: HashMap<String, Vec<DrawCommand>> = HashMap::new();

        let lids: Vec<String> = self.state.layers.keys().cloned().collect();
        for lid in &lids {
            let ly = match self.state.layers.get(lid) { Some(l) => l.clone(), None => continue };
            if ly.text_buffer.is_empty() { continue; }

            // 确定本帧可见的字形范围：有 scetween 就走逐字揭示（全局配置），
            // 无 scetween 则完整渲染。
            let visible_count = if ly.scetween.is_some() {
                ly.reveal_index.min(ly.text_buffer.len())
            } else {
                ly.text_buffer.len()
            };
            let scetween = &ly.scetween;

            let sz = ly.font.size.unwrap_or(40.0);
            let scale = PxScale::from(sz);
            let sf = scaled(&self.font, scale);
            let sf = match sf { Some(s) => s, None => continue };
            let lh = sf.height();
            let lw = if ly.width > 0.0 { ly.width } else { f32::MAX };

            let color = ly.font.color.as_deref().map(parse).unwrap_or([1.0; 3]);
            let oc = ly.font.outline_color.as_deref().map(parse).unwrap_or([0.0, 0.0, 0.0]);
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

                if g.character == "\n" { cx = 0.0; line_y += lh; ls = i + 1; continue; }
                if cx + g.width > lw && i > ls { cx = 0.0; line_y += lh; ls = i; }

                let fx = ly.left + cx + g.offset_x;
                let fy = ly.top + g.offset_y + line_y;

                // 计算每字符的 scetween 动画偏移
                let anim_offset = scetween_char_offset(
                    scetween.as_ref(),
                    i,
                    ly.reveal_clock_ms,
                );

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
                        let from_center = Affine2::from_translation(Vec2::new(-cx_center, -cy_center));
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
                        size: TextureInfo { width: ATLAS_SZ, height: ATLAS_SZ },
                        transform: base_transform,
                        opacity: char_alpha,
                        blend: BlendMode::Alpha,
                        color: ColorFilter { multiply: color, grayscale: false, negative: false },
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
        layer.scetween = Some(config);
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
            if !layer.reveal_pending {
                continue;
            }
            layer.reveal_clock_ms = layer.reveal_clock_ms.saturating_add(delta_ms);

            let is_entrance = layer.scetween.as_ref()
                .map(|cfg| cfg.mode.is_entrance())
                .unwrap_or(true);

            if !is_entrance {
                layer.reveal_index = layer.text_buffer.len();
                layer.reveal_pending = false;
                continue;
            }

            if let Some(ref cfg) = layer.scetween {
                let char_count = layer.text_buffer.len();
                if char_count == 0 {
                    layer.reveal_pending = false;
                    continue;
                }
                if cfg.delay_per_char == 0 {
                    layer.reveal_index = char_count;
                    if cfg.time_per_char > 0 && layer.reveal_clock_ms >= cfg.time_per_char {
                        layer.reveal_pending = false;
                    } else if cfg.time_per_char == 0 {
                        layer.reveal_pending = false;
                    }
                } else {
                    let chars_revealed =
                        (layer.reveal_clock_ms / cfg.delay_per_char) as usize + 1;
                    layer.reveal_index = chars_revealed.min(char_count);
                    if layer.reveal_index >= char_count {
                        // 全部字符已揭示，但最后一个字符的出场动画可能还在播放
                        let last_char_start =
                            (char_count.saturating_sub(1) as u64) * cfg.delay_per_char;
                        if cfg.time_per_char > 0
                            && layer.reveal_clock_ms < last_char_start + cfg.time_per_char
                        {
                            // 继续标记为 pending 以播放动画
                        } else {
                            layer.reveal_pending = false;
                        }
                    }
                }
            } else {
                layer.reveal_index = layer.text_buffer.len();
                layer.reveal_pending = false;
            }
        }
    }

    fn reveal_all(&mut self) {
        let layer = self.state.active_layer_mut();
        layer.reveal_index = layer.text_buffer.len();
        layer.reveal_pending = false;
    }

    fn hide_text(&mut self) {
        let layer = self.state.active_layer_mut();
        layer.reveal_index = 0;
    }

    fn show_text(&mut self) {
        let layer = self.state.active_layer_mut();
        layer.reveal_pending = true;
    }

    fn is_reveal_complete(&self) -> bool {
        if let Some(ref active) = self.state.active_layer {
            if let Some(layer) = self.state.layers.get(active) {
                return !layer.reveal_pending
                    && layer.reveal_index >= layer.text_buffer.len();
            }
        }
        true
    }

    fn font_state(&self) -> &FontState { &self.state }
    fn font_state_mut(&mut self) -> &mut FontState { &mut self.state }
}

/// 计算单个字符的 scetween 动画偏移量。
///
/// 返回 `(offset_x, offset_y, scale_x, scale_y, rotate_degrees, alpha)`，
/// 其中位置偏移为像素值，缩放为倍数（1.0=无缩放），alpha 为 0.0-1.0。
fn scetween_char_offset(
    cfg: Option<&ScetweenConfig>,
    char_index: usize,
    reveal_clock_ms: u64,
) -> (f32, f32, f32, f32, f32, f32) {
    let cfg = match cfg {
        Some(c) => c,
        None => return (0.0, 0.0, 1.0, 1.0, 0.0, 1.0),
    };

    let char_start_ms = char_index as u64 * cfg.delay_per_char;
    if reveal_clock_ms < char_start_ms {
        // 尚未到达该字符的动画开始时间
        return match cfg.mode {
            crate::text::render::ScetweenMode::In
            | crate::text::render::ScetweenMode::Show
            | crate::text::render::ScetweenMode::BacklogDownIn
            | crate::text::render::ScetweenMode::BacklogUpIn => {
                // 入场动画：字符从动画起点开始
                scetween_start_value(cfg)
            }
            _ => (0.0, 0.0, 1.0, 1.0, 0.0, 1.0),
        };
    }

    let elapsed = reveal_clock_ms - char_start_ms;
    let time = if cfg.time_per_char > 0 {
        cfg.time_per_char
    } else {
        return (0.0, 0.0, 1.0, 1.0, 0.0, 1.0);
    };

    if elapsed >= time {
        return (0.0, 0.0, 1.0, 1.0, 0.0, 1.0);
    }

    let t = elapsed as f32 / time as f32;
    let progress = cfg.ease.apply(t);

    // 从起始值到正常值的插值
    let (start_x, start_y, start_sx, start_sy, start_r, start_a) =
        scetween_start_value(cfg);

    let end_x: f32 = 0.0;
    let end_y: f32 = 0.0;
    let end_sx: f32 = 1.0;
    let end_sy: f32 = 1.0;
    let end_r: f32 = 0.0;
    let end_a: f32 = 1.0;

    (
        start_x + (end_x - start_x) * progress,
        start_y + (end_y - start_y) * progress,
        start_sx + (end_sx - start_sx) * progress,
        start_sy + (end_sy - start_sy) * progress,
        start_r + (end_r - start_r) * progress,
        start_a + (end_a - start_a) * progress,
    )
}

/// 根据 scetween 配置计算动画的起始值。
fn scetween_start_value(cfg: &ScetweenConfig) -> (f32, f32, f32, f32, f32, f32) {
    let diff = cfg.diff.unwrap_or(0.0);
    match cfg.param.as_deref() {
        Some("left") => (diff, 0.0, 1.0, 1.0, 0.0, 1.0),
        Some("top") => (0.0, diff, 1.0, 1.0, 0.0, 1.0),
        Some("alpha") => {
            let start_a = if cfg.mode.is_entrance() { 0.0 } else { 1.0 };
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
