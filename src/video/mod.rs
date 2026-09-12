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
//! ## 目标接入方式
//! 1. Core 在帧循环中把解释器的视频事件归约到 runtime media session。
//! 2. Runtime 持有 FFmpeg/平台解码器、PTS 和完成事件。
//! 3. Runtime 把视频合成进宿主提供的最终呈现目标；宿主仍拥有 surface/present。
//!
//! 当前 [`VideoStateBackend`] 仍是迁移期的逻辑状态实现，解码接入将在
//! [`crate::media`] 的契约上逐步替换。

pub mod engine;
pub mod state;

pub use engine::{
    VideoBackend, VideoConfig, VideoFinishEvent, VideoFinishHandler, VideoState,
    is_video_layer_texture_name, video_layer_texture_name,
};
pub use state::VideoStateBackend;
