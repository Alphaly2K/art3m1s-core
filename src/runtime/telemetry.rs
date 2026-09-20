//! 输入管线遥测，用于验证点击管线符合原引擎逻辑。
//!
//! 使用 `ASB_TRACE_INPUT=1` 启用。输出纯引擎状态，不读取游戏 Lua 变量。
//! 按 Sol 指南要求输出：raw_bits / override_bits / effective_bits / dispatch
//! outcomes / wait / pc / queue 等引擎字段。

use super::callbacks::{EffectiveInputFrame, InputSnapshot};
use super::input::DispatchOutcome;
use asb_interpreter::WaitReason;
use std::collections::{BTreeMap, HashSet};

/// 输入遥测是否启用（环境变量 ASB_TRACE_INPUT=1）
pub(super) fn input_telemetry_enabled() -> bool {
    std::env::var("ASB_TRACE_INPUT").is_ok()
}

/// 捕获本帧 raw 输入状态的快照（在 overrideKey 之前）
#[derive(Clone, Debug)]
pub(super) struct RawInputSnapshot {
    pub keys: BTreeMap<u32, u32>, // key → raw bits
}

/// 从 InputSnapshot 提取 raw bits（在 override 之前）
pub(super) fn capture_raw_input_state(
    input: &InputSnapshot,
    now: std::time::Instant,
) -> RawInputSnapshot {
    use super::callbacks::{
        OVERRIDE_IS_DECIDE, OVERRIDE_IS_DOWN, OVERRIDE_IS_DOWN_EDGE, OVERRIDE_IS_PUSH,
        OVERRIDE_IS_UP_EDGE,
    };

    let mut keys = BTreeMap::new();

    // 收集所有可能有输入的键
    let candidate_keys: HashSet<u32> = input
        .keys_down
        .iter()
        .chain(input.keys_down_edge.iter())
        .chain(input.keys_up_edge.iter())
        .chain(input.mouse_buttons_down.iter())
        .chain(input.mouse_buttons_down_edge.iter())
        .chain(input.mouse_buttons_up_edge.iter())
        .copied()
        .collect();

    for &vk in &candidate_keys {
        let mut bits = 0u32;

        // raw_down
        if input.keys_down.contains(&vk) || (vk == 1 && input.mouse_buttons_down.contains(&1)) {
            bits |= OVERRIDE_IS_DOWN;
        }

        // raw_down_edge
        if input.keys_down_edge.contains(&vk)
            || (vk == 1 && (input.clicked || input.mouse_buttons_down_edge.contains(&1)))
        {
            bits |= OVERRIDE_IS_DOWN_EDGE | OVERRIDE_IS_DECIDE;
        }

        // raw_up_edge
        if input.keys_up_edge.contains(&vk) || (vk == 1 && input.mouse_buttons_up_edge.contains(&1))
        {
            bits |= OVERRIDE_IS_UP_EDGE;
        }

        // raw_push (isPush 语义：down_edge 或 按住 ≥0.5s)
        let is_push = if input.keys_down_edge.contains(&vk)
            || (vk == 1 && (input.clicked || input.mouse_buttons_down_edge.contains(&1)))
        {
            true
        } else if bits & OVERRIDE_IS_DOWN != 0 {
            input.keys_pressed_at.get(&vk).is_some_and(|pressed_at| {
                now.duration_since(*pressed_at) >= std::time::Duration::from_millis(500)
            })
        } else {
            false
        };

        if is_push {
            bits |= OVERRIDE_IS_PUSH;
        }

        if bits != 0 {
            keys.insert(vk, bits);
        }
    }

    RawInputSnapshot { keys }
}

/// 格式化位集合为可读字符串（例如 "DOWN|DOWN_EDGE|DECIDE"）
fn format_bits(bits: u32) -> String {
    use super::callbacks::{
        OVERRIDE_IS_DECIDE, OVERRIDE_IS_DOWN, OVERRIDE_IS_DOWN_EDGE, OVERRIDE_IS_PUSH,
        OVERRIDE_IS_UP_EDGE,
    };

    if bits == 0 {
        return "0".to_string();
    }

    let mut parts = Vec::new();
    if bits & OVERRIDE_IS_PUSH != 0 {
        parts.push("PUSH");
    }
    if bits & OVERRIDE_IS_DOWN != 0 {
        parts.push("DOWN");
    }
    if bits & OVERRIDE_IS_DOWN_EDGE != 0 {
        parts.push("DOWN_EDGE");
    }
    if bits & OVERRIDE_IS_UP_EDGE != 0 {
        parts.push("UP_EDGE");
    }
    if bits & OVERRIDE_IS_DECIDE != 0 {
        parts.push("DECIDE");
    }

    if parts.is_empty() {
        format!("0x{bits:x}")
    } else {
        parts.join("|")
    }
}

