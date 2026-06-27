//! 图层动画：属性缓动（tween）与帧动画（anime）。
//!
//! 解释器把 `[lytween]` 解析成 `Event::LayerTween`，把 `[anime]` 解析成帧动画
//! 事件。这些都是基于时间的：合成器记录起止值与时长，在每帧推进时由本模块
//! 更新图层动画状态。

use crate::compositor::scene::Scene;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 缓动函数。Artemis 的 `ease` 字符串在归约阶段映射到这里。
///
/// 支持全部 30 种 Artemis 标准缓动函数，按数学家族分为 10 组，每组含
/// in / out / inout 三种方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Easing {
    #[default]
    Linear,
    // Quadratic (power 2)
    EaseInQuad,
    EaseOutQuad,
    EaseInOutQuad,
    // Cubic (power 3)
    EaseInCubic,
    EaseOutCubic,
    EaseInOutCubic,
    // Quartic (power 4)
    EaseInQuart,
    EaseOutQuart,
    EaseInOutQuart,
    // Quintic (power 5)
    EaseInQuint,
    EaseOutQuint,
    EaseInOutQuint,
    // Exponential
    EaseInExpo,
    EaseOutExpo,
    EaseInOutExpo,
    // Circular
    EaseInCirc,
    EaseOutCirc,
    EaseInOutCirc,
    // Sine
    EaseInSine,
    EaseOutSine,
    EaseInOutSine,
    // Back (overshoot)
    EaseInBack,
    EaseOutBack,
    EaseInOutBack,
    // Elastic
    EaseInElastic,
    EaseOutElastic,
    EaseInOutElastic,
    // Bounce
    EaseInBounce,
    EaseOutBounce,
    EaseInOutBounce,
}

