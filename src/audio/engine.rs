//! 音频逻辑状态与宿主音频协议。
//!
//! Core only produces audio state changes and completion synchronization.  It
//! does not stream PCM through Dart.  A production host should implement the
//! actual audio sink on the native/host side (ring buffer or native pull
//! callback), while Dart controls lifecycle and routes commands.
//!
//! ## 参数约定
//! - gain: Artemis 原始增益值 0-1000，映射为线性增益 0.0-1.0+
//! - pan:  -1000（完全左声道）~ 1000（完全右声道），映射为 -1.0 ~ 1.0
//! - time: 过渡时间，单位毫秒；0 表示即时生效
//!
//! ## 与合成器的关系
//! 音频子系统与 [`crate::compositor::Compositor`] 平级，不直接依赖图形或音频设备
//! 后端。宿主在帧循环中推进 core 的逻辑时钟，并通过 host media command 执行真实
//! 播放。

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// 音频类别
// ---------------------------------------------------------------------------

/// 声音播放类别，决定其使用的音量通道与完成处理器。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SoundCategory {
    /// 背景音乐（单通道，同时只能播放一首）
    Bgm,
    /// 音效（多通道，通过 ID 标识和管理）
    Se,
    /// 语音（与 SE 类似，但使用独立音量通道，并在日志中关联文本）
    Voice,
}

// ---------------------------------------------------------------------------
// 播放配置
// ---------------------------------------------------------------------------

/// BGM 播放参数。
///
/// 对应 Artemis 的 `splay` 标签。BGM 只支持单通道：
/// 新 BGM 开始播放时，前一首自动停止（或由 `sxfade` 交叉淡入）。
#[derive(Debug, Clone)]
pub struct BgmConfig {
    /// 是否循环播放
    pub loop_play: bool,
    /// 增益 0-1000，None 表示不设置（保持默认 1.0）
    pub gain: Option<i32>,
    /// 声像 -1000~1000，None 表示居中
    pub pan: Option<i32>,
    /// 淡入时间（毫秒），0 表示即时
    pub fade_in_ms: u64,
    /// 缓冲区大小（毫秒），-1 表示内存播放
    pub buffer_size: Option<i32>,
}

impl Default for BgmConfig {
    fn default() -> Self {
        Self {
            loop_play: true,
            gain: None,
            pan: None,
            fade_in_ms: 0,
            buffer_size: None,
        }
    }
}

/// SE 播放参数。
///
/// 对应 Artemis 的 `seplay` / `voice` 标签。SE 通过 `id` 标识，
/// 同一 ID 的新播放会覆盖旧播放。
#[derive(Debug, Clone)]
pub struct SeConfig {
    /// 是否循环播放
    pub loop_play: bool,
    /// 增益 0-1000
    pub gain: Option<i32>,
    /// 声像 -1000~1000
    pub pan: Option<i32>,
    /// 淡入时间（毫秒）
    pub fade_in_ms: u64,
    /// 缓冲区大小（毫秒），-1 表示内存播放
    pub buffer_size: Option<i32>,
    /// skippable=1 时，在快进/跳过期间不播放该 SE
    pub skippable: bool,
}

impl Default for SeConfig {
    fn default() -> Self {
        Self {
            loop_play: false,
            gain: None,
            pan: None,
            fade_in_ms: 0,
            buffer_size: None,
            skippable: false,
        }
    }
}

// ---------------------------------------------------------------------------
// 声音完成事件
// ---------------------------------------------------------------------------

/// 声音播放完成时触发的事件处理器注册信息。
///
/// 对应 Artemis 的 `setonsoundfinish` 标签。按 `id` 绑定到
/// 特定 SE 或 BGM（`id` 为 None 时绑定到 BGM）。
#[derive(Debug, Clone)]
pub struct SoundFinishHandler {
    /// 跳转/调用目标脚本文件
    pub file: Option<String>,
    /// 跳转/调用目标标签
    pub label: Option<String>,
    /// call=1 时压调用栈，否则等同 jump
    pub call: bool,
    /// 就地执行的标签名（如 `"calllua"`）
    pub handler: Option<String>,
}

/// 声音完成事件：当一段声音（BGM 或 SE）播放结束时产生。
///
/// 宿主在每帧调用 [`AudioBackend::poll_finish_events`] 获取，
/// 然后把它们交还给解释器执行（类似输入事件的 handler 体系）。
#[derive(Debug, Clone)]
pub struct SoundFinishEvent {
    /// 触发完成的 SE 的 ID（BGM 完成时为 None）
    pub id: Option<String>,
    /// 声音类别
    pub category: SoundCategory,
    /// 已注册的完成处理器（如果存在）
    pub handler: Option<SoundFinishHandler>,
}

