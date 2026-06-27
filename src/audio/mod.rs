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
//! ## 典型接入方式
//! 1. Core 在帧循环中把解释器事件归约到 [`AudioState`]。
//! 2. 前端/宿主通过 runtime 的媒体命令回调执行真实播放。
//! 3. Core 每帧推进淡入淡出状态，并产出声音完成 handler。
//!    并交回解释器执行 handler。

pub mod engine;
pub mod state;

pub use engine::{
    AudioBackend, AudioState, BgmConfig, FadeState, SeConfig, SoundCategory, SoundChannel,
    SoundFinishEvent, SoundFinishHandler,
};
pub use state::AudioStateBackend;
