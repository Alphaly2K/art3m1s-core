//! 后端无关的画面合成器。
//!
//! 合成器消费视觉/交互事件，维护一棵保留模式的图层场景树。渲染管线不属于
//! [`Compositor`] 本体。需要进入转场捕获、shader pass 或实际绘制时，交给顶层
//! [`crate::render_pipeline::RenderPipeline`]。
//!
//! 模块划分：
//! - [`props`]：图层属性的 typed 表示与从原始字符串解析。
//! - [`scene`]：点分层级 ID 的图层树（增删改、子树、继承）。
//! - [`anim`]：属性缓动与缓动函数。
//! - [`renderer`]：DrawList 数据结构和纹理解析边界。
//! - [`build`]：把场景树在某时刻压平成 DrawList。
//! - [`reduce`]：把解释器事件归约到场景状态上的 [`Compositor`]。
//! - [`transition`]：转场状态与转场捕获/叠加规则。
//! - [`mock`]：测试用的假后端。

pub mod anim;
pub mod build;
pub mod events;
pub mod mock;
pub mod props;
pub mod reduce;
pub mod scene;

pub use crate::render_pipeline::draw::{
    BlendMode, ColorFilter, DrawCommand, DrawList, Renderer, TextureId, TextureInfo,
    TextureProvider,
};
pub use anim::{Easing, Tween, TweenHandler};
pub use events::{CompositorEvent, IntoCompositorEvent};
pub use props::LayerProps;
pub use reduce::Compositor;
pub use scene::{Layer, Scene};
