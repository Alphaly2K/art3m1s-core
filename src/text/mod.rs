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
//! [`DrawCommand`]: crate::compositor::DrawCommand

pub mod glyph;
pub mod inject;
pub mod render;
pub mod stub;

pub use glyph::GlyphTextRenderer;
pub use inject::TextInject;
pub use render::{
    FontDesc, FontMetrics, FontState, GlyphInfo, MessageLayer, TextAlignment, TextRenderer,
};
pub use stub::StubTextRenderer;
