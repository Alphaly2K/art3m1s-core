//! 图层动画：属性缓动（tween）与画面转场（transition）。
//!
//! 解释器把 `[lytween]` 解析成 `Event::LayerTween`，把 `[trans]` / `[uitrans]`
//! 解析成转场事件。这些都是基于时间的：合成器记录起止值与时长，在每帧 `build`
//! 时按当前时间求出插值，写回图层属性。本模块只做"按时间求值"，不持有图层引用。

/// 缓动函数。Artemis 的 `ease` 字符串在归约阶段映射到这里。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Easing {
    #[default]
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
}

impl Easing {
    /// 把线性进度 `t`（0.0-1.0）映射为缓动后的进度。
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::EaseIn => t * t,
            Easing::EaseOut => t * (2.0 - t),
            Easing::EaseInOut => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    -1.0 + (4.0 - 2.0 * t) * t
                }
            }
        }
    }

    /// 解析 Artemis 的 ease 名称，未知值回退线性。
    pub fn parse(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "in" | "easein" => Easing::EaseIn,
            "out" | "easeout" => Easing::EaseOut,
            "inout" | "easeinout" => Easing::EaseInOut,
            _ => Easing::Linear,
        }
    }
}

/// 单个数值属性的缓动。
///
/// 时间用毫秒，与解释器事件一致。`param` 是被缓动的属性名（如 `"alpha"`、
/// `"left"`），由归约阶段从事件填入，build 阶段据此把求值结果写回 `LayerProps`。
#[derive(Debug, Clone, PartialEq)]
pub struct Tween {
    pub param: String,
    pub from: f32,
    pub to: f32,
    pub easing: Easing,
    /// 动画开始的时间戳（毫秒），相对合成器时钟。
    pub start_ms: u64,
    pub duration_ms: u64,
}

impl Tween {
    /// 求出给定时刻的当前值。`now` 早于 `start` 时取起始值，超过结束时取终值。
    pub fn value_at(&self, now_ms: u64) -> f32 {
        if self.duration_ms == 0 || now_ms >= self.start_ms + self.duration_ms {
            return self.to;
        }
        if now_ms <= self.start_ms {
            return self.from;
        }
        let elapsed = (now_ms - self.start_ms) as f32;
        let progress = self.easing.apply(elapsed / self.duration_ms as f32);
        self.from + (self.to - self.from) * progress
    }

    /// 动画是否已结束（可被回收）。
    pub fn is_finished(&self, now_ms: u64) -> bool {
        now_ms >= self.start_ms + self.duration_ms
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
        }
    }

    #[test]
    fn linear_interpolates_endpoints_and_midpoint() {
        let t = tween(0.0, 100.0, 1000);
        assert_eq!(t.value_at(1000), 0.0); // t=0
        assert_eq!(t.value_at(1500), 50.0); // 中点
        assert_eq!(t.value_at(2000), 100.0); // 末尾
    }

    #[test]
    fn clamps_outside_range() {
        let t = tween(0.0, 100.0, 1000);
        assert_eq!(t.value_at(500), 0.0); // 开始前
        assert_eq!(t.value_at(9999), 100.0); // 结束后
    }

    #[test]
    fn zero_duration_snaps_to_target() {
        let t = tween(0.0, 100.0, 0);
        assert_eq!(t.value_at(1000), 100.0);
    }

    #[test]
    fn easing_in_is_slower_at_start() {
        // EaseIn 在中点的进度应小于线性的 0.5。
        assert!(Easing::EaseIn.apply(0.5) < 0.5);
        assert!(Easing::EaseOut.apply(0.5) > 0.5);
        assert_eq!(Easing::EaseInOut.apply(0.5), 0.5);
    }

    #[test]
    fn finished_detection() {
        let t = tween(0.0, 1.0, 1000);
        assert!(!t.is_finished(1500));
        assert!(t.is_finished(2000));
    }
}