/// 格式化键集合（例如 "1:DOWN|DECIDE, 17:DOWN"）
fn format_keys(keys: &BTreeMap<u32, u32>) -> String {
    if keys.is_empty() {
        return "-".to_string();
    }

    keys.iter()
        .map(|(key, bits)| format!("{key}:{}", format_bits(*bits)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// 输入管线遥测记录：每个有输入边沿的帧输出一行
#[derive(Clone, Debug)]
pub(super) struct InputTelemetry {
    pub raw: RawInputSnapshot,
    pub overrides: BTreeMap<u32, u32>, // 只包含实际被 override 的键
    pub override_all: Option<u32>,
    pub effective: EffectiveInputFrame,
    pub pointer_target: Option<String>,
    pub layer_outcome: Option<DispatchOutcome>,
    pub push_outcome: DispatchOutcome,
    pub default_role_allowed: bool,
    pub user_input: bool,
    pub advance: bool,
    pub wait_before: Option<WaitReason>,
    pub wait_after: Option<WaitReason>,
    pub script_before: Option<(String, usize)>, // (file, line)
    pub script_after: Option<(String, usize)>,
    pub queue_len_before: usize,
    pub queue_len_after: usize,
}

impl InputTelemetry {
    /// 输出遥测记录（单行格式，用于 grep/awk 处理）
    pub(super) fn emit(&self) {
        // 提取 override 的键（与 raw 不同的）
        let override_keys: BTreeMap<u32, u32> = self
            .overrides
            .iter()
            .filter(|(key, bits)| self.raw.keys.get(key).copied().unwrap_or(0) != **bits)
            .map(|(k, v)| (*k, *v))
            .collect();

        // 格式：[INPUT-TICK] raw={...} override={...} effective={...}
        // ptr={layer} push={outcome} role_allowed={bool}
        // user_input={bool} advance={bool}
        // wait={before→after} pc={file:line→file:line} queue={N→M}

        let raw_str = format_keys(&self.raw.keys);
        let mut override_parts = Vec::new();
        if let Some(bits) = self.override_all {
            override_parts.push(format!("all:{}", format_bits(bits)));
        }
        if !override_keys.is_empty() {
            override_parts.push(format_keys(&override_keys));
        }
        let override_str = if override_parts.is_empty() {
            "-".to_string()
        } else {
            override_parts.join(", ")
        };
        let effective_str = format_keys(&self.effective.keys);

        let ptr_str = self.pointer_target.as_deref().unwrap_or("-");

        let layer_outcome_str = self
            .layer_outcome
            .map(|o| format!("{o:?}"))
            .unwrap_or_else(|| "-".to_string());

        let push_outcome_str = format!("{:?}", self.push_outcome);

        let wait_before_str = self
            .wait_before
            .as_ref()
            .map(|w| format!("{w:?}"))
            .unwrap_or_else(|| "-".to_string());
        let wait_after_str = self
            .wait_after
            .as_ref()
            .map(|w| format!("{w:?}"))
            .unwrap_or_else(|| "-".to_string());

        let pc_before_str = self
            .script_before
            .as_ref()
            .map(|(f, l)| format!("{f}:{l}"))
            .unwrap_or_else(|| "-".to_string());
        let pc_after_str = self
            .script_after
            .as_ref()
            .map(|(f, l)| format!("{f}:{l}"))
            .unwrap_or_else(|| "-".to_string());

        eprintln!(
            "[INPUT-TICK] raw={{{}}} override={{{}}} effective={{{}}} ptr={} layer={} push={} role_allow={} user_input={} advance={} wait={{{}→{}}} pc={{{}→{}}} queue={{{}→{}}}",
            raw_str,
            override_str,
            effective_str,
            ptr_str,
            layer_outcome_str,
            push_outcome_str,
            self.default_role_allowed,
            self.user_input,
            self.advance,
            wait_before_str,
            wait_after_str,
            pc_before_str,
            pc_after_str,
            self.queue_len_before,
            self.queue_len_after,
        );
    }
}
