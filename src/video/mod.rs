//! 视频子系统。
//!
//! 消费解释器发出的视频相关事件（`video`、`setonvideofinish`、`delonvideofinish`），
//! 支持全屏视频和视频图层两种模式。
//!
//! ## 模块
//! - [`engine`]：`VideoBackend` trait、`VideoState`、播放配置类型
//! - [`stub`]：`StubVideoBackend` — 不产生任何视频输出的存根实现
//! - [`ffmpeg`]：`FfmpegBackend` — 基于 FFmpeg 的视频解码实现（`video-backend` feature）
//!
//! ## 视频模式
//! 1. **全屏视频**（`id=None`）：视频直接渲染到整个舞台，不创建图层
//! 2. **视频图层**（`id=Some(...)`）：视频作为图层渲染，支持图层属性
//!
//! ## 典型接入方式
//! 1. 后端实现 [`engine::VideoBackend`]（或使用内置的 [`StubVideoBackend`] / [`FfmpegBackend`]）
//! 2. 在帧循环中把解释器的视频事件转发给 `VideoBackend` 的对应方法
//! 3. 每帧调用 `VideoBackend::advance()` 推进视频解码
//! 4. 每帧调用 `VideoBackend::poll_finish_events()` 获取视频完成事件
//!    并交回解释器执行 handler
//!
//! ## 视频格式支持
//! - **全屏视频**：平台相关（Windows: MPEG-1/WMV, iOS/Android: MPEG-4）
//! - **视频图层**：Ogg Theora (.ogv) 或 Motion-JPEG with Alpha (.mja)
//! - 支持 Alpha 通道（通过 `_m` 后缀的灰度视频）

pub mod engine;
pub mod stub;

#[cfg(feature = "video-backend")]
pub mod ffmpeg;

pub use engine::{VideoBackend, VideoConfig, VideoFinishEvent, VideoFinishHandler, VideoState};
pub use stub::StubVideoBackend;

#[cfg(feature = "video-backend")]
pub use ffmpeg::FfmpegBackend;
