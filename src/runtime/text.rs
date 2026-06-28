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
        let skip_active = self.skip_active();
        let was_skipping = self.was_skipping();
        let mut reveal_complete = false;
        if let Some(renderer) = self.text_renderer.as_mut() {
            renderer.advance_reveal(delta_ms);
            if skip_active {
                renderer.reveal_all();
            } else if was_skipping {
                renderer.reveal_all();
                reveal_complete = renderer.is_reveal_complete();
            }
        }
        if was_skipping && reveal_complete {
            self.clear_was_skipping();
        }
    }

    pub(super) fn reveal_text_now(&mut self) {
        if let Some(renderer) = self.text_renderer.as_mut() {
            renderer.reveal_all();
        }
    }

    pub(super) fn is_text_reveal_complete(&self) -> bool {
        self.text_renderer
            .as_ref()
            .map(|renderer| renderer.is_reveal_complete())
            .unwrap_or(true)
    }

    pub(super) fn build_text_commands(&mut self) -> HashMap<String, Vec<DrawCommand>> {
        let Some(renderer) = self.text_renderer.as_mut() else {
            return HashMap::new();
        };
        // 不再在这里调 advance_reveal(0)——那会把 reveal_index 重置为 1。
        // advance_reveal 只在 advance_text 里每帧调一次。
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
