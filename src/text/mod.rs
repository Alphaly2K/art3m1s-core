//! 字体渲染子系统。
//!
//! 消费解释器发出的文本相关事件（[`ScenarioText`]、[`FontSettings`]、
//! [`MessageLayerSwitch`] 等），维护字体状态，并为每一段文本产出可加入绘制列表
//! 的字形四边形（[`DrawCommand`]）。
//!
//! ## 模块
//! - [`render`]：`TextRenderer` trait、字体状态、字形生成。
//! - [`inject`]：文本注入 trait，供汉化补丁等外部系统替换或修改文本内容。
//!
//! ## 典型接入方式
//! 1. 后端实现 [`render::TextRenderer`]。
//! 2. 在帧循环中把解释器的文本事件转发给 `TextRenderer` 对应方法。
//! 3. 每帧调用 `TextRenderer::build_draw_commands()` 获取文本层字形并注入绘制列表。
//!
//! [`ScenarioText`]: asb_interpreter::event::Event::ScenarioText
//! [`FontSettings`]: asb_interpreter::event::Event::FontSettings
//! [`MessageLayerSwitch`]: asb_interpreter::event::Event::MessageLayerSwitch
//! [`DrawCommand`]: crate::render_pipeline::draw::DrawCommand

pub mod backlog;
pub mod glyph;
pub mod inject;
pub mod render;

pub use backlog::{Backlog, BacklogPage, BacklogSettings, BacklogTag};
pub use glyph::GlyphTextRenderer;
pub use inject::{FfiTextInject, InjectionChain, TextInject};
pub use render::{
    FontDesc, FontMetrics, FontState, GlyphInfo, LinkHitArea, LinkRange, MessageLayer, RubyRange,
    ScetweenConfig, ScetweenMode, ScetweenSetMode, TextAlignment, TextRenderer, TextSpanToken,
};

pub(crate) fn message_layer_scene_id(message_id: &str, layered: bool) -> String {
    if layered {
        return message_id.to_string();
    }
    use std::fmt::Write;
    let mut encoded = String::from(crate::compositor::scene::MESSAGE_LAYER_OVERLAY_PREFIX);
    for byte in message_id.as_bytes() {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

pub(crate) fn font_fallback_candidates(face: &str) -> Vec<String> {
    let (stem, extension) = face.rsplit_once('.').unwrap_or((face, ""));
    let lower = stem.to_ascii_lowercase();
    for separator in ['-', '_'] {
        let suffix = format!("{separator}medium");
        if lower.ends_with(&suffix) {
            let family = &stem[..stem.len() - suffix.len()];
            let extension = if extension.is_empty() {
                String::new()
            } else {
                format!(".{extension}")
            };
            return ["regular", "bold"]
                .into_iter()
                .map(|weight| format!("{family}{separator}{weight}{extension}"))
                .collect();
        }
    }
    Vec::new()
}

pub(crate) fn capture_message_text(state: &FontState) -> Option<crate::save::MessageTextSnapshot> {
    use crate::save::{MessageLayerTextSnapshot, MessageTextSnapshot};

    let mut layers: Vec<_> = state
        .layers
        .iter()
        .filter(|(_, layer)| !layer.page_tags.is_empty())
        .map(|(id, layer)| MessageLayerTextSnapshot {
            id: id.clone(),
            page_font: layer.page_font.clone(),
            tags: layer.page_tags.clone(),
        })
        .collect();
    if layers.is_empty() {
        return None;
    }
    layers.sort_by(|a, b| a.id.cmp(&b.id));
    Some(MessageTextSnapshot {
        active_layer: state.active_layer.clone(),
        layers,
    })
}
