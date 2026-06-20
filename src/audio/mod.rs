//! 音频子系统。
//!
//! 消费解释器发出的音频相关事件（`splay`、`sstop`、`seplay`、`sestop` 等），
//! 维护 BGM / SE / Voice 三组声音通道，管理音量与声像的平滑过渡（淡入淡出），
//! 并在声音播放完成时触发完成事件处理器。
//!
//! ## 模块
//! - [`engine`]：`AudioBackend` trait、`AudioState`、播放配置类型、淡出逻辑
//! - [`stub`]：`StubAudioBackend` — 不产生任何音频输出的存根实现
//!
//! ## 典型接入方式
//! 1. 后端实现 [`engine::AudioBackend`]（或使用内置的 [`StubAudioBackend`]）。
//! 2. 在帧循环中把解释器事件转发给 `AudioBackend` 的对应方法。
//! 3. 每帧调用 `AudioBackend::advance()` 推进淡入淡出。
//! 4. 每帧调用 `AudioBackend::poll_finish_events()` 获取声音完成事件
//!    并交回解释器执行 handler。
//!
//! ## 计划中的后端
//! - 基于 [`rodio`] crate 的完整实现（`audio-backend` feature，待接入）
//!
//! [`rodio`]: https://crates.io/crates/rodio

pub mod engine;
pub mod stub;

pub use engine::{
    AudioBackend, AudioState, BgmConfig, FadeState, SeConfig, SoundCategory, SoundChannel,
    SoundFinishEvent, SoundFinishHandler,
};
pub use stub::StubAudioBackend;
