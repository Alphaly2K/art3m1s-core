//! 存根文本渲染器。
//!
//! 在真实字体后端就绪前用于编译/测试，不产生任何绘制输出。

use crate::compositor::renderer::DrawCommand;
use crate::text::render::{FontState, TextRenderer};
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
    }

    fn push_line_break(&mut self) {}

    fn push_page_break(&mut self, _backlog: Option<i32>) {
        // 清空当前层文本
        let layer = self.state.active_layer_mut();
        layer.text_buffer.clear();
    }

    fn build_text_commands(
        &mut self,
        _provider: &mut dyn crate::compositor::renderer::TextureProvider,
    ) -> HashMap<String, Vec<DrawCommand>> {
        HashMap::new()
    }

    fn font_state(&self) -> &FontState {
        &self.state
    }

    fn font_state_mut(&mut self) -> &mut FontState {
        &mut self.state
    }
}
