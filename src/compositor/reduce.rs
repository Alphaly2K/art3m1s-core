//! 事件归约：把解释器的 [`Event`] 流应用到合成器状态上。
//!
//! [`Compositor`] 持有一棵 [`Scene`] 和一个合成器时钟。它消费解释器在 `run` 过程
//! 中通过回调发出的 `Event`，把与画面相关的变体（图层增删改、缓动、转场）落到
//! 场景树上；与画面无关的变体（音频、存档、文本…）忽略，留给引擎别的子系统。
//!
//! 时间推进与渲染分离：解释器只管"发生了什么"，宿主把合成器状态交给顶层
//! [`crate::render_pipeline::RenderPipeline`] 进入后续渲染管线。

use crate::compositor::anim::{self, AnimeState, TweenHandler};
use crate::compositor::events::{CompositorEvent, IntoCompositorEvent};
use crate::compositor::scene::{LayerEventHandler, Scene};
use crate::render_pipeline::transition::{self, TransitionState};
use asb_interpreter::event::LayerEvent;
use std::cell::RefCell;
use std::collections::HashMap;

mod hit_test;

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
    pub(crate) scene: Scene,
    /// 合成器时钟（毫秒），缓动与转场都基于它。
    pub(crate) clock_ms: u64,
    /// 舞台到物理像素的缩放因子（HiDPI）。
    pub(super) stage_scale: f32,
    /// 输入事件处理器注册表，按 (event_name, key) 索引。
    pub(super) input_handlers: HashMap<(String, String), InputHandler>,
    /// 自上次 `poll_tween_events` 以来产生待处理的缓动完成事件。
    pub(super) pending_tween_events: Vec<TweenHandler>,
    /// `[trans]` 转场状态（交叉淡化等）。
    pub(crate) trans_state: RefCell<Option<TransitionState>>,
    /// `[anime]` 帧动画状态，按图层 ID 索引。
    pub(super) anime_states: HashMap<String, AnimeState>,
}

impl std::fmt::Debug for Compositor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compositor")
            .field("scene", &self.scene)
            .field("clock_ms", &self.clock_ms)
            .field("stage_scale", &self.stage_scale)
            .field("input_handlers", &self.input_handlers)
            .field("pending_tween_events", &self.pending_tween_events)
            .finish()
    }
}

impl Default for Compositor {
    fn default() -> Self {
        Self::new_with_stage_size(1280, 720)
    }
}

impl Compositor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn new_with_stage_size(_stage_width: u32, _stage_height: u32) -> Self {
        Self {
            scene: Scene::new(),
            clock_ms: 0,
            stage_scale: 1.0,
            input_handlers: HashMap::new(),
            pending_tween_events: Vec::new(),
            trans_state: RefCell::new(None),
            anime_states: HashMap::new(),
        }
    }
}

impl Compositor {
    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    pub fn scene_snapshot(&self) -> Scene {
        self.scene.clone()
    }

    pub fn ensure_layer(&mut self, id: &str) {
        self.scene.ensure(id);
    }

    pub fn restore_scene(&mut self, scene: Scene) {
        self.scene.replace_with(scene);
        self.pending_tween_events.clear();
    }

    /// 读档是全局状态切换边界：旧画面和旧 UI 输入处理器不能穿透到新存档。
    /// 随后若存档携带 scene，调用方再用 `restore_scene` 覆盖。
    pub fn reset_for_load(&mut self) {
        self.scene.replace_with(Scene::new());
        self.input_handlers.clear();
        self.pending_tween_events.clear();
        *self.trans_state.borrow_mut() = None;
        self.anime_states.clear();
    }

    pub fn clock_ms(&self) -> u64 {
        self.clock_ms
    }

    /// 设置舞台缩放因子（HiDPI scale）。宿主在窗口初始化/缩放变化时调用。
    pub fn set_stage_scale(&mut self, scale: f32) {
        self.stage_scale = scale;
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

        transition::clear_finished(&self.trans_state, self.clock_ms);
        anim::gc_finished_tweens(
            &mut self.scene,
            self.clock_ms,
            &mut self.pending_tween_events,
        );
        anim::update_anime_frames(&mut self.scene, &mut self.anime_states, self.clock_ms);
    }
    ///
    /// 宿主在每帧 `advance` 之后调用，将返回到的 [`TweenHandler`] 交回解释器
    /// 执行（对于 `sync` 缓动则构造 Wait 事件暂停脚本）。
    pub fn poll_tween_events(&mut self) -> Vec<TweenHandler> {
        std::mem::take(&mut self.pending_tween_events)
    }

