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

/// 后端无关的合成器：场景树 + 时钟 + 事件归约。
#[derive(Debug, Default)]
pub struct Compositor {
    scene: Scene,
    /// 合成器时钟（毫秒），缓动与转场都基于它。
    clock_ms: u64,
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
