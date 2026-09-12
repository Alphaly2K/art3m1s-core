//! 音频子系统。
//!
//! 消费解释器发出的音频相关事件（`splay`、`sstop`、`seplay`、`sestop` 等），
//! 维护 BGM / SE / Voice 三组声音通道，管理音量与声像的平滑过渡（淡入淡出），
//! 并在声音播放完成时触发完成事件处理器。
//!
//! ## 模块
//! - [`engine`]：`AudioBackend` trait、`AudioState`、播放配置类型、淡出逻辑
//! - [`state`]：`AudioStateBackend` — 逻辑状态实现
//!
//! ## 目标接入方式
//! 1. Core 在帧循环中把解释器事件归约到 runtime media session。
//! 2. Runtime 负责解码并生成 PCM，宿主负责真实音频设备输出。
//! 3. Host 通过拉取接口消费 PCM，并把播放时间反馈给 runtime。
//!
//! 当前 [`AudioStateBackend`] 仍维护逻辑状态，真实播放由宿主媒体命令执行；
//! 迁移期结束后，音频设备输出仍固定于宿主侧。

pub mod engine;
pub mod state;

pub use engine::{
    AudioBackend, AudioState, BgmConfig, FadeState, SeConfig, SoundCategory, SoundChannel,
    SoundFinishEvent, SoundFinishHandler,
};
pub use state::AudioStateBackend;
