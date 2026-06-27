//! 视频子系统。
//!
//! 消费解释器发出的视频相关事件（`video`、`setonvideofinish`、`delonvideofinish`），
//! 支持全屏视频和视频图层两种模式。
//!
//! ## 模块
//! - [`engine`]：`VideoBackend` trait、`VideoState`、播放配置类型
//! - [`state`]：`VideoStateBackend` — 逻辑状态实现
//!
//! ## 视频模式
//! 1. **全屏视频**（`id=None`）：视频直接渲染到整个舞台，不创建图层
//! 2. **视频图层**（`id=Some(...)`）：视频作为图层渲染，支持图层属性
//!
//! ## 典型接入方式
//! 1. Core 在帧循环中把解释器的视频事件归约到 [`VideoState`]。
//! 2. Core 发出 host media 命令，前端/宿主选择平台解码器或 fallback。
//! 3. 宿主播放结束后通过 FFI 通知 core，core 再恢复脚本或执行 finish handler。
//!
//! Core 不持有 FFmpeg backend，也不把解码帧存回视频状态。

pub mod engine;
pub mod state;

pub use engine::{VideoBackend, VideoConfig, VideoFinishEvent, VideoFinishHandler, VideoState};
pub use state::VideoStateBackend;
