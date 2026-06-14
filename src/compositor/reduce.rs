//! 事件归约：把解释器的 [`Event`] 流应用到合成器状态上。
//!
//! [`Compositor`] 持有一棵 [`Scene`] 和一个合成器时钟。它消费解释器在 `run` 过程
//! 中通过回调发出的 `Event`，把与画面相关的变体（图层增删改、缓动、转场）落到
//! 场景树上；与画面无关的变体（音频、存档、文本…）忽略，留给引擎别的子系统。
//!
//! 时间推进与渲染分离：解释器只管"发生了什么"，由宿主每帧调用 [`Compositor::render`]
//! 把当前时刻的场景画出来。

use crate::compositor::anim::{Easing, Tween};
use crate::compositor::build::build_frame;
use crate::compositor::renderer::{DrawList, Renderer, TextureProvider};
use crate::compositor::scene::{LayerEventHandler, Scene};
use asb_interpreter::event::{Event, LayerEvent};
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
#[derive(Debug, Default)]
pub struct Compositor {
    scene: Scene,
    /// 合成器时钟（毫秒），缓动与转场都基于它。
    clock_ms: u64,
    /// 舞台到物理像素的缩放因子（HiDPI）。
    /// 纹理尺寸是物理像素，props 的 left/top 是逻辑坐标，
    /// 命中测试需要把纹理尺寸除以这个值才能与逻辑鼠标坐标对齐。
    stage_scale: f32,
    /// 输入事件处理器注册表，按 (event_name, key) 索引。
    /// key 来自 setonpush 标签的参数（如 "1" 表示鼠标左键）。
    input_handlers: HashMap<(String, String), InputHandler>,
}

impl Compositor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    pub fn clock_ms(&self) -> u64 {
        self.clock_ms
    }

    /// 设置舞台缩放因子（HiDPI scale）。宿主在窗口初始化/缩放变化时调用。
    pub fn set_stage_scale(&mut self, scale: f32) {
        self.stage_scale = scale.max(1.0);
    }

    pub fn stage_scale(&self) -> f32 {
        self.stage_scale
    }

    /// 查询指定事件/键组合的已注册处理器。宿主在检测到输入后调用。
    pub fn get_input_handler(&self, event_name: &str, key: &str) -> Option<&InputHandler> {
        self.input_handlers.get(&(event_name.to_string(), key.to_string()))
    }

    /// 推进合成器时钟。宿主每帧用累计的真实时间调用一次。
    pub fn advance(&mut self, delta_ms: u64) {
        self.clock_ms = self.clock_ms.saturating_add(delta_ms);
        self.gc_finished_tweens();
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
                ..
            } => self.apply_tween(id, param, from.as_deref(), to.as_deref(), ease.as_deref(), *time),
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
                    // 只移除指定 key 的处理器
                    self.input_handlers.remove(&(event_name.clone(), key.clone()));
                } else {
                    // 移除该事件类型的所有处理器
                    self.input_handlers.retain(|(name, _), _| name != event_name);
                }
            }
            // 其余事件（音频、文本、存档、系统 UI…）不影响图层合成。
            _ => {}
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
    fn apply_tween(
        &mut self,
        id: &str,
        param: &str,
        from: Option<&str>,
        to: Option<&str>,
        ease: Option<&str>,
        time: Option<u64>,
    ) {
        // 没有可解析的目标值，缓动无意义，直接忽略。
        let Some(to_value) = to.and_then(parse_num) else {
            return;
        };

        // 确保图层存在，再读出起始值（from 省略时取属性当前值）。
        self.scene.ensure(id);
        let from_value = from
            .and_then(parse_num)
            .unwrap_or_else(|| self.current_param_value(id, param));

        let tween = Tween {
            param: param.to_string(),
            from: from_value,
            to: to_value,
            easing: ease.map(Easing::parse).unwrap_or_default(),
            start_ms: self.clock_ms,
            duration_ms: time.unwrap_or(0),
        };

        if let Some(layer) = self.scene.get_mut(id) {
            // 同一属性的旧缓动被新的取代，避免叠加冲突。
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
            "xscale" => p.x_scale.unwrap_or(100.0),
            "yscale" => p.y_scale.unwrap_or(100.0),
            "rotate" => p.rotate.unwrap_or(0.0),
            "alpha" => p.alpha.unwrap_or(255) as f32,
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

    /// 回收已结束的缓动，把终值固化到属性里。每帧 `advance` 调用。
    fn gc_finished_tweens(&mut self) {
        let now = self.clock_ms;
        // 先收集需要固化的 (id, param, value)，避免借用冲突。
        let mut settle: Vec<(String, String, f32)> = Vec::new();
        let ids: Vec<String> = self.scene.iter_ids();
        for id in &ids {
            if let Some(layer) = self.scene.get(id) {
                for t in &layer.tweens {
                    if t.is_finished(now) {
                        settle.push((id.clone(), t.param.clone(), t.to));
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
    }

    /// 用当前时刻构建一帧并交给后端渲染。
    pub fn render(&self, renderer: &mut dyn Renderer, provider: &mut dyn TextureProvider) {
        let frame = self.build(provider);
        renderer.render(&frame);
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
    pub fn hit_test(
        &self,
        x: f32,
        y: f32,
        provider: &mut dyn TextureProvider,
    ) -> Option<String> {
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
            if let Some(hit) = self.hit_test_subtree(child_id, abs_x, abs_y, mx, my, scale, provider)
            {
                return Some(hit);
            }
        }

        // 再检测本层：注册了任意事件处理器、且未被 clickablethreshold 判为透明。
        if !layer.event_handlers.is_empty() && !self.is_pointer_transparent(props) {
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
                return Some(id.to_string());
            }
        }

        None
    }

    /// 按 `clickablethreshold` 判断图层是否对指针透明（不接收 hover/click）。
    ///
    /// Artemis 的 `clickablethreshold` 是指针命中的 alpha 阈值：被点处的有效
    /// alpha 低于阈值时，指针穿透该图层。我们没有逐像素 alpha，但用图层级
    /// alpha 近似——这足以放行像 `mw.zmask` 这种整层 alpha=0 的全透明拖拽遮罩
    /// （threshold=128 而 alpha=0 → 透明）。未设阈值的图层一律可点（默认行为不变）。
    fn is_pointer_transparent(&self, props: &crate::compositor::props::LayerProps) -> bool {
        let Some(threshold) = props
            .custom
            .get("clickablethreshold")
            .and_then(|v| v.trim().parse::<i32>().ok())
        else {
            return false;
        };
        let alpha = props.alpha.unwrap_or(255) as i32;
        alpha < threshold
    }

    /// 仅构建当前帧的绘制列表（不渲染），供测试或自定义循环使用。
    pub fn build(&self, provider: &mut dyn TextureProvider) -> DrawList {
        build_frame(&self.scene, self.clock_ms, provider)
    }
}

fn parse_num(value: &str) -> Option<f32> {
    value.trim().parse().ok()
}

fn default_param_value(param: &str) -> f32 {
    match param {
        "xscale" | "yscale" => 100.0,
        "alpha" => 255.0,
        _ => 0.0,
    }
}

/// 把缓动终值格式化回属性字符串（整数属性按整数）。
fn format_param(param: &str, value: f32) -> String {
    match param {
        "alpha" | "visible" => (value.round() as i64).to_string(),
        _ => value.to_string(),
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
