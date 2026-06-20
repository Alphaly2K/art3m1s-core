//! 存根文本渲染器。
//!
//! 在真实字体后端就绪前用于编译/测试，不产生任何绘制输出。

use crate::compositor::renderer::{DrawCommand, TextureId};
use crate::text::render::{FontState, GlyphInfo, ScetweenConfig, TextRenderer};
use std::collections::HashMap;

/// 不绘制任何文本的存根渲染器，仅记录最后收到的文本。
#[derive(Debug, Default)]
pub struct StubTextRenderer {
    state: FontState,
    /// 最后收到的文本内容
    pub last_text: Option<String>,
}

impl StubTextRenderer {
    pub fn new() -> Self {
        Self::default()
    }
}

impl TextRenderer for StubTextRenderer {
    fn apply_font_settings(&mut self, settings: &HashMap<String, String>) {
        let layer = self.state.active_layer_mut();
        let stacked = settings
            .get("stack")
            .map(|v| matches!(v.as_str(), "1" | "true"))
            .unwrap_or(true);
        if stacked {
            layer.font_stack.push(layer.font.clone());
        }
        layer.font.merge_raw(settings);
    }

    fn font_init(&mut self) {
        let default = self.state.default_font.clone();
        let layer = self.state.active_layer_mut();
        layer.font = default;
    }

    fn font_pop(&mut self) {
        let layer = self.state.active_layer_mut();
        if let Some(saved) = layer.font_stack.pop() {
            layer.font = saved;
        }
    }

    fn font_default(&mut self, settings: &HashMap<String, String>) {
        self.state.default_font.merge_raw(settings);
    }

    fn switch_message_layer(&mut self, id: Option<&str>) {
        if let Some(ref prev) = self.state.active_layer {
            self.state.layer_stack.push(prev.clone());
        }
        self.state.active_layer = id.map(|s| s.to_string());
    }

    fn pop_message_layer(&mut self) {
        if let Some(prev) = self.state.layer_stack.pop() {
            self.state.active_layer = Some(prev);
        }
    }

    fn set_glyph_config(&mut self, config: &HashMap<String, String>) {
        self.state.glyph_config.clone_from(config);
    }

    fn push_text(&mut self, content: &str, _inline: bool) {
        self.last_text = Some(content.to_string());
        let layer = self.state.active_layer_mut();
        // 为每个字符添加一个虚拟字形（存根不实际渲染，但需要字形计数供逐字显示）
        for c in content.chars() {
            layer.text_buffer.push(GlyphInfo {
                character: c.to_string(),
                texture_id: TextureId(0),
                atlas_x: 0.0,
                atlas_y: 0.0,
                atlas_w: 0.0,
                atlas_h: 0.0,
                offset_x: 0.0,
                offset_y: 0.0,
                width: 0.0,
                height: 0.0,
                advance_x: 0.0,
            });
        }
        layer.reveal_pending = true;
        layer.reveal_clock_ms = 0;
    }

    fn push_line_break(&mut self) {}

    fn push_page_break(&mut self, _backlog: Option<i32>) {
        let layer = self.state.active_layer_mut();
        layer.text_buffer.clear();
        layer.reveal_index = 0;
        layer.reveal_pending = false;
    }

    fn build_text_commands(
        &mut self,
        _provider: &mut dyn crate::compositor::renderer::TextureProvider,
    ) -> HashMap<String, Vec<DrawCommand>> {
        HashMap::new()
    }

    // ── 逐字显示 ──

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

            let is_entrance = layer
                .scetween
                .as_ref()
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
                    let chars_revealed = (layer.reveal_clock_ms / cfg.delay_per_char) as usize + 1;
                    layer.reveal_index = chars_revealed.min(char_count);
                    if layer.reveal_index >= char_count {
                        let last_char_start =
                            (char_count.saturating_sub(1) as u64) * cfg.delay_per_char;
                        if cfg.time_per_char > 0
                            && layer.reveal_clock_ms < last_char_start + cfg.time_per_char
                        {
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
                return !layer.reveal_pending && layer.reveal_index >= layer.text_buffer.len();
            }
        }
        true
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
    use super::*;
    use crate::text::render::ScetweenMode;

    #[test]
    fn advance_reveal_no_scetween_reveals_all() {
        let mut s = StubTextRenderer::new();
        s.push_text("Hello", false);
        assert!(!s.is_reveal_complete());

        s.advance_reveal(0);
        assert!(s.is_reveal_complete());
        let layer = s.font_state().layers.get("adv01").unwrap();
        assert_eq!(layer.reveal_index, 5);
    }

    #[test]
    fn advance_reveal_with_scetween_delay() {
        let mut s = StubTextRenderer::new();
        s.push_text("ABC", false);
        s.set_scetween(ScetweenConfig {
            mode: ScetweenMode::In,
            delay_per_char: 100,
            ..Default::default()
        });

        s.advance_reveal(50);
        assert!(!s.is_reveal_complete());

        s.advance_reveal(100);
        assert!(!s.is_reveal_complete());

        s.advance_reveal(200);
        assert!(s.is_reveal_complete());
        assert_eq!(s.font_state().layers["adv01"].reveal_index, 3);
    }

    #[test]
    fn reveal_all_immediately_shows_all() {
        let mut s = StubTextRenderer::new();
        s.push_text("Hello, World!", false);
        assert!(!s.is_reveal_complete());

        s.reveal_all();
        assert!(s.is_reveal_complete());
        assert_eq!(s.font_state().layers["adv01"].reveal_index, 13);
    }

    #[test]
    fn hide_and_show_text() {
        let mut s = StubTextRenderer::new();
        s.push_text("Test", false);
        s.reveal_all();

        s.hide_text();
        assert_eq!(s.font_state().layers["adv01"].reveal_index, 0);

        s.show_text();
        assert!(s.font_state().layers["adv01"].reveal_pending);
    }
}
