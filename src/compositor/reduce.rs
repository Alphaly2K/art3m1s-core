//! 事件归约：把解释器的 [`Event`] 流应用到合成器状态上。
//!
//! [`Compositor`] 持有一棵 [`Scene`] 和一个合成器时钟。它消费解释器在 `run` 过程
//! 中通过回调发出的 `Event`，把与画面相关的变体（图层增删改、缓动、转场）落到
//! 场景树上；与画面无关的变体（音频、存档、文本…）忽略，留给引擎别的子系统。
//!
//! 时间推进与渲染分离：解释器只管"发生了什么"，由宿主每帧调用 [`Compositor::render`]
//! 把当前时刻的场景画出来。

use crate::audio::engine::{
    AudioBackend, BgmConfig, SeConfig, SoundFinishEvent, SoundFinishHandler,
};
use crate::compositor::anim::{Easing, Tween, TweenHandler};
use crate::compositor::build::build_frame;
use crate::compositor::renderer::{DrawCommand, DrawList, Renderer, TextureProvider};
use crate::compositor::scene::{LayerEventHandler, Scene};
use crate::save::{AudioChannelSnapshot, AudioSnapshot};
use crate::text::render::{ScetweenConfig, ScetweenMode, ScetweenSetMode, TextRenderer};
use crate::video::engine::{VideoBackend, VideoConfig, VideoFinishEvent, VideoFinishHandler};
use asb_interpreter::event::{Event, LayerEvent};
use std::cell::RefCell;
use std::collections::HashMap;

/// 已注册的输入事件处理器。
///
/// 由 Lua 脚本通过 `e:tag{"setonpush", key=..., handler="calllua", function="..."}`
/// 之类的 seton* 标签注册。引擎在检测到相应输入时把它交还解释器执行，自身不解释
/// handler/function 的含义——与 [`LayerEventHandler`] 同构。
#[derive(Debug, Clone, Default)]
pub struct InputHandler {
    /// 命中时先就地执行的标签名（如 `"calllua"`）。
    pub handler: Option<String>,
    /// 跳转/调用目标脚本文件。
    pub file: Option<String>,
    /// 跳转/调用目标标签。
    pub label: Option<String>,
    /// call=1 时压调用栈（对应 call 标签），否则等同 jump。
    pub call: bool,
    /// 标签里除已知字段外的所有参数（function、key、adv、ui、btn 等），
    /// 触发时原样塞进 handler 标签的参数表。
    pub params: HashMap<String, String>,
}

/// 后端无关的合成器：场景树 + 时钟 + 事件归约。
pub struct Compositor {
    scene: Scene,
    /// 合成器时钟（毫秒），缓动与转场都基于它。
    clock_ms: u64,
    /// 舞台到物理像素的缩放因子（HiDPI）。
    stage_scale: f32,
    /// 输入事件处理器注册表，按 (event_name, key) 索引。
    input_handlers: HashMap<(String, String), InputHandler>,
    /// 文本渲染器（RefCell 使 render(&self) 可调用其 &mut 方法）。
    text_renderer: RefCell<Option<Box<dyn TextRenderer>>>,
    /// 音频后端（RefCell 使 apply_event(&mut self) 外亦可内部分发）。
    audio_backend: RefCell<Option<Box<dyn AudioBackend>>>,
    /// 视频后端（RefCell 使 apply_event(&mut self) 外亦可内部分发）。
    video_backend: RefCell<Option<Box<dyn VideoBackend>>>,
    /// 自上次 `poll_tween_events` 以来产生待处理的缓动完成事件。
    pending_tween_events: Vec<TweenHandler>,
}

impl std::fmt::Debug for Compositor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compositor")
            .field("scene", &self.scene)
            .field("clock_ms", &self.clock_ms)
            .field("stage_scale", &self.stage_scale)
            .field("input_handlers", &self.input_handlers)
            .field(
                "text_renderer",
                &self.text_renderer.borrow().as_ref().map(|_| ".."),
            )
            .field(
                "audio_backend",
                &self.audio_backend.borrow().as_ref().map(|_| ".."),
            )
            .field(
                "video_backend",
                &self.video_backend.borrow().as_ref().map(|_| ".."),
            )
            .field("pending_tween_events", &self.pending_tween_events)
            .finish()
    }
}

impl Default for Compositor {
    fn default() -> Self {
        let audio_backend = {
            #[cfg(feature = "audio-backend")]
            {
                crate::audio::RodioBackend::new()
                    .ok()
                    .map(|a| Box::new(a) as Box<dyn AudioBackend>)
            }
            #[cfg(not(feature = "audio-backend"))]
            {
                None
            }
        };
        let video_backend = {
            #[cfg(feature = "video-backend")]
            {
                crate::core_debug!("[Video] 使用 FFmpeg 视频后端");
                Some(Box::new(crate::video::FfmpegBackend::new()) as Box<dyn VideoBackend>)
            }
            #[cfg(not(feature = "video-backend"))]
            {
                crate::core_info!("[Video] 使用存根视频后端（video-backend feature 未启用）");
                Some(Box::new(crate::video::StubVideoBackend::new()) as Box<dyn VideoBackend>)
            }
        };
        Self {
            scene: Scene::new(),
            clock_ms: 0,
            stage_scale: 1.0,
            input_handlers: HashMap::new(),
            text_renderer: RefCell::new(None),
            audio_backend: RefCell::new(audio_backend),
            video_backend: RefCell::new(video_backend),
            pending_tween_events: Vec::new(),
        }
    }
}