impl Easing {
    /// 把线性进度 `t`（0.0-1.0）映射为缓动后的进度。
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            // Quadratic ──────────────────────────────────────
            Easing::EaseInQuad => t * t,
            Easing::EaseOutQuad => t * (2.0 - t),
            Easing::EaseInOutQuad => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    -1.0 + (4.0 - 2.0 * t) * t
                }
            }
            // Cubic ─────────────────────────────────────────
            Easing::EaseInCubic => t * t * t,
            Easing::EaseOutCubic => {
                let t1 = t - 1.0;
                t1 * t1 * t1 + 1.0
            }
            Easing::EaseInOutCubic => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    let t1 = -2.0 * t + 2.0;
                    1.0 - t1 * t1 * t1 / 2.0
                }
            }
            // Quartic ───────────────────────────────────────
            Easing::EaseInQuart => t * t * t * t,
            Easing::EaseOutQuart => {
                let t1 = t - 1.0;
                1.0 - t1 * t1 * t1 * t1
            }
            Easing::EaseInOutQuart => {
                if t < 0.5 {
                    8.0 * t * t * t * t
                } else {
                    let t1 = -2.0 * t + 2.0;
                    1.0 - t1 * t1 * t1 * t1 / 2.0
                }
            }
            // Quintic ───────────────────────────────────────
            Easing::EaseInQuint => t * t * t * t * t,
            Easing::EaseOutQuint => {
                let t1 = t - 1.0;
                1.0 + t1 * t1 * t1 * t1 * t1
            }
            Easing::EaseInOutQuint => {
                if t < 0.5 {
                    16.0 * t * t * t * t * t
                } else {
                    let t1 = -2.0 * t + 2.0;
                    1.0 - t1 * t1 * t1 * t1 * t1 / 2.0
                }
            }
            // Exponential ───────────────────────────────────
            Easing::EaseInExpo => {
                if t <= 0.0 {
                    0.0
                } else {
                    (2.0_f32).powf(10.0 * t - 10.0)
                }
            }
            Easing::EaseOutExpo => {
                if t >= 1.0 {
                    1.0
                } else {
                    1.0 - (2.0_f32).powf(-10.0 * t)
                }
            }
            Easing::EaseInOutExpo => {
                if t <= 0.0 {
                    0.0
                } else if t >= 1.0 {
                    1.0
                } else if t < 0.5 {
                    (2.0_f32).powf(20.0 * t - 10.0) / 2.0
                } else {
                    (2.0 - (2.0_f32).powf(-20.0 * t + 10.0)) / 2.0
                }
            }
            // Circular ──────────────────────────────────────
            Easing::EaseInCirc => 1.0 - (1.0 - t * t).sqrt(),
            Easing::EaseOutCirc => (1.0 - (t - 1.0) * (t - 1.0)).sqrt(),
            Easing::EaseInOutCirc => {
                if t < 0.5 {
                    (1.0 - (1.0 - (2.0 * t) * (2.0 * t)).sqrt()) / 2.0
                } else {
                    ((1.0 - (-2.0 * t + 2.0) * (-2.0 * t + 2.0)).sqrt() + 1.0) / 2.0
                }
            }
            // Sine ──────────────────────────────────────────
            Easing::EaseInSine => 1.0 - (t * std::f32::consts::FRAC_PI_2).cos(),
            Easing::EaseOutSine => (t * std::f32::consts::FRAC_PI_2).sin(),
            Easing::EaseInOutSine => -((std::f32::consts::PI * t).cos() - 1.0) / 2.0,
            // Back ──────────────────────────────────────────
            Easing::EaseInBack => {
                const C1: f32 = 1.70158;
                const C3: f32 = C1 + 1.0;
                C3 * t * t * t - C1 * t * t
            }
            Easing::EaseOutBack => {
                const C1: f32 = 1.70158;
                const C3: f32 = C1 + 1.0;
                1.0 + C3 * (t - 1.0).powi(3) + C1 * (t - 1.0).powi(2)
            }
            Easing::EaseInOutBack => {
                const C1: f32 = 1.70158;
                const C2: f32 = C1 * 1.525;
                if t < 0.5 {
                    let t2 = 2.0 * t;
                    (t2 * t2 * ((C2 + 1.0) * t2 - C2)) / 2.0
                } else {
                    let t2 = 2.0 * t - 2.0;
                    (t2 * t2 * ((C2 + 1.0) * t2 + C2)) / 2.0 + 1.0
                }
            }
            // Elastic ───────────────────────────────────────
            Easing::EaseInElastic => {
                if t <= 0.0 || t >= 1.0 {
                    return t;
                }
                const C4: f32 = 2.0 * std::f32::consts::PI / 3.0;
                -(2.0_f32).powf(10.0 * t - 10.0) * ((t * 10.0 - 10.75) * C4).sin()
            }
            Easing::EaseOutElastic => {
                if t <= 0.0 || t >= 1.0 {
                    return t;
                }
                const C4: f32 = 2.0 * std::f32::consts::PI / 3.0;
                (2.0_f32).powf(-10.0 * t) * ((t * 10.0 - 0.75) * C4).sin() + 1.0
            }
            Easing::EaseInOutElastic => {
                if t <= 0.0 || t >= 1.0 {
                    return t;
                }
                const C5: f32 = 2.0 * std::f32::consts::PI / 4.5;
                if t < 0.5 {
                    -(2.0_f32).powf(20.0 * t - 10.0) * ((20.0 * t - 11.125) * C5).sin() / 2.0
                } else {
                    (2.0_f32).powf(-20.0 * t + 10.0) * ((20.0 * t - 11.125) * C5).sin() / 2.0 + 1.0
                }
            }
            // Bounce ────────────────────────────────────────
            Easing::EaseInBounce => 1.0 - Easing::EaseOutBounce.apply(1.0 - t),
            Easing::EaseOutBounce => {
                const N1: f32 = 7.5625;
                const D1: f32 = 2.75;
                if t < 1.0 / D1 {
                    N1 * t * t
                } else if t < 2.0 / D1 {
                    let t1 = t - 1.5 / D1;
                    N1 * t1 * t1 + 0.75
                } else if t < 2.5 / D1 {
                    let t1 = t - 2.25 / D1;
                    N1 * t1 * t1 + 0.9375
                } else {
                    let t1 = t - 2.625 / D1;
                    N1 * t1 * t1 + 0.984375
                }
            }
            Easing::EaseInOutBounce => {
                if t < 0.5 {
                    (1.0 - Easing::EaseOutBounce.apply(1.0 - 2.0 * t)) / 2.0
                } else {
                    (1.0 + Easing::EaseOutBounce.apply(2.0 * t - 1.0)) / 2.0
                }
            }
        }
    }

    /// 解析 Artemis 的 ease 名称（标准 30 种 + 旧式简短别名）。
    pub fn parse(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            // 旧式简短别名（兼容旧脚本）
            "in" | "easein" => Easing::EaseInQuad,
            "out" | "easeout" => Easing::EaseOutQuad,
            "inout" | "easeinout" => Easing::EaseInOutQuad,
            // 标准 30 种
            "easein_quad" | "easeinquad" => Easing::EaseInQuad,
            "easeout_quad" | "easeoutquad" => Easing::EaseOutQuad,
            "easeinout_quad" | "easeinoutquad" => Easing::EaseInOutQuad,
            "easein_cubic" | "easeincubic" => Easing::EaseInCubic,
            "easeout_cubic" | "easeoutcubic" => Easing::EaseOutCubic,
            "easeinout_cubic" | "easeinoutcubic" => Easing::EaseInOutCubic,
            "easein_quart" | "easeinquart" => Easing::EaseInQuart,
            "easeout_quart" | "easeoutquart" => Easing::EaseOutQuart,
            "easeinout_quart" | "easeinoutquart" => Easing::EaseInOutQuart,
            "easein_quint" | "easeinquint" => Easing::EaseInQuint,
            "easeout_quint" | "easeoutquint" => Easing::EaseOutQuint,
            "easeinout_quint" | "easeinoutquint" => Easing::EaseInOutQuint,
            "easein_expo" | "easeinexpo" => Easing::EaseInExpo,
            "easeout_expo" | "easeoutexpo" => Easing::EaseOutExpo,
            "easeinout_expo" | "easeinoutexpo" => Easing::EaseInOutExpo,
            "easein_circ" | "easeincirc" => Easing::EaseInCirc,
            "easeout_circ" | "easeoutcirc" => Easing::EaseOutCirc,
            "easeinout_circ" | "easeinoutcirc" => Easing::EaseInOutCirc,
            "easein_sine" | "easeinsine" => Easing::EaseInSine,
            "easeout_sine" | "easeoutsine" => Easing::EaseOutSine,
            "easeinout_sine" | "easeinoutsine" => Easing::EaseInOutSine,
            "easein_back" | "easeinback" => Easing::EaseInBack,
            "easeout_back" | "easeoutback" => Easing::EaseOutBack,
            "easeinout_back" | "easeinoutback" => Easing::EaseInOutBack,
            "easein_elastic" | "easeinelastic" => Easing::EaseInElastic,
            "easeout_elastic" | "easeoutelastic" => Easing::EaseOutElastic,
            "easeinout_elastic" | "easeinoutelastic" => Easing::EaseInOutElastic,
            "easein_bounce" | "easeinbounce" => Easing::EaseInBounce,
            "easeout_bounce" | "easeoutbounce" => Easing::EaseOutBounce,
            "easeinout_bounce" | "easeinoutbounce" => Easing::EaseInOutBounce,
            _ => Easing::Linear,
        }
    }
}

