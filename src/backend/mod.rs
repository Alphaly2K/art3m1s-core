//! 渲染后端。
//!
//! 后端是合成器 [`crate::compositor::renderer`] 抽象的具体实现，把后端无关的
//! [`DrawList`](crate::compositor::DrawList) 真正画出来。当前提供基于 glow 的
//! GLES 后端 [`gl`]（ANGLE 目标），它不拥有窗口/GL 上下文——上下文由宿主创建：
//! 生产环境用 winit + glutin 接 ANGLE 的 EGL，测试用离屏 CGL 上下文。

#[cfg(feature = "gl-backend")]
pub mod gl;