impl Compositor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    pub fn scene_snapshot(&self) -> Scene {
        self.scene.clone()
    }

    pub fn audio_snapshot(&self) -> AudioSnapshot {
        let audio_opt = self.audio_backend.borrow();
        let Some(audio) = audio_opt.as_ref() else {
            return AudioSnapshot::default();
        };
        let state = audio.audio_state();
        let bgm = state
            .bgm_channel
            .as_ref()
            .filter(|channel| channel.playing)
            .map(AudioChannelSnapshot::from);
        let mut se: Vec<_> = state
            .se_channels
            .values()
            .filter(|channel| channel.playing)
            .map(AudioChannelSnapshot::from)
            .collect();
        let mut voice: Vec<_> = state
            .voice_channels
            .values()
            .filter(|channel| channel.playing)
            .map(AudioChannelSnapshot::from)
            .collect();
        se.sort_by(|a, b| a.id.cmp(&b.id));
        voice.sort_by(|a, b| a.id.cmp(&b.id));
        AudioSnapshot { bgm, se, voice }
    }

    pub fn restore_scene(&mut self, scene: Scene) {
        self.scene.replace_with(scene);
        self.pending_tween_events.clear();
    }

    pub fn restore_audio(&self, snapshot: &AudioSnapshot) {
        let mut audio_opt = self.audio_backend.borrow_mut();
        let Some(audio) = audio_opt.as_mut() else {
            return;
        };
        if let Some(bgm) = &snapshot.bgm {
            audio.play_bgm(
                &bgm.file,
                &BgmConfig {
                    loop_play: bgm.loop_play,
                    gain: Some(bgm.gain),
                    pan: Some(bgm.pan),
                    fade_in_ms: 0,
                    buffer_size: None,
                },
            );
        }
        for se in &snapshot.se {
            audio.play_se(
                &se.id,
                &se.file,
                &SeConfig {
                    loop_play: se.loop_play,
                    gain: Some(se.gain),
                    pan: Some(se.pan),
                    fade_in_ms: 0,
                    buffer_size: None,
                    skippable: se.skippable,
                },
            );
        }
        for voice in &snapshot.voice {
            audio.play_voice(
                &voice.id,
                &voice.file,
                &SeConfig {
                    loop_play: voice.loop_play,
                    gain: Some(voice.gain),
                    pan: Some(voice.pan),
                    fade_in_ms: 0,
                    buffer_size: None,
                    skippable: voice.skippable,
                },
            );
        }
    }

    /// 读档是全局状态切换边界：旧画面、旧 UI 输入处理器和正在播放的音视频都
    /// 不能穿透到新存档。随后若存档携带 scene，调用方再用 `restore_scene` 覆盖。
    pub fn reset_for_load(&mut self) {
        self.scene.replace_with(Scene::new());
        self.input_handlers.clear();
        self.pending_tween_events.clear();
        if let Some(audio) = self.audio_backend.borrow_mut().as_mut() {
            audio.stop_all_sounds();
        }
        if let Some(video) = self.video_backend.borrow_mut().as_mut() {
            video.stop_all_videos();
        }
    }

    pub fn clock_ms(&self) -> u64 {
        self.clock_ms
    }

    /// 设置舞台缩放因子（HiDPI scale）。宿主在窗口初始化/缩放变化时调用。
    pub fn set_stage_scale(&mut self, scale: f32) {
        self.stage_scale = scale;
    }

    /// 安装文本渲染器。
    pub fn set_text_renderer(&self, renderer: Box<dyn TextRenderer>) {
        *self.text_renderer.borrow_mut() = Some(renderer);
    }

    /// 安装音频后端。
    pub fn set_audio_backend(&self, backend: Box<dyn AudioBackend>) {
        *self.audio_backend.borrow_mut() = Some(backend);
    }

    /// 安装视频后端。
    pub fn set_video_backend(&self, backend: Box<dyn VideoBackend>) {
        *self.video_backend.borrow_mut() = Some(backend);
    }

    pub fn audio_mut(&self) -> std::cell::RefMut<'_, Option<Box<dyn AudioBackend>>> {
        self.audio_backend.borrow_mut()
    }

    pub fn video_mut(&self) -> std::cell::RefMut<'_, Option<Box<dyn VideoBackend>>> {
        self.video_backend.borrow_mut()
    }

    pub fn stage_scale(&self) -> f32 {
        self.stage_scale
    }

    /// 查询指定事件/键组合的已注册处理器。宿主在检测到输入后调用。
    pub fn get_input_handler(&self, event_name: &str, key: &str) -> Option<&InputHandler> {
        self.input_handlers
            .get(&(event_name.to_string(), key.to_string()))
    }

    /// 推进合成器时钟。宿主每帧用累计的真实时间调用一次。
    pub fn advance(&mut self, delta_ms: u64) {
        self.clock_ms = self.clock_ms.saturating_add(delta_ms);
        self.gc_finished_tweens();
        if let Some(tr) = self.text_renderer.borrow_mut().as_mut() {
            tr.advance_reveal(delta_ms);
        }
        if let Some(audio) = self.audio_backend.borrow_mut().as_mut() {
            audio.advance(delta_ms);
        }
        if let Some(video) = self.video_backend.borrow_mut().as_mut() {
            video.advance(delta_ms);
        }
    }

    /// 查询并清空自上次调用以来产生的声音完成事件。
    pub fn poll_sound_finish_events(&mut self) -> Vec<SoundFinishEvent> {
        self.audio_backend
            .borrow_mut()
            .as_mut()
            .map(|a| a.poll_finish_events())
            .unwrap_or_default()
    }

    /// 查询并清空自上次调用以来产生的视频完成事件。
    pub fn poll_video_finish_events(&mut self) -> Vec<VideoFinishEvent> {
        self.video_backend
            .borrow_mut()
            .as_mut()
            .map(|v| v.poll_finish_events())
            .unwrap_or_default()
    }
    ///
    /// 宿主在每帧 `advance` 之后调用，将返回到的 [`TweenHandler`] 交回解释器
    /// 执行（对于 `sync` 缓动则构造 Wait 事件暂停脚本）。
    pub fn poll_tween_events(&mut self) -> Vec<TweenHandler> {
        std::mem::take(&mut self.pending_tween_events)
    }

    /// 把一个解释器事件应用到场景上。与画面无关的事件返回而不改动状态。
    pub fn apply_event(&mut self, event: &Event) {
        match event {
            Event::Layer(layer_event) => self.apply_layer_event(layer_event),
            Event::LayerRename { id, to } => {
                self.scene.rename(id, to);
            }
            Event::LayerTween {
                id,
                param,
                from,
                to,
                ease,
                time,
                delay,
                loop_count,
                yoyo,
                loop_delay,
                sync,
                delete,
                handler_file,
                handler_label,
                handler_handler,
            } => self.apply_tween(
                id,
                param,
                from.as_deref(),
                to.as_deref(),
                ease.as_deref(),
                *time,
                *delay,
                *loop_count,
                *yoyo,
                *loop_delay,
                *sync,
                *delete,
                handler_file.as_deref(),
                handler_label.as_deref(),
                handler_handler.as_deref(),
            ),
            Event::LayerTweenDelete { id } => {
                // 强制完成：把该图层所有缓动直接落到终值并清空。
                self.finish_tweens(id);
            }
            Event::LayerEventHandler {
                id,
                event_type,
                mode,
                file,
                label,
                call,
                handler,
                penetration,
                extra_params,
            } => {
                // mode 语义见 lyevent spec：
                //   init    -> 注册/覆盖该事件类型的处理器
                //   reset   -> 移除该事件类型的处理器
                //   disable -> 保留信息但暂停执行（这里以移除近似；脚本随后会 enable 重设）
                //   enable  -> 重新启用（脚本通常配合 init 重新注册，无需特殊处理）
                self.scene.ensure(id);
                if let Some(layer) = self.scene.get_mut(id) {
                    match mode.as_str() {
                        "reset" | "disable" => {
                            layer.event_handlers.remove(event_type);
                        }
                        // "init"、"enable" 以及未指定都视为注册。
                        _ => {
                            layer.event_handlers.insert(
                                event_type.clone(),
                                LayerEventHandler {
                                    handler: handler.clone(),
                                    file: file.clone(),
                                    label: label.clone(),
                                    call: *call,
                                    penetration: *penetration,
                                    params: extra_params.clone(),
                                },
                            );
                        }
                    }
                }
            }
            // 输入事件处理器注册（setonpush 等 seton* 标签）。
            Event::SetEventHandler {
                event_name,
                file,
                label,
                call,
                handler,
                extra_params,
            } => {
                // key 字段标识处理器响应的按键/输入（"1" = 鼠标左键）。
                // 引擎按 (event_name, key) 索引，不解释 handler/function 的语义。
                let key = extra_params.get("key").cloned().unwrap_or_default();
                self.input_handlers.insert(
                    (event_name.clone(), key),
                    InputHandler {
                        handler: handler.clone(),
                        file: file.clone(),
                        label: label.clone(),
                        call: *call,
                        params: extra_params.clone(),
                    },
                );
            }
            Event::DelEventHandler { event_name, key } => {
                if let Some(key) = key {
                    self.input_handlers
                        .remove(&(event_name.clone(), key.clone()));
                } else {
                    self.input_handlers
                        .retain(|(name, _), _| name != event_name);
                }
            }
            // ── 音频事件转发 ──
            _ => self.forward_audio_or_text_event(event),
        }
    }

    fn apply_layer_event(&mut self, event: &LayerEvent) {
        match event {
            LayerEvent::Create { id, file } => {
                self.scene.create(id, Some(file.clone()));
            }
            LayerEvent::Create2 { id, file, alpha } => {
                self.scene.create(id, Some(file.clone()));
                if let Some(alpha) = alpha {
                    let mut raw = HashMap::new();
                    raw.insert("alpha".to_string(), alpha.to_string());
                    self.scene.set_props(id, &raw);
                }
            }
            LayerEvent::Delete { id } => {
                self.scene.delete(id);
            }
            LayerEvent::SetProperty {
                id,
                property,
                value,
            } => {
                let mut raw = HashMap::new();
                raw.insert(property.clone(), value.clone());
                self.scene.set_props(id, &raw);
            }
            LayerEvent::SetProperties { id, properties } => {
                self.scene.set_props(id, properties);
            }
        }
    }

    /// 把一个 `[lytween]` 落成图层上的 [`Tween`]。
    ///
    /// `from` 省略时取属性当前值；`to` 解析失败则忽略本次缓动（没有目标无意义）。
    #[allow(clippy::too_many_arguments)]
    fn apply_tween(
        &mut self,
        id: &str,
        param: &str,
        from: Option<&str>,
        to: Option<&str>,
        ease: Option<&str>,
        time: Option<u64>,
        delay: Option<u64>,
        loop_count: Option<i32>,
        yoyo: Option<i32>,
        loop_delay: Option<u64>,
        sync: bool,
        delete: bool,
        handler_file: Option<&str>,
        handler_label: Option<&str>,
        handler_handler: Option<&str>,
    ) {
        let Some(to_value) = to.and_then(parse_num) else {
            return;
        };

        self.scene.ensure(id);
        let from_value = from
            .and_then(parse_num)
            .unwrap_or_else(|| self.current_param_value(id, param));

        let start_ms = self.clock_ms + delay.unwrap_or(0);

        // 解析循环：-1 → 无限，0 → 不循环，N → 循环 N 次
        let infinite_loop = loop_count == Some(-1);
        let loops: Option<u32> = if infinite_loop || loop_count.unwrap_or(0) <= 0 {
            None
        } else {
            Some(loop_count.unwrap() as u32)
        };

        // 解析 yoyo：-1 → 无限乒乓，0 → 不乒乓，N → 乒乓 N 次
        let yoyo_enabled = yoyo == Some(-1) || yoyo.unwrap_or(0) > 0;
        let yoyo_loops: Option<u32> = if yoyo_enabled {
            if yoyo == Some(-1) {
                None // 无限
            } else if yoyo.unwrap_or(0) > 0 {
                Some(yoyo.unwrap() as u32)
            } else {
                None
            }
        } else {
            None
        };

        // 使用 yoyo 的循环次数（如果有的话），否则用 loop_count
        let effective_loops = yoyo_loops.or(loops);
        let infinite = infinite_loop || yoyo == Some(-1);

        let tween = Tween {
            param: param.to_string(),
            from: from_value,
            to: to_value,
            easing: ease.map(Easing::parse).unwrap_or_default(),
            start_ms,
            duration_ms: time.unwrap_or(0),
            infinite_loop: infinite,
            loop_count: effective_loops,
            yoyo: yoyo_enabled,
            yoyo_reverse: false,
            loop_delay_ms: loop_delay.unwrap_or(0),
            delete_on_finish: delete,
            handler: if sync
                || handler_file.is_some()
                || handler_label.is_some()
                || handler_handler.is_some()
                || delete
            {
                Some(TweenHandler {
                    file: handler_file.map(|s| s.to_string()),
                    label: handler_label.map(|s| s.to_string()),
                    call: false,
                    handler: handler_handler.map(|s| s.to_string()),
                })
            } else {
                None
            },
        };

        if let Some(layer) = self.scene.get_mut(id) {
            layer.tweens.retain(|t| t.param != param);
            layer.tweens.push(tween);
        }
    }

    /// 读取图层某属性的当前数值，作为缓动的默认起点。未知属性回退 0。
    fn current_param_value(&self, id: &str, param: &str) -> f32 {
        let Some(layer) = self.scene.get(id) else {
            return default_param_value(param);
        };
        let p = &layer.props;
        match param {
            "left" | "x" => p.left.unwrap_or(0.0),
            "top" | "y" => p.top.unwrap_or(0.0),
            "xscale" | "scale_x" => p.x_scale.unwrap_or(100.0),
            "yscale" | "scale_y" => p.y_scale.unwrap_or(100.0),
            "rotate" => p.rotate.unwrap_or(0.0),
            "alpha" => p.alpha.unwrap_or(255) as f32,
            "anchorx" | "anchor_x" => p.anchor_x.unwrap_or(0.0),
            "anchory" | "anchor_y" => p.anchor_y.unwrap_or(0.0),
            "width" => p.width.unwrap_or(0.0),
            "height" => p.height.unwrap_or(0.0),
            "zoom" => p.x_scale.unwrap_or(100.0),
            _ => default_param_value(param),
        }
    }

    /// 强制完成某图层的所有缓动：把终值写回属性，清空缓动列表。
    fn finish_tweens(&mut self, id: &str) {
        if let Some(layer) = self.scene.get_mut(id) {
            let finished: Vec<(String, f32)> = layer
                .tweens
                .iter()
                .map(|t| (t.param.clone(), t.to))
                .collect();
            layer.tweens.clear();
            let props = &mut layer.props;
            for (param, value) in finished {
                props.set_raw(&param, &format_param(&param, value));
            }
        }
    }

    /// 回收已结束的缓动，把终值固化到属性里，并收集完成回调。每帧 `advance` 调用。
    fn gc_finished_tweens(&mut self) {
        let now = self.clock_ms;
        let mut settle: Vec<(String, String, f32)> = Vec::new();
        let mut completed: Vec<(String, Option<TweenHandler>, bool)> = Vec::new();
        let ids: Vec<String> = self.scene.iter_ids();
        for id in &ids {
            if let Some(layer) = self.scene.get(id) {
                for t in &layer.tweens {
                    if t.is_finished(now) {
                        settle.push((id.clone(), t.param.clone(), t.to));
                        completed.push((id.clone(), t.handler.clone(), t.delete_on_finish));
                    }
                }
            }
        }
        for (id, param, value) in settle {
            if let Some(layer) = self.scene.get_mut(&id) {
                layer.props.set_raw(&param, &format_param(&param, value));
                layer.tweens.retain(|t| !t.is_finished(now));
            }
        }
        for (id, handler, delete) in completed {
            if let Some(handler) = handler {
                self.pending_tween_events.push(handler);
            }
            if delete {
                self.scene.delete(&id);
            }
        }
    }

    /// 用当前时刻构建一帧并交给后端渲染。
    pub fn render(&self, renderer: &mut dyn Renderer, provider: &mut dyn TextureProvider) {
        // 更新视频纹理（如果有视频正在播放）
        self.update_video_textures(provider);

        let mut frame = self.build(provider);

        // 全屏视频：在帧的最底层插入一个覆盖整个舞台的四边形
        self.inject_fullscreen_video(&mut frame, provider);

        renderer.render(&frame);
    }

    /// 如果有全屏视频正在播放，解析其纹理并替换整个帧（独占画面）。
    fn inject_fullscreen_video(&self, frame: &mut DrawList, provider: &mut dyn TextureProvider) {
        let video_opt = self.video_backend.borrow();
        let Some(video) = video_opt.as_ref() else {
            return;
        };
        let state = video.video_state();
        let Some(ref v) = state.fullscreen_video else {
            return;
        };
        if !v.playing {
            return;
        }

        let Some((tex, info)) = provider.resolve("__video_fullscreen__") else {
            return;
        };
        frame.commands.clear();
        frame.commands.push(DrawCommand {
            texture: tex,
            size: info,
            transform: glam::Affine2::IDENTITY,
            opacity: 1.0,
            blend: crate::compositor::renderer::BlendMode::Alpha,
            color: crate::compositor::renderer::ColorFilter::default(),
            clip: crate::compositor::renderer::ClipRect::full(info),
        });
    }

    /// 更新视频纹理（将解码后的帧上传到 GPU）。
    fn update_video_textures(&self, provider: &mut dyn TextureProvider) {
        let mut video_opt = self.video_backend.borrow_mut();
        let Some(video) = video_opt.as_mut() else {
            return;
        };

        // 更新全屏视频纹理
        let fullscreen_info = {
            let state = video.video_state();
            state
                .fullscreen_video
                .as_ref()
                .map(|v| (v.width, v.height, v.playing))
        };
        if let Some((width, height, playing)) = fullscreen_info {
            if playing {
                if let Some(frame_data) = video.get_fullscreen_frame() {
                    provider.upload_rgba("__video_fullscreen__", width, height, frame_data);
                } else {
                    crate::core_debug!("[Video] 全屏视频没有新帧");
                }
            }
        }

        // 更新视频图层纹理
        let layer_infos: Vec<(String, u32, u32, bool)> = {
            let state = video.video_state();
            state
                .video_layers
                .iter()
                .map(|(id, layer)| (id.clone(), layer.width, layer.height, layer.playing))
                .collect()
        };
        for (layer_id, width, height, playing) in layer_infos {
            if playing {
                if let Some(frame_data) = video.get_frame(&layer_id) {
                    let texture_name = format!("__video_layer_{}__", layer_id);
                    provider.upload_rgba(&texture_name, width, height, frame_data);
                }
            }
        }
    }

    /// 转发音频事件到 AudioBackend，视频事件到 VideoBackend，其余事件转到 TextRenderer。
    fn forward_audio_or_text_event(&mut self, event: &Event) {
        // 先尝试音频事件
        if self.forward_audio_event(event) {
            return;
        }
        // 再尝试视频事件
        if self.forward_video_event(event) {
            return;
        }
        // 其余→文本
        self.forward_text_event(event);
    }

    /// 转发音频事件给 AudioBackend。返回 true 表示已处理。
    fn forward_audio_event(&mut self, event: &Event) -> bool {
        let mut audio_opt = self.audio_backend.borrow_mut();
        let Some(audio) = audio_opt.as_mut() else {
            return false;
        };
        match event {
            Event::BgmPlay {
                file,
                loop_play,
                gain,
                pan,
                fade_time,
            } => {
                audio.play_bgm(
                    file,
                    &BgmConfig {
                        loop_play: *loop_play,
                        gain: *gain,
                        pan: *pan,
                        fade_in_ms: fade_time.unwrap_or(0),
                        buffer_size: None,
                    },
                );
                true
            }
            Event::BgmStop { fade_time } => {
                audio.stop_bgm(fade_time.unwrap_or(0));
                true
            }
            Event::BgmFade { gain, time } => {
                audio.fade_bgm_gain(*gain, *time);
                true
            }
            Event::BgmPan { pan } => {
                audio.pan_bgm(*pan, 0);
                true
            }
            Event::BgmCrossFade {
                file,
                loop_play,
                gain,
                pan,
                time,
            } => {
                audio.crossfade_bgm(
                    file,
                    &BgmConfig {
                        loop_play: *loop_play,
                        gain: *gain,
                        pan: *pan,
                        fade_in_ms: *time,
                        buffer_size: None,
                    },
                );
                true
            }
            Event::SePlay {
                id,
                file,
                loop_play,
                gain,
                pan,
                fade_time,
                skippable,
            } => {
                audio.play_se(
                    id,
                    file,
                    &SeConfig {
                        loop_play: *loop_play,
                        gain: *gain,
                        pan: *pan,
                        fade_in_ms: fade_time.unwrap_or(0),
                        buffer_size: None,
                        skippable: *skippable,
                    },
                );
                true
            }
            Event::SeStop { id, fade_time } => {
                audio.stop_se(id, fade_time.unwrap_or(0));
                true
            }
            Event::SeFade { id, gain, time } => {
                audio.fade_se_gain(id, *gain, *time);
                true
            }
            Event::SePan { id, pan } => {
                audio.pan_se(id, *pan, 0);
                true
            }
            Event::VoicePlay {
                file,
                gain,
                pan,
                fade_time,
            } => {
                audio.play_voice(
                    "",
                    file,
                    &SeConfig {
                        loop_play: false,
                        gain: *gain,
                        pan: *pan,
                        fade_in_ms: fade_time.unwrap_or(0),
                        buffer_size: None,
                        skippable: false,
                    },
                );
                true
            }
            Event::StopAllSounds { .. } => {
                audio.stop_all_sounds();
                true
            }
            Event::SoundFinishHandler {
                id,
                file,
                label,
                call,
                handler,
            } => {
                audio.set_sound_finish_handler(
                    if id.is_empty() {
                        None
                    } else {
                        Some(id.as_str())
                    },
                    SoundFinishHandler {
                        file: file.clone(),
                        label: label.clone(),
                        call: *call,
                        handler: handler.clone(),
                    },
                );
                true
            }
            Event::SoundFinishHandlerDel { id } => {
                audio.remove_sound_finish_handler(if id.is_empty() {
                    None
                } else {
                    Some(id.as_str())
                });
                true
            }
            _ => false,
        }
    }

    /// 转发视频事件给 VideoBackend。返回 true 表示已处理。
    fn forward_video_event(&mut self, event: &Event) -> bool {
        let mut video_opt = self.video_backend.borrow_mut();
        let Some(video) = video_opt.as_mut() else {
            crate::core_warn!("[Video] 视频后端未初始化，忽略视频事件");
            return false;
        };
        match event {
            Event::VideoPlay {
                id,
                file,
                skip,
                loop_play,
            } => {
                crate::core_debug!("[Video] VideoPlay: file={}, id={:?}", file, id);
                let config = VideoConfig {
                    file: file.clone(),
                    skippable: *skip,
                    loop_play: *loop_play,
                    delay_margin_ms: None,
                };
                match id {
                    Some(layer_id) => {
                        video.play_layer(layer_id, &config);
                    }
                    None => {
                        video.play_fullscreen(&config);
                    }
                }
                true
            }
            Event::VideoFinishHandler {
                file,
                label,
                call,
                handler,
            } => {
                video.set_finish_handler(VideoFinishHandler {
                    file: file.clone(),
                    label: label.clone(),
                    call: *call,
                    handler: handler.clone(),
                });
                true
            }
            Event::VideoFinishHandlerDel => {
                video.remove_finish_handler();
                true
            }
            _ => false,
        }
    }

    /// 转发文本相关事件给 TextRenderer。
    fn forward_text_event(&mut self, event: &Event) {
        let mut tr_opt = self.text_renderer.borrow_mut();
        let Some(tr) = tr_opt.as_mut() else { return };
        match event {
            Event::ScenarioText { content, inline } => tr.push_text(content, *inline),
            Event::FontSettings(settings) => tr.apply_font_settings(settings),
            Event::FontInit => tr.font_init(),
            Event::FontClose => tr.font_pop(),
            Event::FontDefault(settings) => tr.font_default(settings),
            Event::MessageLayerSwitch { id, .. } => {
                if let Some(lid) = id {
                    self.scene.ensure(lid);
                }
                tr.switch_message_layer(id.as_deref());
            }
            Event::MessageLayerPop => tr.pop_message_layer(),
            Event::LineBreak => tr.push_line_break(),
            Event::PageBreak { backlog } => tr.push_page_break(*backlog),
            Event::GlyphConfig(config) => tr.set_glyph_config(config),
            Event::TextAnimation(params) => {
                tr.set_scetween(scetween_from_params(params));
            }
            Event::SceneIn => tr.show_text(),
            Event::SceneOut => tr.hide_text(),
            _ => {}
        }
    }

    /// 命中测试：返回舞台坐标 (x, y) 处最上层、可接收指针输入的图层 ID。
    ///
    /// Artemis 的命中是「单次取最上层」的：找到顶端的可交互图层后，宿主再按事件
    /// 类型（click/rollover/...）去它的 `event_handlers` 取处理器——并**不**分事件
    /// 类型各做一次命中。
    ///
    /// 「可交互」需同时满足：visible != false、注册了至少一个事件处理器、且未被
    /// `clickablethreshold` 判为透明。`clickablethreshold` 是 Artemis 的指针命中
    /// 阈值：图层有效 alpha 低于该阈值时对指针透明（不吃事件）。tablet 的
    /// `mw.zmask` 正是一张 alpha=0、clickablethreshold=128 的全宽透明遮罩，只用于
    /// 程序化拖拽，不应拦截落到其下工具栏按钮的 hover/click——靠这个阈值放行。
    ///
    /// 命中用图层 left/top/width/height 做 AABB 判定。没有可推断宽高的纯分组节点跳过。
    pub fn hit_test(&self, x: f32, y: f32, provider: &mut dyn TextureProvider) -> Option<String> {
        let roots = self.scene.roots();
        let scale = self.stage_scale;
        for root in roots.iter().rev() {
            if let Some(hit) = self.hit_test_subtree(root, 0.0, 0.0, x, y, scale, provider) {
                return Some(hit);
            }
        }
        None
    }

    fn hit_test_subtree(
        &self,
        id: &str,
        parent_x: f32,
        parent_y: f32,
        mx: f32,
        my: f32,
        scale: f32,
        provider: &mut dyn TextureProvider,
    ) -> Option<String> {
        let layer = self.scene.get(id)?;
        let props = &layer.props;

        if props.visible == Some(false) {
            return None;
        }

        let (lx, ly) = props.offset();
        let abs_x = parent_x + lx;
        let abs_y = parent_y + ly;

        // 先递归检测子层（高 z-order 优先，reverse 遍历）。
        // 注意按 Artemis 图层顺序排序（与绘制次序一致），不能用原始插入顺序，
        // 否则命中的 z-order 与画面不符。
        let children = self.scene.children(id);
        for child_id in children.iter().rev() {
            if let Some(hit) =
                self.hit_test_subtree(child_id, abs_x, abs_y, mx, my, scale, provider)
            {
                return Some(hit);
            }
        }

        // 再检测本层：注册了任意事件处理器。
        if !layer.event_handlers.is_empty() {
            // 宽高优先级：
            // 1. props.width/height（显式设置的逻辑尺寸）
            // 2. clip 的宽高（精灵表裁剪区域，已经是逻辑坐标）
            // 3. 纹理物理尺寸 / scale（整张纹理的逻辑尺寸）
            let (w, h) = if let (Some(w), Some(h)) = (props.width, props.height) {
                (w, h)
            } else if let Some(clip) = props.clip_rect() {
                // clip = [x, y, w, h]，取 w 和 h
                (clip[2], clip[3])
            } else if let Some(file) = &layer.file {
                if let Some((_, info)) = provider.resolve(file) {
                    (info.width as f32 / scale, info.height as f32 / scale)
                } else {
                    return None;
                }
            } else {
                return None;
            };

            if mx >= abs_x && mx < abs_x + w && my >= abs_y && my < abs_y + h {
                // 检查 clickablethreshold：当坐标处像素 alpha 低于阈值时，指针穿透。
                if !self.is_pointer_transparent_at(
                    props,
                    mx,
                    my,
                    abs_x,
                    abs_y,
                    scale,
                    w,
                    h,
                    provider,
                    &layer.file,
                ) {
                    return Some(id.to_string());
                }
            }
        }

        None
    }

    /// 按 `clickablethreshold` 判断图层在指定坐标处是否对指针透明。
    ///
    /// Artemis 的 `clickablethreshold` 是指针命中的 alpha 阈值：**坐标处的像素
    /// alpha**（乘以图层 alpha 后的有效 alpha）低于阈值时，指针穿透该图层。
    /// 例如，圆形按钮四角的透明像素 alpha=0，低于阈值 128，点击穿透；中心像素
    /// alpha=255，高于阈值，点击被该图层接收。
    ///
    /// 未设 `clickablethreshold` 的图层一律可点（默认行为）。
    fn is_pointer_transparent_at(
        &self,
        props: &crate::compositor::props::LayerProps,
        mx: f32,
        my: f32,
        abs_x: f32,
        abs_y: f32,
        scale: f32,
        _layer_w: f32,
        _layer_h: f32,
        provider: &mut dyn crate::compositor::renderer::TextureProvider,
        file: &Option<String>,
    ) -> bool {
        let Some(threshold) = props
            .custom
            .get("clickablethreshold")
            .and_then(|v| v.trim().parse::<i32>().ok())
        else {
            return false;
        };

        // 计算该点在纹理中的局部像素坐标。
        // mx, my 是舞台坐标；abs_x, abs_y 是图层左上角的舞台坐标；
        // scale 是舞台到物理像素的缩放因子。
        let local_x = ((mx - abs_x) * scale) as u32;
        let local_y = ((my - abs_y) * scale) as u32;

        // 加上 clip 偏移（如果图层有 clip 属性）。
        let (tex_x, tex_y) = if let Some(clip) = props.clip_rect() {
            (local_x + clip[0] as u32, local_y + clip[1] as u32)
        } else {
            (local_x, local_y)
        };

        // 采样纹理像素 alpha。
        let pixel_alpha = if let Some(file) = file {
            provider
                .resolve(file)
                .and_then(|(tid, _)| provider.pixel_alpha(tid, tex_x, tex_y))
        } else {
            None
        };

        // 有效 alpha = 像素 alpha × 图层 alpha / 255。
        let layer_alpha = props.alpha.unwrap_or(255) as i32;
        let effective_alpha = match pixel_alpha {
            Some(pa) => (pa as i32) * layer_alpha / 255,
            None => layer_alpha, // 无法采样时只用图层 alpha
        };

        effective_alpha < threshold
    }

    /// 仅构建当前帧的绘制列表（不渲染），供测试或自定义循环使用。
    pub fn build(&self, provider: &mut dyn TextureProvider) -> DrawList {
        let text_map: std::collections::HashMap<String, Vec<DrawCommand>> = {
            let mut tr_opt = self.text_renderer.borrow_mut();
            match tr_opt.as_mut() {
                Some(tr) => {
                    // 兜底揭示：本帧可能先 advance 再推文本，此时 advance_reveal
                    // 已执行但新文本尚未到达，故在渲染前再推进一次（delta=0 只会
                    // 把刚推入的首个字符设为可见，不会重复计算已逝时间）。
                    tr.advance_reveal(0);
                    tr.build_text_commands(provider)
                }
                None => std::collections::HashMap::new(),
            }
        };
        let text_for: Option<&dyn Fn(&str) -> Vec<DrawCommand>> = if text_map.is_empty() {
            None
        } else {
            Some(&|lid: &str| text_map.get(lid).cloned().unwrap_or_default())
        };
        build_frame(&self.scene, self.clock_ms, provider, text_for)
    }
}

