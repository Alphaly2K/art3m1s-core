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
use crate::compositor::scene::Scene;
use asb_interpreter::event::{Event, LayerEvent};
use std::collections::HashMap;

/// 已注册的输入事件处理器。
///
/// 由 Lua 脚本通过 `e:tag{"setonpush", key=..., handler="calllua", function="..."}` 注册。
/// 宿主在检测到相应输入（鼠标点击、按键）时查找并调用。
#[derive(Debug, Clone)]
pub struct InputHandler {
    /// Lua 回调函数名（来自标签的 `function` 字段）。
    pub function: String,
    /// 注册时附带的元数据（key、adv、ui、btn 等），触发时作为 param 表传给 Lua。
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
            Event::LayerEventHandler { id, event_type, click, over, out, extra_params, mode, .. } => {
                // mode="disable" 表示移除该事件类型的回调，mode="init" 表示注册。
                let disabling = mode.as_str() == "disable";
                let et = event_type.as_str();

                self.scene.ensure(id);
                if let Some(layer) = self.scene.get_mut(id) {
                    // 方式一：直接带 click/over/out 字段（参数直接传）
                    // 方式二：通过 e:tag{..., type="click", ["function"]="fn_name"} 调用
                    // Lua 的 lyevent() 会把 click/over/out 各拆成一个 e:tag 调用，
                    // tcopy 会带上原始的 click/over/out 字段，所以必须按 event_type
                    // 过滤，避免 rollover 事件里的 click="btn_click" 覆盖 click_lua_fn。
                    let fn_name = extra_params.get("function").cloned();
                    match et {
                        "click" => {
                            // function 字段优先（lyevent 桥接用它覆盖原函数名），
                            // 没有则回退到 click 字段（直接形式）
                            let f = fn_name.as_ref().or(click.as_ref());
                            if disabling { layer.click_lua_fn = None; }
                            else if let Some(f) = f { layer.click_lua_fn = Some(f.clone()); }
                        }
                        "rollover" => {
                            let f = fn_name.as_ref().or(over.as_ref());
                            if disabling { layer.over_lua_fn = None; }
                            else if let Some(f) = f { layer.over_lua_fn = Some(f.clone()); }
                        }
                        "rollout" => {
                            let f = fn_name.as_ref().or(out.as_ref());
                            if disabling { layer.out_lua_fn = None; }
                            else if let Some(f) = f { layer.out_lua_fn = Some(f.clone()); }
                        }
                        _ => {
                            // event_type 未指定（直接形式），各字段对应各回调
                            if let Some(f) = click {
                                if disabling { layer.click_lua_fn = None; }
                                else { layer.click_lua_fn = Some(f.clone()); }
                            }
                            if let Some(f) = over {
                                if disabling { layer.over_lua_fn = None; }
                                else { layer.over_lua_fn = Some(f.clone()); }
                            }
                            if let Some(f) = out {
                                if disabling { layer.out_lua_fn = None; }
                                else { layer.out_lua_fn = Some(f.clone()); }
                            }
                        }
                    }

                    // 把 name/key/se 等按钮元数据存入，供回调使用
                    for (k, v) in extra_params {
                        if k != "function" {
                            layer.event_params.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
            // 输入事件处理器注册（setonpush 等）
            Event::SetEventHandler { event_name, handler, extra_params, .. } => {
                // 只处理 calllua 类型的 push 事件处理器
                if handler.as_deref() == Some("calllua") {
                    if let Some(func) = extra_params.get("function") {
                        // key 字段标识处理器响应的按键/输入（"1" = 鼠标左键）
                        let key = extra_params.get("key").cloned().unwrap_or_default();
                        self.input_handlers.insert(
                            (event_name.clone(), key),
                            InputHandler {
                                function: func.clone(),
                                params: extra_params.clone(),
                            },
                        );
                    }
                }
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

    /// 命中测试：返回舞台坐标 (x, y) 处最上层有 click 回调的图层信息。
    ///
    /// 返回 `(click_fn, over_fn, out_fn, extra_params)`。
    /// 只考虑 visible != false 的图层，使用图层 left/top/width/height 做 AABB 判定。
    /// 没有 width/height 时跳过（纯分组节点）。
    pub fn hit_test(
        &self,
        x: f32,
        y: f32,
        provider: &mut dyn TextureProvider,
    ) -> Option<(String, Option<String>, Option<String>, std::collections::HashMap<String, String>)>
    {
        self.hit_test_with_id(x, y, provider)
            .map(|(_id, click, over, out, params)| (click, over, out, params))
    }

    /// 命中测试 + 图层 ID。用于 hover 跟踪：宿主用 ID 判断鼠标进出。
    pub fn hit_test_with_id(
        &self,
        x: f32,
        y: f32,
        provider: &mut dyn TextureProvider,
    ) -> Option<(String, String, Option<String>, Option<String>, std::collections::HashMap<String, String>)>
    {
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
    ) -> Option<(String, String, Option<String>, Option<String>, std::collections::HashMap<String, String>)>
    {
        let layer = self.scene.get(id)?;
        let props = &layer.props;

        if props.visible == Some(false) {
            return None;
        }

        let (lx, ly) = props.offset();
        let abs_x = parent_x + lx;
        let abs_y = parent_y + ly;

        // 先递归检测子层（高 z-order 优先，reverse 遍历）
        let children: Vec<String> = layer.children.clone();
        for child_id in children.iter().rev() {
            if let Some(hit) = self.hit_test_subtree(child_id, abs_x, abs_y, mx, my, scale, provider) {
                return Some(hit);
            }
        }

        // 再检测本层
        if layer.click_lua_fn.is_some() {
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
                return Some((
                    id.to_string(),
                    layer.click_lua_fn.clone().unwrap(),
                    layer.over_lua_fn.clone(),
                    layer.out_lua_fn.clone(),
                    layer.event_params.clone(),
                ));
            }
        }

        None
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