/// 缓动完成后的回调处理器（由 lytween 的 `file` / `label` / `handler` 参数指定）。
///
/// 与 [`crate::compositor::scene::LayerEventHandler`] 同构，当缓动自然完成
/// （非被删除/替换导致的中断）时触发。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TweenHandler {
    pub file: Option<String>,
    pub label: Option<String>,
    pub call: bool,
    pub handler: Option<String>,
}

/// 单个数值属性的缓动。
///
/// 时间用毫秒，与解释器事件一致。`param` 是被缓动的属性名（如 `"alpha"`、
/// `"left"`），由归约阶段从事件填入，build 阶段据此把求值结果写回 `LayerProps`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tween {
    pub param: String,
    pub from: f32,
    pub to: f32,
    pub easing: Easing,
    /// 缓动开始前等待的时间（毫秒）。`start_ms` 已包含此偏移。
    pub start_ms: u64,
    pub duration_ms: u64,
    /// 是否无限循环（`loop="-1"`）。
    pub infinite_loop: bool,
    /// 有限循环剩余次数。None 表示不循环。
    pub loop_count: Option<u32>,
    /// 是否乒乓循环（yoyo: 每次循环交换 from/to）。
    pub yoyo: bool,
    /// 是否是 yoyo 的回程（内部使用）。
    pub yoyo_reverse: bool,
    /// 循环间延迟（毫秒）。
    pub loop_delay_ms: u64,
    /// 缓动完成后是否删除图层（`delete` 参数）。
    pub delete_on_finish: bool,
    /// 缓动完成后的回调处理器（`sync` / `file` / `label` / `handler`）。
    pub handler: Option<TweenHandler>,
}