// ---------------------------------------------------------------------------
// 淡出状态
// ---------------------------------------------------------------------------

/// 通道淡出状态。
///
/// 淡出用于 sstop/sestop 的 `time` 参数：在指定毫秒内将音量从当前值
/// 渐变到 0，完成后停止通道。也用于 sfade/sefade/span/sepan 的非零 `time` 参数。
#[derive(Debug, Clone)]
pub struct FadeState {
    /// 目标增益（线性 0.0-1.0+）
    pub target_gain: f32,
    /// 目标声像（线性 -1.0~1.0）
    pub target_pan: f32,
    /// 起始增益
    pub from_gain: f32,
    /// 起始声像
    pub from_pan: f32,
    /// 淡入淡出开始时刻（音频子系统时钟，毫秒）
    pub start_ms: u64,
    /// 过渡时长（毫秒）
    pub duration_ms: u64,
    /// 过渡完成后是否停止该通道（用于 fade-out stop）
    pub stop_on_complete: bool,
}

impl FadeState {
    /// 根据已流逝时间计算当前值。
    ///
    /// 返回 `(当前增益, 当前声像, 是否已完成)`。
    pub fn current_value(&self, now_ms: u64) -> (f32, f32, bool) {
        let elapsed = now_ms.saturating_sub(self.start_ms);
        if elapsed >= self.duration_ms || self.duration_ms == 0 {
            return (self.target_gain, self.target_pan, true);
        }
        let t = elapsed as f32 / self.duration_ms as f32;
        let gain = self.from_gain + (self.target_gain - self.from_gain) * t;
        let pan = self.from_pan + (self.target_pan - self.from_pan) * t;
        (gain, pan, false)
    }
}

// ---------------------------------------------------------------------------
// 声音通道
// ---------------------------------------------------------------------------

/// 单个声音播放通道的状态。
#[derive(Debug, Clone)]
pub struct SoundChannel {
    /// SE/Voice 的 ID；BGM 固定为 `"bgm"`
    pub id: String,
    /// 音频文件路径
    pub file: String,
    /// 播放类型
    pub category: SoundCategory,
    /// 是否正在播放（实际音频输出）
    pub playing: bool,
    /// 是否循环播放
    pub loop_play: bool,
    /// Artemis 原始增益值 (0-1000)
    pub raw_gain: i32,
    /// Artemis 原始声像值 (-1000..1000)
    pub raw_pan: i32,
    /// 当前实际线性增益
    pub current_gain: f32,
    /// 当前实际线性声像
    pub current_pan: f32,
    /// 进行中的淡入淡出（如果有）
    pub fade: Option<FadeState>,
    /// skippable=1 时，在快进/跳过期间不播放该 SE。
    pub skippable: bool,
}

impl SoundChannel {
    pub fn new(id: &str, file: &str, category: SoundCategory) -> Self {
        Self {
            id: id.to_string(),
            file: file.to_string(),
            category,
            playing: false,
            loop_play: false,
            raw_gain: 1000,
            raw_pan: 0,
            current_gain: 1.0,
            current_pan: 0.0,
            fade: None,
            skippable: false,
        }
    }

    /// Artemis 增益 (0-1000) → 线性增益 (0.0+)。
    pub fn gain_to_linear(raw: i32) -> f32 {
        (raw.max(0) as f32) / 1000.0
    }

    /// Artemis 声像 (-1000..1000) → 线性声像 (-1.0..1.0)。
    pub fn pan_to_linear(raw: i32) -> f32 {
        (raw.clamp(-1000, 1000) as f32) / 1000.0
    }
}

// ---------------------------------------------------------------------------
// 全局音频状态
// ---------------------------------------------------------------------------

/// 音频子系统的全局状态。
///
/// 维护所有活跃的声音通道、各通道组的音量控制、完成事件处理器注册表
/// 以及淡入淡出进度所需的内部时钟。
#[derive(Debug, Clone)]
pub struct AudioState {
    /// 当前 BGM 通道（单通道，同时只有一首）
    pub bgm_channel: Option<SoundChannel>,
    /// 活跃的 SE 通道表（按 ID 索引）
    pub se_channels: HashMap<String, SoundChannel>,
    /// 活跃的 Voice 通道表（按 ID 索引）
    pub voice_channels: HashMap<String, SoundChannel>,
    /// 主音量 0.0-1.0
    pub master_volume: f32,
    /// BGM 音量 0.0-1.0
    pub bgm_volume: f32,
    /// SE 音量 0.0-1.0
    pub se_volume: f32,
    /// Voice 音量 0.0-1.0
    pub voice_volume: f32,
    /// BGM 播放完成事件处理器
    pub bgm_finish_handler: Option<SoundFinishHandler>,
    /// SE 播放完成事件处理器表（按 SE ID 索引）
    pub se_finish_handlers: HashMap<String, SoundFinishHandler>,
    /// 音频子系统内部时钟（毫秒），用于淡入淡出计时
    pub clock_ms: u64,
    /// 当前是否处于快进/跳过模式（skippable SE 此时不播放）
    pub is_skipping: bool,
}