    /// 把一个视觉/交互事件应用到场景上。
    pub fn apply_event<'a>(&mut self, event: impl IntoCompositorEvent<'a>) {
        let Some(event) = event.into_compositor_event() else {
            return;
        };
        self.apply_compositor_event(event);
    }

    fn apply_compositor_event(&mut self, event: CompositorEvent<'_>) {
        match event {
            CompositorEvent::Layer(layer_event) => self.apply_layer_event(layer_event),
            CompositorEvent::LayerRename { id, to } => {
                self.scene.rename(id, to);
            }
            CompositorEvent::LayerTween {
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
            } => anim::apply_tween(
                &mut self.scene,
                self.clock_ms,
                anim::TweenRequest {
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
                },
            ),
            CompositorEvent::LayerTweenDelete { id } => {
                // 强制完成：把该图层所有缓动直接落到终值并清空。
                anim::finish_tweens(&mut self.scene, id);
            }
            CompositorEvent::LayerEventHandler {
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
                    match mode {
                        "reset" | "disable" => {
                            layer.event_handlers.remove(event_type);
                        }
                        // "init"、"enable" 以及未指定都视为注册。
                        _ => {
                            layer.event_handlers.insert(
                                event_type.to_string(),
                                LayerEventHandler {
                                    handler: handler.map(str::to_string),
                                    file: file.map(str::to_string),
                                    label: label.map(str::to_string),
                                    call,
                                    penetration,
                                    params: extra_params.clone(),
                                },
                            );
                        }
                    }
                }
            }
            // 输入事件处理器注册（setonpush 等 seton* 标签）。
            CompositorEvent::SetInputHandler {
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
                    (event_name.to_string(), key),
                    InputHandler {
                        handler: handler.map(str::to_string),
                        file: file.map(str::to_string),
                        label: label.map(str::to_string),
                        call,
                        params: extra_params.clone(),
                    },
                );
            }
            CompositorEvent::DelInputHandler { event_name, key } => {
                if let Some(key) = key {
                    self.input_handlers
                        .remove(&(event_name.to_string(), key.to_string()));
                } else {
                    self.input_handlers
                        .retain(|(name, _), _| name != event_name);
                }
            }
            // ── 帧动画 ──
            CompositorEvent::Anime {
                id,
                mode,
                file,
                mask,
                time,
                loop_count,
                props,
            } => anim::apply_anime_event(
                &mut self.scene,
                &mut self.anime_states,
                self.clock_ms,
                anim::AnimeRequest {
                    id,
                    mode,
                    file,
                    mask,
                    time,
                    loop_count,
                    props,
                },
            ),
            // ── 转场 ──
            CompositorEvent::Trans {
                trans_type,
                time,
                rule,
                vague,
                input,
            } => {
                transition::start(
                    &self.trans_state,
                    self.clock_ms,
                    transition::TransitionRequest {
                        trans_type,
                        time,
                        rule,
                        vague,
                        input,
                    },
                );
            }
            // ── Flip 即刻提交 ──
            CompositorEvent::Flip => {
                transition::clear(&self.trans_state);
            }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::mock::MockProvider;
    use asb_interpreter::Event;
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
        let frame = crate::render_pipeline::RenderPipeline::new(&c).build(&mut provider);
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
    fn reset_for_load_clears_scene_and_input() {
        let mut c = Compositor::new();
        c.apply_event(&create("title", "title_bg"));
        c.apply_event(&Event::SetEventHandler {
            event_name: "push".into(),
            file: None,
            label: None,
            call: false,
            handler: Some("calllua".into()),
            extra_params: HashMap::from([("key".into(), "1".into())]),
        });

        c.reset_for_load();

        assert!(c.scene().is_empty());
        assert!(c.get_input_handler("push", "1").is_none());
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