impl Tween {
    /// 求出给定时刻的当前值。`now` 早于 `start` 时取起始值，超过结束时取终值。
    ///
    /// 支持 delay（`start_ms` 已包含）、loop（循环重置时钟）、yoyo（乒乓反转）。
    pub fn value_at(&self, now_ms: u64) -> f32 {
        if self.duration_ms == 0 {
            return self.to;
        }

        // 尚未到达开始时间（含 delay）
        if now_ms < self.start_ms {
            return self.from;
        }

        let effective_now = self.effective_time(now_ms);
        let (local_from, local_to) = if self.yoyo_reverse {
            (self.to, self.from)
        } else {
            (self.from, self.to)
        };

        if effective_now >= self.duration_ms {
            return local_to;
        }

        let elapsed = effective_now as f32;
        let progress = self.easing.apply(elapsed / self.duration_ms as f32);
        local_from + (local_to - local_from) * progress
    }

    /// 计算在当前循环内已流逝的时间（毫秒）。
    fn effective_time(&self, now_ms: u64) -> u64 {
        let total_cycle = self.duration_ms + self.loop_delay_ms;

        if total_cycle == 0 || !self.is_looping() {
            return (now_ms - self.start_ms).min(self.duration_ms);
        }

        let elapsed = now_ms - self.start_ms;
        let cycle_index = elapsed / total_cycle;

        // 有限循环：超过总次数则固定为终值
        if let Some(max) = self.loop_count {
            if cycle_index >= max as u64 {
                return self.duration_ms;
            }
        }

        let intra = elapsed % total_cycle;
        intra.min(self.duration_ms)
    }

    /// 当前是否处于循环中。
    fn is_looping(&self) -> bool {
        self.infinite_loop || self.loop_count.is_some()
    }

    /// 缓动是否已彻底结束（含所有循环）。
    pub fn is_finished(&self, now_ms: u64) -> bool {
        if self.infinite_loop {
            return false;
        }
        if self.duration_ms == 0 {
            return true;
        }

        if let Some(max) = self.loop_count {
            // N 次循环 = N × duration + (N−1) × loop_delay（最后一次无后续等待）
            let total_duration = self.start_ms
                + max as u64 * self.duration_ms
                + max.saturating_sub(1) as u64 * self.loop_delay_ms;
            return now_ms >= total_duration;
        }

        now_ms >= self.start_ms + self.duration_ms
    }

    /// 本次循环中 yoyo 是否处于反转方向。
    pub fn is_yoyo_reverse(&self, now_ms: u64) -> bool {
        if !self.yoyo || !self.is_looping() {
            return false;
        }
        let total_cycle = self.duration_ms + self.loop_delay_ms;
        if total_cycle == 0 {
            return false;
        }
        let elapsed = now_ms - self.start_ms;
        let cycle_index = elapsed / total_cycle;
        cycle_index % 2 == 1
    }

    /// 获取缓动完成的回调处理器（如果有）。
    pub fn finish_handler(&self) -> Option<&TweenHandler> {
        self.handler.as_ref()
    }
}

