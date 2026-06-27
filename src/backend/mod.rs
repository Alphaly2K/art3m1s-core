//! 渲染后端。
//!
//! 后端执行 [`crate::render_pipeline`] 选出的 pass 与 draw list。当前提供基于
//! glow 的 GLES 后端 [`gl`]（ANGLE 目标），它不拥有窗口/GL 上下文——上下文由宿主
//! 创建：生产环境用 winit + glutin 接 ANGLE 的 EGL，测试用离屏 CGL 上下文。

#[cfg(feature = "gl-backend")]
pub mod gl;
