use super::CoreRuntime;
use crate::render_pipeline::draw::DrawCommand;
use crate::text::render::{ScetweenConfig, TextRenderer};
use asb_interpreter::Event;
use std::collections::HashMap;

impl CoreRuntime {
    pub(super) fn set_text_renderer(&mut self, renderer: Box<dyn TextRenderer>) {
        self.text_renderer = Some(renderer);
    }

    pub(super) fn advance_text(&mut self, delta_ms: u64) {
        if let Some(renderer) = self.text_renderer.as_mut() {
            renderer.advance_reveal(delta_ms);
        }
    }

    pub(super) fn build_text_commands(&mut self) -> HashMap<String, Vec<DrawCommand>> {
        let Some(renderer) = self.text_renderer.as_mut() else {
            return HashMap::new();
        };
        // 兜底揭示：本帧可能先 advance 再推文本，此时 advance_reveal
        // 已执行但新文本尚未到达，故在渲染前再推进一次（delta=0 只会
        // 把刚推入的首个字符设为可见，不会重复计算已逝时间）。
        renderer.advance_reveal(0);
        renderer.build_text_commands(&mut self.texture_provider)
    }

    pub(super) fn apply_text_event(&mut self, event: &Event) {
        let Some(renderer) = self.text_renderer.as_mut() else {
            return;
        };
        match event {
            Event::ScenarioText { content, inline } => renderer.push_text(content, *inline),
            Event::FontSettings(settings) => renderer.apply_font_settings(settings),
            Event::FontInit => renderer.font_init(),
            Event::FontClose => renderer.font_pop(),
            Event::FontDefault(settings) => renderer.font_default(settings),
            Event::MessageLayerSwitch { id, .. } => {
                if let Some(layer_id) = id {
                    self.compositor.ensure_layer(layer_id);
                }
                renderer.switch_message_layer(id.as_deref());
            }
            Event::MessageLayerPop => renderer.pop_message_layer(),
            Event::LineBreak => renderer.push_line_break(),
            Event::PageBreak { backlog } => renderer.push_page_break(*backlog),
            Event::GlyphConfig(config) => renderer.set_glyph_config(config),
            Event::TextAnimation(params) => {
                renderer.set_scetween(ScetweenConfig::from_params(params));
            }
            Event::SceneIn => renderer.show_text(),
            Event::SceneOut => renderer.hide_text(),
            _ => {}
        }
    }
}