/// `[lytween]` 事件归约为动画系统所需的参数。
pub(crate) struct TweenRequest<'a> {
    pub(crate) id: &'a str,
    pub(crate) param: &'a str,
    pub(crate) from: Option<&'a str>,
    pub(crate) to: Option<&'a str>,
    pub(crate) ease: Option<&'a str>,
    pub(crate) time: Option<u64>,
    pub(crate) delay: Option<u64>,
    pub(crate) loop_count: Option<i32>,
    pub(crate) yoyo: Option<i32>,
    pub(crate) loop_delay: Option<u64>,
    pub(crate) sync: bool,
    pub(crate) delete: bool,
    pub(crate) handler_file: Option<&'a str>,
    pub(crate) handler_label: Option<&'a str>,
    pub(crate) handler_handler: Option<&'a str>,
}

/// `[anime]` 帧动画事件归约为动画系统所需的参数。
pub(crate) struct AnimeRequest<'a> {
    pub(crate) id: &'a str,
    pub(crate) mode: &'a str,
    pub(crate) file: Option<&'a str>,
    pub(crate) mask: Option<&'a str>,
    pub(crate) time: Option<u64>,
    pub(crate) loop_count: Option<i32>,
    pub(crate) props: &'a HashMap<String, String>,
}

/// `[anime]` 帧动画的单帧数据。
#[derive(Debug, Clone)]
pub(crate) struct AnimeFrame {
    pub(crate) time_ms: u64,
    pub(crate) file: String,
    #[allow(dead_code)]
    pub(crate) mask: Option<String>,
    pub(crate) props: HashMap<String, String>,
}

/// `[anime]` 图层的帧动画播放状态。
#[derive(Debug, Clone)]
pub(crate) struct AnimeState {
    pub(crate) frames: Vec<AnimeFrame>,
    /// -1=无限循环, 0=不循环(播一次), N=循环 N 次
    pub(crate) loop_count: i32,
    pub(crate) start_ms: u64,
    pub(crate) total_duration_ms: u64,
}

/// 把一个 `[lytween]` 落成图层上的 [`Tween`]。
///
/// `from` 省略时取属性当前值；`to` 解析失败则忽略本次缓动（没有目标无意义）。
pub(crate) fn apply_tween(scene: &mut Scene, clock_ms: u64, request: TweenRequest<'_>) {
    let Some(to_value) = request.to.and_then(parse_num) else {
        return;
    };

    scene.ensure(request.id);
    let from_value = request
        .from
        .and_then(parse_num)
        .unwrap_or_else(|| current_param_value(scene, request.id, request.param));

    let start_ms = clock_ms + request.delay.unwrap_or(0);

    // 解析循环：-1 -> 无限，0 -> 不循环，N -> 循环 N 次
    let infinite_loop = request.loop_count == Some(-1);
    let loops: Option<u32> = if infinite_loop || request.loop_count.unwrap_or(0) <= 0 {
        None
    } else {
        Some(request.loop_count.unwrap() as u32)
    };

    // 解析 yoyo：-1 -> 无限乒乓，0 -> 不乒乓，N -> 乒乓 N 次
    let yoyo_enabled = request.yoyo == Some(-1) || request.yoyo.unwrap_or(0) > 0;
    let yoyo_loops: Option<u32> = if yoyo_enabled {
        if request.yoyo == Some(-1) {
            None
        } else if request.yoyo.unwrap_or(0) > 0 {
            Some(request.yoyo.unwrap() as u32)
        } else {
            None
        }
    } else {
        None
    };

    let effective_loops = yoyo_loops.or(loops);
    let infinite = infinite_loop || request.yoyo == Some(-1);

    let tween = Tween {
        param: request.param.to_string(),
        from: from_value,
        to: to_value,
        easing: request.ease.map(Easing::parse).unwrap_or_default(),
        start_ms,
        duration_ms: request.time.unwrap_or(0),
        infinite_loop: infinite,
        loop_count: effective_loops,
        yoyo: yoyo_enabled,
        yoyo_reverse: false,
        loop_delay_ms: request.loop_delay.unwrap_or(0),
        delete_on_finish: request.delete,
        handler: if request.sync
            || request.handler_file.is_some()
            || request.handler_label.is_some()
            || request.handler_handler.is_some()
            || request.delete
        {
            Some(TweenHandler {
                file: request.handler_file.map(str::to_string),
                label: request.handler_label.map(str::to_string),
                call: false,
                handler: request.handler_handler.map(str::to_string),
            })
        } else {
            None
        },
    };

    if let Some(layer) = scene.get_mut(request.id) {
        layer.tweens.retain(|t| t.param != request.param);
        layer.tweens.push(tween);
    }
}

