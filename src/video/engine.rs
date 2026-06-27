//! 视频状态与宿主视频协议。
//!
//! Core does not own an FFmpeg/Theora backend and does not keep decoded video
//! frames.  It keeps script-visible video state and synchronization points, then
//! emits host media commands.  The frontend/host decides whether to use a
//! platform decoder, native fallback, or a texture-producing decoder.
//!
//! ## 与合成器的关系
//! 视频子系统与 [`crate::compositor::Compositor`] 平级。合成器只看到普通图层状态；
//! 宿主侧视频输出不进入 core 的 render pipeline。

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// 播放配置
// ---------------------------------------------------------------------------

/// 视频播放参数。
///
/// 对应 Artemis 的 `video` 标签。
#[derive(Debug, Clone)]
pub struct VideoConfig {
    /// 视频文件路径
    pub file: String,
    /// 是否允许单击跳过（skip=1）
    pub skippable: bool,
    /// 是否循环播放（仅全屏视频有效）
    pub loop_play: bool,
    /// 帧跳过延迟阈值（毫秒），仅视频图层有效
    /// 当解码延迟超过此值时跳过一些帧
    pub delay_margin_ms: Option<i32>,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            file: String::new(),
            skippable: true,
            loop_play: false,
            delay_margin_ms: None,
        }
    }
}

// ---------------------------------------------------------------------------
// 视频完成事件
// ---------------------------------------------------------------------------

/// 视频播放完成时触发的事件处理器注册信息。
///
/// 对应 Artemis 的 `setonvideofinish` 标签。
#[derive(Debug, Clone)]
pub struct VideoFinishHandler {
    /// 跳转/调用目标脚本文件
    pub file: Option<String>,
    /// 跳转/调用目标标签
    pub label: Option<String>,
    /// call=1 时压调用栈，否则等同 jump
    pub call: bool,
    /// 就地执行的标签名（如 `"calllua"`）
    pub handler: Option<String>,
}

/// 视频完成事件：当视频播放结束时产生。
///
/// 宿主在每帧调用 [`VideoBackend::poll_finish_events`] 获取，
/// 然后把它们交还给解释器执行（类似输入事件的 handler 体系）。
#[derive(Debug, Clone)]
pub struct VideoFinishEvent {
    /// 视频图层的 ID（全屏视频时为 None）
    pub id: Option<String>,
    /// 已注册的完成处理器（如果存在）
    pub handler: Option<VideoFinishHandler>,
}

// ---------------------------------------------------------------------------
// 视频图层状态
// ---------------------------------------------------------------------------

/// 单个视频播放通道的状态。
#[derive(Debug, Clone)]
pub struct VideoChannel {
    /// 视频图层 ID（全屏视频时为特殊 ID）
    pub id: String,
    /// 视频文件路径
    pub file: String,
    /// 是否正在播放
    pub playing: bool,
    /// 是否循环播放
    pub loop_play: bool,
    /// 是否允许跳过
    pub skippable: bool,
    /// 当前播放位置（毫秒）
    pub position_ms: u64,
}

impl VideoChannel {
    pub fn new(id: &str, file: &str) -> Self {
        Self {
            id: id.to_string(),
            file: file.to_string(),
            playing: false,
            loop_play: false,
            skippable: true,
            position_ms: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// 全局视频状态
// ---------------------------------------------------------------------------

/// 视频子系统的全局状态。
///
/// 维护所有活跃的视频通道、完成事件处理器注册表以及视频进度所需的内部时钟。
#[derive(Debug, Clone, Default)]
pub struct VideoState {
    /// 当前全屏视频通道（同时只能有一个）
    pub fullscreen_video: Option<VideoChannel>,
    /// 活跃的视频图层通道表（按 ID 索引）
    pub video_layers: HashMap<String, VideoChannel>,
    /// 视频播放完成事件处理器（全局，非图层绑定）
    pub finish_handler: Option<VideoFinishHandler>,
    /// 视频子系统内部时钟（毫秒），用于视频进度计时
    pub clock_ms: u64,
}

// ---------------------------------------------------------------------------
// VideoBackend trait
// ---------------------------------------------------------------------------

/// 视频逻辑状态后端。
///
/// 实现方只维护 core 侧状态和完成事件。真实解码/显示在宿主侧完成。host 在帧循环中：
/// 1. 把解释器的视频事件转发给 [`VideoBackend`] 的对应方法
/// 2. 每帧调用 [`VideoBackend::advance`] 推进同步时钟
/// 3. 通过 [`VideoBackend::poll_finish_events`] 获取视频完成事件
pub trait VideoBackend {
    // -----------------------------------------------------------------------
    // 全屏视频操作
    // -----------------------------------------------------------------------

    /// 播放全屏视频。
    ///
    /// 全屏视频直接渲染到整个舞台，不创建图层。
    /// 若已有全屏视频在播放，先停止旧视频再播放新视频。
    fn play_fullscreen(&mut self, config: &VideoConfig);

    /// 停止全屏视频。
    ///
    /// 返回 `true` 表示确实有全屏视频被停止。
    fn stop_fullscreen(&mut self) -> bool;

    /// 查询是否有全屏视频正在播放。
    fn is_fullscreen_playing(&self) -> bool;

    // -----------------------------------------------------------------------
    // 视频图层操作
    // -----------------------------------------------------------------------

    /// 播放视频图层。
    ///
    /// 视频作为图层渲染，支持图层属性。
    /// 相同 `id` 的新播放会直接覆盖旧播放。
    fn play_layer(&mut self, id: &str, config: &VideoConfig);

    /// 停止指定 ID 的视频图层。
    ///
    /// 返回 `true` 表示该 ID 的视频图层确实存在并被停止。
    fn stop_layer(&mut self, id: &str) -> bool;

    /// 查询是否有指定 ID 的视频图层正在播放。
    fn is_layer_playing(&self, id: &str) -> bool;

    // -----------------------------------------------------------------------
    // 全局操作
    // -----------------------------------------------------------------------

    /// 立即停止所有视频（全屏 + 图层）。
    fn stop_all_videos(&mut self);

    // -----------------------------------------------------------------------
    // 视频完成事件处理
    // -----------------------------------------------------------------------

    /// 注册视频播放完成事件处理器。
    fn set_finish_handler(&mut self, handler: VideoFinishHandler);

    /// 解除视频播放完成事件处理器。
    fn remove_finish_handler(&mut self);

    // -----------------------------------------------------------------------
    // 帧循环
    // -----------------------------------------------------------------------

    /// 推进视频内部时钟。
    ///
    /// host 每帧用累计的真实时间增量调用一次。
    fn advance(&mut self, delta_ms: u64);

    /// 查询并清空自上次调用以来产生的视频完成事件。
    ///
    /// 包括：非循环的视频自然播放完成。
    /// 返回的事件按发生先后顺序排列。
    fn poll_finish_events(&mut self) -> Vec<VideoFinishEvent>;

    // -----------------------------------------------------------------------
    // 状态查询
    // -----------------------------------------------------------------------

    /// 获取当前视频状态（只读）。
    fn video_state(&self) -> &VideoState;

    /// 获取当前视频状态（可变）。
    fn video_state_mut(&mut self) -> &mut VideoState;

}