impl Default for AudioState {
    fn default() -> Self {
        Self {
            bgm_channel: None,
            se_channels: HashMap::new(),
            voice_channels: HashMap::new(),
            master_volume: 1.0,
            bgm_volume: 1.0,
            se_volume: 1.0,
            voice_volume: 1.0,
            bgm_finish_handler: None,
            se_finish_handlers: HashMap::new(),
            clock_ms: 0,
            is_skipping: false,
        }
    }
}

// ---------------------------------------------------------------------------
// AudioBackend trait
// ---------------------------------------------------------------------------

/// 音频逻辑状态后端。
///
/// 实现方维护 core 侧通道状态、淡入淡出和完成事件。真实输出设备由 host/native
/// audio sink 负责。host 在帧循环中：
/// 1. 把解释器的音频事件转发给 [`AudioBackend`] 的对应方法
/// 2. 每帧调用 [`AudioBackend::advance`] 推进淡入淡出
/// 3. 通过 [`AudioBackend::poll_finish_events`] 获取声音完成事件
///
/// ## 计划实现
/// - [`crate::audio::AudioStateBackend`]：逻辑状态实现，用于测试和 runtime
pub trait AudioBackend {
    // -----------------------------------------------------------------------
    // BGM 操作
    // -----------------------------------------------------------------------

    /// 播放 BGM。
    ///
    /// BGM 单通道约束：若已有 BGM 在播放，先停止旧 BGM 再播放新曲。
    /// 需要交叉淡入时使用 [`crossfade_bgm`]。
    fn play_bgm(&mut self, file: &str, config: &BgmConfig);

    /// 停止 BGM，可选淡出。
    ///
    /// 返回 `true` 表示确实有 BGM 被停止。
    fn stop_bgm(&mut self, fade_time_ms: u64) -> bool;

    /// 交叉淡入 BGM：启动新曲同时淡出旧曲。
    ///
    /// 旧 BGM 在 `fade_time_ms` 内淡出，新 BGM 同步淡入。
    fn crossfade_bgm(&mut self, file: &str, config: &BgmConfig);

    /// 调整 BGM 增益（可带淡入淡出）。
    fn fade_bgm_gain(&mut self, target_gain_raw: i32, time_ms: u64);

    /// 调整 BGM 声像（可带淡入淡出）。
    fn pan_bgm(&mut self, target_pan_raw: i32, time_ms: u64);

    // -----------------------------------------------------------------------
    // SE 操作
    // -----------------------------------------------------------------------

    /// 播放音效。
    ///
    /// 相同 `id` 的新播放会直接覆盖旧播放（停止旧 SE，开始新 SE）。
    /// `config.skippable` 为 true 且当前正在快进时，不实际播放。
    fn play_se(&mut self, id: &str, file: &str, config: &SeConfig);

    /// 停止指定 ID 的音效，可选淡出。
    ///
    /// 返回 `true` 表示该 ID 的音效确实存在并被停止。
    fn stop_se(&mut self, id: &str, fade_time_ms: u64) -> bool;

    /// 调整指定 SE 的增益（可带淡入淡出）。
    fn fade_se_gain(&mut self, id: &str, target_gain_raw: i32, time_ms: u64);

    /// 调整指定 SE 的声像（可带淡入淡出）。
    fn pan_se(&mut self, id: &str, target_pan_raw: i32, time_ms: u64);

    // -----------------------------------------------------------------------
    // Voice 操作
    // -----------------------------------------------------------------------

    /// 播放语音。
    ///
    /// 与 SE 共用参数空间，但使用独立的音量通道（`voice_volume`），
    /// 并在内置后台日志中关联文本。
    fn play_voice(&mut self, id: &str, file: &str, config: &SeConfig);

    // -----------------------------------------------------------------------
    // 全局操作
    // -----------------------------------------------------------------------

    /// 立即停止所有声音（BGM + SE + Voice）。
    fn stop_all_sounds(&mut self);

    /// 设置主音量。
    fn set_master_volume(&mut self, volume: f32);

    /// 设置 BGM 音量。
    fn set_bgm_volume(&mut self, volume: f32);

    /// 设置 SE 音量。
    fn set_se_volume(&mut self, volume: f32);

    /// 设置 Voice 音量。
    fn set_voice_volume(&mut self, volume: f32);

    /// 设置快进/跳过模式。
    ///
    /// 启用后，标记为 `skippable` 的 SE 不会被实际播放。
    fn set_skipping(&mut self, skipping: bool);

