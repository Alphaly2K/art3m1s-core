//! 后端无关的画面合成器。
//!
//! 合成器消费 [`asb_interpreter`] 发出的事件，维护一棵保留模式的图层场景树，并在
//! 每一时刻把它构建成一个有序的 [`renderer::DrawList`]。它不依赖任何图形 API：
//! 真正把画面画出来的后端只需实现 [`renderer::Renderer`] 与
//! [`renderer::TextureProvider`] 两个 trait（计划中的 ANGLE 后端即如此接入）。
//!
//! 模块划分：
//! - [`props`]：图层属性的 typed 表示与从原始字符串解析。
//! - [`scene`]：点分层级 ID 的图层树（增删改、子树、继承）。
//! - [`anim`]：属性缓动与缓动函数。
//! - [`renderer`]：合成器与后端之间的边界（trait 与绘制命令）。
//! - [`build`]：把场景树在某时刻压平成绘制列表。
//! - [`reduce`]：把解释器事件归约到场景状态上的 [`Compositor`]。
//! - [`mock`]：测试用的假后端。

pub mod anim;
pub mod build;
pub mod mock;
pub mod props;
pub mod reduce;
pub mod renderer;
pub mod scene;

pub use anim::{Easing, Tween};
pub use props::LayerProps;
pub use reduce::Compositor;
pub use renderer::{
    BlendMode, ColorFilter, DrawCommand, DrawList, Renderer, TextureId, TextureInfo,
    TextureProvider,
};
pub use scene::{Layer, Scene};