/// 强制完成某图层的所有缓动：把终值写回属性，清空缓动列表。
pub(crate) fn finish_tweens(scene: &mut Scene, id: &str) {
    if let Some(layer) = scene.get_mut(id) {
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

/// 回收已结束的缓动，把终值固化到属性里，并收集完成回调。
pub(crate) fn gc_finished_tweens(
    scene: &mut Scene,
    now: u64,
    pending_tween_events: &mut Vec<TweenHandler>,
) {
    let mut settle: Vec<(String, String, f32)> = Vec::new();
    let mut completed: Vec<(String, Option<TweenHandler>, bool)> = Vec::new();
    let ids: Vec<String> = scene.iter_ids();
    for id in &ids {
        if let Some(layer) = scene.get(id) {
            for t in &layer.tweens {
                if t.is_finished(now) {
                    settle.push((id.clone(), t.param.clone(), t.to));
                    completed.push((id.clone(), t.handler.clone(), t.delete_on_finish));
                }
            }
        }
    }
    for (id, param, value) in settle {
        if let Some(layer) = scene.get_mut(&id) {
            layer.props.set_raw(&param, &format_param(&param, value));
            layer.tweens.retain(|t| !t.is_finished(now));
        }
    }
    for (id, handler, delete) in completed {
        if let Some(handler) = handler {
            pending_tween_events.push(handler);
        }
        if delete {
            scene.delete(&id);
        }
    }
}

/// 把 `[anime]` init/add/end 事件归约到帧动画状态。
pub(crate) fn apply_anime_event(
    scene: &mut Scene,
    states: &mut HashMap<String, AnimeState>,
    clock_ms: u64,
    request: AnimeRequest<'_>,
) {
    match request.mode {
        "init" => {
            let file = request.file.unwrap_or_default().to_string();
            scene.ensure(request.id);
            if let Some(layer) = scene.get_mut(request.id) {
                layer.file = Some(file.clone());
                layer.props.merge_raw(request.props);
            }
            let frame = AnimeFrame {
                time_ms: request.time.unwrap_or(0),
                file,
                mask: request.mask.map(str::to_string),
                props: request.props.clone(),
            };
            states.insert(
                request.id.to_string(),
                AnimeState {
                    frames: vec![frame],
                    loop_count: request.loop_count.unwrap_or(-1),
                    start_ms: 0,
                    total_duration_ms: 0,
                },
            );
        }
        "add" => {
            if let Some(state) = states.get_mut(request.id) {
                state.frames.push(AnimeFrame {
                    time_ms: request.time.unwrap_or(0),
                    file: request.file.unwrap_or_default().to_string(),
                    mask: request.mask.map(str::to_string),
                    props: request.props.clone(),
                });
            }
        }
        "end" => {
            if let Some(state) = states.get_mut(request.id) {
                state.frames.sort_by_key(|f| f.time_ms);
                state.total_duration_ms = request.time.unwrap_or(0);
                state.start_ms = clock_ms;
                apply_first_anime_frame(scene, request.id, state);
            }
        }
        _ => {}
    }
}

/// 推进帧动画：根据时钟前进到对应的帧，更新图层的文件和属性。
pub(crate) fn update_anime_frames(
    scene: &mut Scene,
    states: &mut HashMap<String, AnimeState>,
    now: u64,
) {
    for (layer_id, state) in states {
        if state.frames.is_empty() || state.total_duration_ms == 0 {
            continue;
        }
        let elapsed = now.saturating_sub(state.start_ms);

        let t = if state.loop_count == -1 {
            elapsed % state.total_duration_ms
        } else if state.loop_count == 0 {
            if elapsed >= state.total_duration_ms {
                state.total_duration_ms - 1
            } else {
                elapsed
            }
        } else {
            let max_elapsed = state.total_duration_ms * (state.loop_count as u64 + 1);
            if elapsed >= max_elapsed {
                state.total_duration_ms - 1
            } else {
                elapsed % state.total_duration_ms
            }
        };

        let frame = state
            .frames
            .iter()
            .rev()
            .find(|f| f.time_ms <= t)
            .or_else(|| state.frames.first());

        if let Some(frame) = frame {
            if let Some(layer) = scene.get_mut(layer_id) {
                layer.file = Some(frame.file.clone());
                layer.props.merge_raw(&frame.props);
            }
        }
    }
}

fn apply_first_anime_frame(scene: &mut Scene, id: &str, state: &AnimeState) {
    if let Some(first) = state.frames.first() {
        if let Some(layer) = scene.get_mut(id) {
            layer.file = Some(first.file.clone());
            layer.props.merge_raw(&first.props);
        }
    }
}

/// 读取图层某属性的当前数值，作为缓动的默认起点。未知属性回退 0。
fn current_param_value(scene: &Scene, id: &str, param: &str) -> f32 {
    let Some(layer) = scene.get(id) else {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tween(from: f32, to: f32, dur: u64) -> Tween {
        Tween {
            param: "alpha".into(),
            from,
            to,
            easing: Easing::Linear,
            start_ms: 1000,
            duration_ms: dur,
            infinite_loop: false,
            loop_count: None,
            yoyo: false,
            yoyo_reverse: false,
            loop_delay_ms: 0,
            delete_on_finish: false,
            handler: None,
        }
    }

    #[test]
    fn linear_interpolates_endpoints_and_midpoint() {
        let t = tween(0.0, 100.0, 1000);
        assert_eq!(t.value_at(1000), 0.0);
        assert_eq!(t.value_at(1500), 50.0);
        assert_eq!(t.value_at(2000), 100.0);
    }

    #[test]
    fn clamps_outside_range() {
        let t = tween(0.0, 100.0, 1000);
        assert_eq!(t.value_at(500), 0.0);
        assert_eq!(t.value_at(9999), 100.0);
    }

    #[test]
    fn zero_duration_snaps_to_target() {
        let t = tween(0.0, 100.0, 0);
        assert_eq!(t.value_at(1000), 100.0);
    }

    #[test]
    fn easing_in_is_slower_at_start() {
        assert!(Easing::EaseInQuad.apply(0.5) < 0.5);
        assert!(Easing::EaseOutQuad.apply(0.5) > 0.5);
        assert_eq!(Easing::EaseInOutQuad.apply(0.5), 0.5);
    }

    #[test]
    fn finished_detection() {
        let t = tween(0.0, 1.0, 1000);
        assert!(!t.is_finished(1500));
        assert!(t.is_finished(2000));
    }

    #[test]
    fn delay_defers_start() {
        let mut t = tween(0.0, 100.0, 1000);
        t.start_ms = 1500; // 500ms delay from clock=1000
        assert_eq!(t.value_at(1200), 0.0); // still before start
        assert_eq!(t.value_at(2000), 50.0); // midpoint between 1500-2500
        assert_eq!(t.value_at(2500), 100.0);
    }

    #[test]
    fn infinite_loop_never_ends() {
        let mut t = tween(0.0, 100.0, 1000);
        t.infinite_loop = true;
        assert!(!t.is_finished(99999));
        // value should be in range (cycling)
        let v = t.value_at(2500); // 1500ms into second cycle
        assert!(v >= 0.0 && v <= 100.0);
    }

    #[test]
    fn finite_loop_completes_after_n_cycles() {
        let mut t = tween(0.0, 100.0, 1000);
        t.loop_count = Some(2);
        // Cycle 1: 1000-2000, Cycle 2: 2000-3000
        assert!(!t.is_finished(2500));
        assert!(t.is_finished(3000));
    }

    #[test]
    fn yoyo_alternates_direction() {
        let mut t = tween(0.0, 100.0, 1000);
        t.loop_count = Some(2);
        t.yoyo = true;

        // Cycle 1 (forward): 1000→2000, from 0 to 100
        assert_eq!(t.value_at(1500), 50.0);

        // Cycle 2 (reverse): 2000→3000, from 100 to 0
        // yoyo_reverse is computed dynamically by build_frame, so test the logic directly
        assert!(t.is_yoyo_reverse(2500));
    }

    #[test]
    fn loop_delay_adds_gap_between_cycles() {
        let mut t = tween(0.0, 100.0, 1000);
        t.loop_count = Some(2);
        t.loop_delay_ms = 500;
        // Cycle 1: 1000-2000, delay: 2000-2500, Cycle 2: 2500-3500
        assert!(!t.is_finished(3400));
        assert!(t.is_finished(3500));
    }

    // ── easing function accuracy tests ──

    #[test]
    fn easing_linear_is_identity() {
        assert_eq!(Easing::Linear.apply(0.0), 0.0);
        assert_eq!(Easing::Linear.apply(0.5), 0.5);
        assert_eq!(Easing::Linear.apply(1.0), 1.0);
    }

    #[test]
    fn easing_endpoints_return_bounds() {
        let all = [
            Easing::EaseInQuad,
            Easing::EaseOutQuad,
            Easing::EaseInOutQuad,
            Easing::EaseInCubic,
            Easing::EaseOutCubic,
            Easing::EaseInOutCubic,
            Easing::EaseInQuart,
            Easing::EaseOutQuart,
            Easing::EaseInOutQuart,
            Easing::EaseInQuint,
            Easing::EaseOutQuint,
            Easing::EaseInOutQuint,
            Easing::EaseInExpo,
            Easing::EaseOutExpo,
            Easing::EaseInOutExpo,
            Easing::EaseInCirc,
            Easing::EaseOutCirc,
            Easing::EaseInOutCirc,
            Easing::EaseInSine,
            Easing::EaseOutSine,
            Easing::EaseInOutSine,
            Easing::EaseInBack,
            Easing::EaseOutBack,
            Easing::EaseInOutBack,
            Easing::EaseInElastic,
            Easing::EaseOutElastic,
            Easing::EaseInOutElastic,
            Easing::EaseInBounce,
            Easing::EaseOutBounce,
            Easing::EaseInOutBounce,
        ];
        for e in all {
            let eps = if matches!(
                e,
                Easing::EaseInElastic | Easing::EaseOutElastic | Easing::EaseInOutElastic
            ) {
                0.01 // elastic oscillates near 0 and 1
            } else {
                1e-4
            };
            assert!((e.apply(0.0) - 0.0).abs() < eps, "{e:?} at t=0");
            assert!((e.apply(1.0) - 1.0).abs() < eps, "{e:?} at t=1");
        }
    }

    #[test]
    fn easing_in_vs_out_are_symmetric() {
        let pairs = [
            (Easing::EaseInQuad, Easing::EaseOutQuad),
            (Easing::EaseInCubic, Easing::EaseOutCubic),
            (Easing::EaseInBack, Easing::EaseOutBack),
            (Easing::EaseInBounce, Easing::EaseOutBounce),
        ];
        for (e_in, e_out) in pairs {
            for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
                let eps = if matches!(e_in, Easing::EaseInBack | Easing::EaseOutBack) {
                    0.01
                } else {
                    1e-4
                };
                assert!(
                    (e_in.apply(t) + e_out.apply(1.0 - t) - 1.0).abs() < eps,
                    "{e_in:?} + {e_out:?}(1-{t}) != 1"
                );
            }
        }
    }

    #[test]
    fn parse_recognises_all_aliases() {
        assert_eq!(Easing::parse("easein"), Easing::EaseInQuad);
        assert_eq!(Easing::parse("easeout"), Easing::EaseOutQuad);
        assert_eq!(Easing::parse("easeinout"), Easing::EaseInOutQuad);
        assert_eq!(Easing::parse("easein_cubic"), Easing::EaseInCubic);
        assert_eq!(Easing::parse("easeout_elastic"), Easing::EaseOutElastic);
        assert_eq!(Easing::parse("easeinout_bounce"), Easing::EaseInOutBounce);
        assert_eq!(Easing::parse("easeoutbounce"), Easing::EaseOutBounce);
        assert_eq!(Easing::parse("garbage"), Easing::Linear);
    }
}