    /// 查询是否有指定 ID 的音效正在播放。
    fn is_se_playing(&self, id: &str) -> bool;

    /// 查询 BGM 是否正在播放。
    fn is_bgm_playing(&self) -> bool;

    // -----------------------------------------------------------------------
    // 声音完成事件处理
    // -----------------------------------------------------------------------

    /// 注册声音播放完成事件处理器。
    ///
    /// `id` 为 `None` 时注册到 BGM，否则注册到指定 ID 的 SE。
    fn set_sound_finish_handler(&mut self, id: Option<&str>, handler: SoundFinishHandler);

    /// 解除声音播放完成事件处理器。
    ///
    /// `id` 为 `None` 时解除 BGM，否则解除指定 ID 的 SE。
    fn remove_sound_finish_handler(&mut self, id: Option<&str>);

    // -----------------------------------------------------------------------
    // 帧循环
    // -----------------------------------------------------------------------

    /// 推进音频内部时钟，处理淡入淡出进度。
    ///
    /// host 每帧用累计的真实时间增量调用一次。
    fn advance(&mut self, delta_ms: u64);

    /// 查询并清空自上次调用以来产生的声音完成事件。
    ///
    /// 包括：非循环的 BGM/SE 自然播放完成、fade-out 后停止的通道。
    /// 返回的事件按发生先后顺序排列。
    fn poll_finish_events(&mut self) -> Vec<SoundFinishEvent>;

    // -----------------------------------------------------------------------
    // 状态查询
    // -----------------------------------------------------------------------

    /// 获取当前音频状态（只读）。
    fn audio_state(&self) -> &AudioState;

    /// 获取当前音频状态（可变）。
    fn audio_state_mut(&mut self) -> &mut AudioState;
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 将淡出时间（毫秒）应用到通道上。
///
/// 如果 `time_ms` > 0，为该通道创建一个 fade-out 过渡（目标增益=0，完成后自动停止）。
/// 如果 `time_ms` == 0，立即停止通道。
pub(crate) fn apply_channel_fade_out(channel: &mut SoundChannel, time_ms: u64, clock_ms: u64) {
    if time_ms == 0 {
        channel.playing = false;
        channel.current_gain = 1.0;
        channel.fade = None;
    } else {
        let from_gain = channel.current_gain;
        channel.fade = Some(FadeState {
            target_gain: 0.0,
            target_pan: channel.current_pan,
            from_gain,
            from_pan: channel.current_pan,
            start_ms: clock_ms,
            duration_ms: time_ms,
            stop_on_complete: true,
        });
    }
}

/// 将增益过渡应用到通道上。
pub(crate) fn apply_channel_gain_fade(
    channel: &mut SoundChannel,
    target_gain_raw: i32,
    time_ms: u64,
    clock_ms: u64,
) {
    let target = SoundChannel::gain_to_linear(target_gain_raw);
    channel.raw_gain = target_gain_raw;

    if time_ms == 0 {
        channel.current_gain = target;
        channel.fade = None;
    } else {
        let from_gain = channel.current_gain;
        channel.fade = Some(FadeState {
            target_gain: target,
            target_pan: channel.current_pan,
            from_gain,
            from_pan: channel.current_pan,
            start_ms: clock_ms,
            duration_ms: time_ms,
            stop_on_complete: false,
        });
    }
}

/// 将声像过渡应用到通道上。
pub(crate) fn apply_channel_pan_fade(
    channel: &mut SoundChannel,
    target_pan_raw: i32,
    time_ms: u64,
    clock_ms: u64,
) {
    let target = SoundChannel::pan_to_linear(target_pan_raw);
    channel.raw_pan = target_pan_raw;

    if time_ms == 0 {
        channel.current_pan = target;
        channel.fade = None;
    } else {
        let from_pan = channel.current_pan;
        channel.fade = Some(FadeState {
            target_gain: channel.current_gain,
            target_pan: target,
            from_gain: channel.current_gain,
            from_pan,
            start_ms: clock_ms,
            duration_ms: time_ms,
            stop_on_complete: false,
        });
    }
}

/// 推进单个通道的淡入淡出时钟。
///
/// 返回 `true` 表示该通道的 fade 已完成且应被停止（`stop_on_complete`）。
pub(crate) fn advance_channel_fade(channel: &mut SoundChannel, now_ms: u64) -> bool {
    let Some(fade) = &channel.fade else {
        return false;
    };
    let (gain, pan, finished) = fade.current_value(now_ms);
    channel.current_gain = gain;
    channel.current_pan = pan;

    if finished {
        let stop = fade.stop_on_complete;
        channel.fade = None;
        if stop {
            channel.playing = false;
        }
        return stop;
    }
    false
}
