//! 基于 ab_glyph 的字形光栅化文本渲染器。

use crate::compositor::renderer::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, TextureId, TextureInfo, TextureProvider,
};
use crate::text::render::{FontState, GlyphInfo, TextRenderer};
use ab_glyph::{Font, FontRef, PxScale, PxScaleFont, ScaleFont};
use glam::Affine2;
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
        // 清理由 switch_message_layer（新层）和 push_page_break（翻页）负责，
        // push_text 累加到当前缓冲。

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
        if let Some(l) = self.state.layers.get_mut(&lid) { l.text_buffer.clear(); }
    }

    fn build_text_commands(&mut self, p: &mut dyn TextureProvider) -> HashMap<String, Vec<DrawCommand>> {
        let (tex, _) = self.atlas.flush(p);
        let mut out: HashMap<String, Vec<DrawCommand>> = HashMap::new();

        let lids: Vec<String> = self.state.layers.keys().cloned().collect();
        for lid in &lids {
            let ly = match self.state.layers.get(lid) { Some(l) => l.clone(), None => continue };
            if ly.text_buffer.is_empty() { continue; }
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
                if g.character == "\n" { cx = 0.0; line_y += lh; ls = i + 1; continue; }
                if cx + g.width > lw && i > ls { cx = 0.0; line_y += lh; ls = i; }
                let fx = ly.left + cx + g.offset_x;
                let fy = ly.top + g.offset_y + line_y;
                if g.atlas_w > 0.0 && g.atlas_h > 0.0 {
                    let clip = ClipRect {
                        uv_offset: [g.atlas_x / ATLAS_SZ as f32, g.atlas_y / ATLAS_SZ as f32],
                        uv_scale: [g.atlas_w / ATLAS_SZ as f32, g.atlas_h / ATLAS_SZ as f32],
                        quad_size: [g.atlas_w, g.atlas_h],
                    };
                    let base = DrawCommand {
                        texture: tex,
                        size: TextureInfo { width: ATLAS_SZ, height: ATLAS_SZ },
                        transform: Affine2::from_translation(glam::Vec2::new(fx, fy)),
                        opacity: 1.0, blend: BlendMode::Alpha,
                        color: ColorFilter { multiply: color, grayscale: false, negative: false },
                        clip: clip.clone(),
                    };
                    if has_shadow {
                        let mut sc = base;
                        sc.color.multiply = oc;
                        sc.transform = Affine2::from_translation(glam::Vec2::new(fx + 2.0, fy + 2.0));
                        v.push(sc);
                    }
                    if has_outline {
                        for &(ox, oy) in &OUTLINE_OFFSETS {
                            let mut ocp = base;
                            ocp.color.multiply = oc;
                            ocp.transform = Affine2::from_translation(glam::Vec2::new(fx + ox, fy + oy));
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

    fn font_state(&self) -> &FontState { &self.state }
    fn font_state_mut(&mut self) -> &mut FontState { &mut self.state }
}