fn parse_num(value: &str) -> Option<f32> {
    value.trim().parse().ok()
}

fn default_param_value(param: &str) -> f32 {
    match param {
        "xscale" | "yscale" | "zoom" => 100.0,
        "alpha" => 255.0,
        _ => 0.0,
    }
}

/// 把缓动终值格式化回属性字符串（整数属性按整数）。
fn format_param(param: &str, value: f32) -> String {
    match param {
        "alpha" | "visible" | "reversex" | "reversey" | "grayscale" | "negative" | "delete"
        | "vertical" | "hung" | "anchorcenter" | "overflow" => (value.round() as i64).to_string(),
        _ => value.to_string(),
    }
}

/// 从 `TextAnimation` 事件的参数 map 构建 [`ScetweenConfig`]。
fn scetween_from_params(params: &HashMap<String, String>) -> ScetweenConfig {
    let mode = params
        .get("type")
        .map(|s| ScetweenMode::from_str(s))
        .unwrap_or(ScetweenMode::In);

    let set_mode = match params.get("mode").map(|s| s.as_str()) {
        Some("add") => ScetweenSetMode::Add,
        _ => ScetweenSetMode::Init,
    };

    let param = params.get("param").cloned();
    let ease = Easing::parse(params.get("ease").map(|s| s.as_str()).unwrap_or(""));
    let diff = params.get("diff").and_then(|v| v.parse().ok());
    let delay_per_char = params
        .get("delay")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let time_per_char = params.get("time").and_then(|v| v.parse().ok()).unwrap_or(0);
    let random_delay = params.get("randomdelay").map(|v| v == "1").unwrap_or(false);

    ScetweenConfig {
        mode,
        set_mode,
        param,
        ease,
        diff,
        delay_per_char,
        time_per_char,
        random_delay,
        random_order: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::mock::MockProvider;
    use asb_interpreter::event::LayerEvent;

    fn create(id: &str, file: &str) -> Event {
        Event::Layer(LayerEvent::Create {
            id: id.into(),
            file: file.into(),
        })
    }

    #[test]
    fn create_and_delete_via_events() {
        let mut c = Compositor::new();
        c.apply_event(&create("1", "bg"));
        c.apply_event(&create("1.0", "fg"));
        assert_eq!(c.scene().len(), 2);

        c.apply_event(&Event::Layer(LayerEvent::Delete { id: "1".into() }));
        assert!(c.scene().is_empty());
    }

    #[test]
    fn create2_applies_alpha() {
        let mut c = Compositor::new();
        c.apply_event(&Event::Layer(LayerEvent::Create2 {
            id: "1".into(),
            file: "bg".into(),
            alpha: Some(128),
        }));
        assert_eq!(c.scene().get("1").unwrap().props.alpha, Some(128));
    }

    #[test]
    fn set_properties_event_merges() {
        let mut c = Compositor::new();
        c.apply_event(&create("1", "bg"));
        let mut props = HashMap::new();
        props.insert("left".to_string(), "50".to_string());
        props.insert("alpha".to_string(), "200".to_string());
        c.apply_event(&Event::Layer(LayerEvent::SetProperties {
            id: "1".into(),
            properties: props,
        }));
        let p = &c.scene().get("1").unwrap().props;
        assert_eq!(p.left, Some(50.0));
        assert_eq!(p.alpha, Some(200));
    }

    #[test]
    fn rename_event_moves_layer() {
        let mut c = Compositor::new();
        c.apply_event(&create("1.0", "a"));
        c.apply_event(&Event::LayerRename {
            id: "1.0".into(),
            to: "1.5".into(),
        });
        assert!(c.scene().get("1.0").is_none());
        assert_eq!(c.scene().get("1.5").unwrap().file.as_deref(), Some("a"));
    }

    #[test]
    fn tween_event_drives_value_then_settles() {
        let mut c = Compositor::new();
        c.apply_event(&create("1", "a"));
        c.apply_event(&Event::LayerTween {
            id: "1".into(),
            param: "alpha".into(),
            from: Some("0".into()),
            to: Some("255".into()),
            ease: None,
            time: Some(1000),
            delay: None,
            loop_count: None,
            yoyo: None,
            loop_delay: None,
            sync: false,
            delete: false,
            handler_file: None,
            handler_label: None,
            handler_handler: None,
        });

        // 推进到中点，缓动仍在进行。
        c.advance(500);
        let mut provider = MockProvider::new();
        let frame = c.build(&mut provider);
        assert!((frame.commands[0].opacity - 0.5).abs() < 0.02);

        // 推进到结束，缓动被回收且终值固化到属性。
        c.advance(600);
        assert!(c.scene().get("1").unwrap().tweens.is_empty());
        assert_eq!(c.scene().get("1").unwrap().props.alpha, Some(255));
    }

    #[test]
    fn ignores_unrelated_events() {
        let mut c = Compositor::new();
        c.apply_event(&create("1", "a"));
        // 文本/音频等事件不应改变场景。
        c.apply_event(&Event::Text {
            content: "hello".into(),
        });
        c.apply_event(&Event::StopAllSounds { duration: 0 });
        assert_eq!(c.scene().len(), 1);
    }

    #[test]
    fn reset_for_load_clears_scene_input_and_audio() {
        let mut c = Compositor::new();
        c.set_audio_backend(Box::new(crate::audio::StubAudioBackend::new()));
        c.apply_event(&create("title", "title_bg"));
        c.apply_event(&Event::SetEventHandler {
            event_name: "push".into(),
            file: None,
            label: None,
            call: false,
            handler: Some("calllua".into()),
            extra_params: HashMap::from([("key".into(), "1".into())]),
        });
        c.apply_event(&Event::BgmPlay {
            file: "title.ogg".into(),
            loop_play: true,
            gain: None,
            pan: None,
            fade_time: None,
        });

        c.reset_for_load();

        assert!(c.scene().is_empty());
        assert!(c.get_input_handler("push", "1").is_none());
        let audio = c.audio_mut();
        let state = audio.as_ref().unwrap().audio_state();
        assert!(state.bgm_channel.is_none());
    }

    #[test]
    fn audio_snapshot_replays_active_channels_from_start() {
        let mut c = Compositor::new();
        c.set_audio_backend(Box::new(crate::audio::StubAudioBackend::new()));
        c.apply_event(&Event::BgmPlay {
            file: "bgm/scene.ogg".into(),
            loop_play: true,
            gain: Some(700),
            pan: Some(-100),
            fade_time: Some(1000),
        });
        c.apply_event(&Event::SePlay {
            id: "amb".into(),
            file: "se/wind.ogg".into(),
            loop_play: true,
            gain: Some(400),
            pan: Some(100),
            fade_time: Some(500),
            skippable: true,
        });
        c.advance(250);

        let snapshot = c.audio_snapshot();
        assert_eq!(snapshot.bgm.as_ref().unwrap().file, "bgm/scene.ogg");
        assert_eq!(snapshot.bgm.as_ref().unwrap().gain, 700);
        assert_eq!(snapshot.se[0].file, "se/wind.ogg");
        assert!(snapshot.se[0].skippable);

        c.reset_for_load();
        c.restore_audio(&snapshot);

        let audio = c.audio_mut();
        let state = audio.as_ref().unwrap().audio_state();
        let bgm = state.bgm_channel.as_ref().unwrap();
        assert_eq!(bgm.file, "bgm/scene.ogg");
        assert_eq!(bgm.current_gain, crate::audio::SoundChannel::gain_to_linear(700));
        assert!(bgm.fade.is_none());
        let se = state.se_channels.get("amb").unwrap();
        assert_eq!(se.file, "se/wind.ogg");
        assert_eq!(se.current_gain, crate::audio::SoundChannel::gain_to_linear(400));
        assert!(se.skippable);
        assert!(se.fade.is_none());
    }

    #[test]
    fn tween_default_from_uses_current_value() {
        let mut c = Compositor::new();
        c.apply_event(&create("1", "a"));
        // 当前 left=100，from 省略，应从 100 缓动到 0。
        let mut props = HashMap::new();
        props.insert("left".to_string(), "100".to_string());
        c.apply_event(&Event::Layer(LayerEvent::SetProperties {
            id: "1".into(),
            properties: props,
        }));
        c.apply_event(&Event::LayerTween {
            id: "1".into(),
            param: "left".into(),
            from: None,
            to: Some("0".into()),
            ease: None,
            time: Some(1000),
            delay: None,
            loop_count: None,
            yoyo: None,
            loop_delay: None,
            sync: false,
            delete: false,
            handler_file: None,
            handler_label: None,
            handler_handler: None,
        });
        let t = &c.scene().get("1").unwrap().tweens[0];
        assert_eq!(t.from, 100.0);
        assert_eq!(t.to, 0.0);
    }
}
