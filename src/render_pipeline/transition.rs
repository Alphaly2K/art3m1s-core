use crate::render_pipeline::draw::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawList, ShaderEffect, TextureId, TextureInfo,
    TextureProvider,
};
use std::cell::RefCell;
use std::collections::BTreeMap;

const CAPTURE_TEXTURE_NAME: &str = "__trans_capture__";

/// Parameters reduced from a `[trans]` script event.
pub(crate) struct TransitionRequest<'a> {
    pub(crate) trans_type: i32,
    pub(crate) time: Option<u64>,
    pub(crate) rule: Option<&'a str>,
    pub(crate) vague: Option<i32>,
    pub(crate) input: i32,
}

/// Runtime transition state consumed by the render pipeline.
#[derive(Debug, Clone)]
pub(crate) struct TransitionState {
    trans_type: i32,
    start_ms: u64,
    duration_ms: u64,
    captured_texture: Option<TextureId>,
    captured_info: Option<TextureInfo>,
    needs_capture: bool,
    /// type=2 的规则灰度图路径（type 0/1 忽略）。
    rule: Option<String>,
    /// type=2 的边缘模糊度（文档建议 32 左右，按 0-255 尺度归一化进 shader）。
    vague: Option<i32>,
    /// 用户输入跳过策略：0=禁止，1（缺省）=允许，2=仅已处于跳过状态时允许。
    input: i32,
}

/// Whether a `[trans]` request has an animated interval that needs an old-frame
/// capture. `time=0` is an immediate scene commit, not a one-frame transition.
pub(crate) fn is_animated_request(trans_type: i32, time: Option<u64>) -> bool {
    trans_type != 0 && time.unwrap_or(1000) > 0
}

pub(crate) fn start(
    slot: &RefCell<Option<TransitionState>>,
    clock_ms: u64,
    request: TransitionRequest<'_>,
) {
    let duration_ms = request.time.unwrap_or(1000);
    if !is_animated_request(request.trans_type, request.time) {
        clear(slot);
        return;
    }

    *slot.borrow_mut() = Some(TransitionState {
        trans_type: request.trans_type,
        start_ms: clock_ms,
        duration_ms,
        captured_texture: None,
        captured_info: None,
        needs_capture: true,
        rule: request.rule.map(str::to_string),
        vague: request.vague,
        input: request.input,
    });
}

pub(crate) fn clear(slot: &RefCell<Option<TransitionState>>) {
    *slot.borrow_mut() = None;
}

/// 用户输入请求跳过转场。
///
/// 按 `[trans]` 的 `input` 参数决定是否放行：
/// - `0`：禁止输入跳过，返回 `false`；
/// - `2`：仅当引擎已处于跳过状态（`in_skip_mode`）时才跳过；
/// - 其余（缺省/`1`）：允许跳过。
///
/// 放行时立刻清除转场（等价于转场瞬间完成），返回 `true`。
/// 无进行中转场时返回 `false`。
pub(crate) fn skip_by_input(slot: &RefCell<Option<TransitionState>>, in_skip_mode: bool) -> bool {
    let allowed = {
        let state = slot.borrow();
        match state.as_ref() {
            None => return false,
            Some(transition) => match transition.input {
                0 => false,
                2 => in_skip_mode,
                _ => true,
            },
        }
    };
    if allowed {
        *slot.borrow_mut() = None;
    }
    allowed
}

pub(crate) fn clear_finished(slot: &RefCell<Option<TransitionState>>, clock_ms: u64) {
    let mut state = slot.borrow_mut();
    let Some(transition) = state.as_ref() else {
        return;
    };
    if !transition.needs_capture
        && clock_ms.saturating_sub(transition.start_ms) >= transition.duration_ms
    {
        *state = None;
    }
}

pub(crate) fn needs_capture(slot: &RefCell<Option<TransitionState>>) -> bool {
    slot.borrow()
        .as_ref()
        .map(|state| state.needs_capture)
        .unwrap_or(false)
}

pub(crate) fn is_in_progress(slot: &RefCell<Option<TransitionState>>, clock_ms: u64) -> bool {
    slot.borrow()
        .as_ref()
        .map(|state| {
            state.needs_capture || clock_ms.saturating_sub(state.start_ms) < state.duration_ms
        })
        .unwrap_or(false)
}

pub(crate) fn capture_texture(
    slot: &RefCell<Option<TransitionState>>,
    clock_ms: u64,
    pixels: &[u8],
    width: u32,
    height: u32,
    provider: &mut dyn TextureProvider,
) {
    let mut state = slot.borrow_mut();
    let Some(transition) = state.as_mut() else {
        return;
    };
    if !transition.needs_capture {
        return;
    }
    if let Some((texture, info)) =
        provider.upload_rgba_render_only(CAPTURE_TEXTURE_NAME, width, height, pixels)
    {
        transition.captured_texture = Some(texture);
        transition.captured_info = Some(info);
        transition.needs_capture = false;
        transition.start_ms = clock_ms;
    }
}

pub(crate) fn retained_files(slot: &RefCell<Option<TransitionState>>) -> Vec<String> {
    slot.borrow()
        .as_ref()
        .filter(|state| !state.needs_capture && state.captured_texture.is_some())
        .map(|state| {
            let mut files = vec![CAPTURE_TEXTURE_NAME.to_string()];
            // type=2 的 rule 纹理在转场期间同样要保活，防止被 retain 驱逐。
            if state.trans_type == 2
                && let Some(rule) = &state.rule
            {
                files.push(rule.clone());
            }
            files
        })
        .unwrap_or_default()
}

pub(crate) fn overlay_old_frame(
    slot: &RefCell<Option<TransitionState>>,
    clock_ms: u64,
    frame: &mut DrawList,
    provider: &mut dyn TextureProvider,
) {
    let state = slot.borrow();
    let Some(transition) = state.as_ref() else {
        return;
    };
    if transition.needs_capture {
        return;
    }
    let (Some(texture), Some(info)) = (transition.captured_texture, transition.captured_info)
    else {
        return;
    };

    let elapsed = clock_ms.saturating_sub(transition.start_ms);
    let progress = (elapsed as f32 / transition.duration_ms as f32).clamp(0.0, 1.0);

    // type=2 且 rule 可解析时用规则溶解 shader；rule 缺失/解析失败时
    // 退化为交叉淡化（比瞬切更贴近脚本意图）。
    let rule_effect = if transition.trans_type == 2 {
        transition
            .rule
            .as_deref()
            .filter(|rule| !rule.is_empty())
            .and_then(|rule| provider.resolve(rule))
            .map(|(rule_texture, _)| {
                // vague 按 Artemis 的 0-255 灰度尺度归一化；<=0 时取 1 保证软边有效。
                let vague = transition.vague.unwrap_or(32).max(1) as f32 / 255.0;
                let mut uniforms = BTreeMap::new();
                uniforms.insert("progress".to_string(), vec![progress]);
                uniforms.insert("vague".to_string(), vec![vague]);
                ShaderEffect {
                    name: crate::render_pipeline::shader::RULE_TRANS_SHADER.to_string(),
                    uniforms,
                    mask_texture: Some(rule_texture),
                    user_texture: None,
                }
            })
    } else {
        None
    };

    match (transition.trans_type, rule_effect) {
        // 规则图像转场：旧帧整屏叠加，逐像素 alpha 由 shader 按 rule 灰度决定。
        (2, Some(effect)) => {
            frame.push(DrawCommand {
                texture,
                size: info,
                transform: glam::Affine2::IDENTITY,
                // 自定义 shader 路径里 alpha uniform 来自 opacity；旧帧本体
                // 不额外淡出，溶解全部交给 rule 阈值。
                opacity: 1.0,
                blend: BlendMode::Alpha,
                color: ColorFilter::default(),
                clip: ClipRect::full(info),
                clip_bounds: None,
                shader: Some(effect),
                mesh: None,
                stencil: None,
            });
        }
        // 交叉淡化（type=1，以及 rule 不可用时 type=2 的回退）。
        (1, _) | (2, None) => {
            frame.push(DrawCommand {
                texture,
                size: info,
                transform: glam::Affine2::IDENTITY,
                opacity: 1.0 - progress,
                blend: BlendMode::Alpha,
                color: ColorFilter::default(),
                clip: ClipRect::full(info),
                clip_bounds: None,
                shader: None,
                mesh: None,
                stencil: None,
            });
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::mock::MockProvider;

    fn start_type2(slot: &RefCell<Option<TransitionState>>, input: i32) {
        start(
            slot,
            0,
            TransitionRequest {
                trans_type: 2,
                time: Some(1000),
                rule: Some("rule.png"),
                vague: Some(32),
                input,
            },
        );
    }

    fn capture(slot: &RefCell<Option<TransitionState>>, provider: &mut MockProvider) {
        capture_texture(slot, 0, &[0u8; 16], 2, 2, provider);
    }

    #[test]
    fn zero_duration_transition_is_an_immediate_commit() {
        let slot = RefCell::new(None);
        start_type2(&slot, 1);
        assert!(needs_capture(&slot));

        start(
            &slot,
            500,
            TransitionRequest {
                trans_type: 1,
                time: Some(0),
                rule: None,
                vague: None,
                input: 1,
            },
        );

        assert!(slot.borrow().is_none());
        assert!(!needs_capture(&slot));
        assert!(!is_in_progress(&slot, 500));
        assert!(!is_animated_request(1, Some(0)));
        assert!(is_animated_request(1, None));
    }

    #[test]
    fn rule_transition_overlays_shader_command_with_progress_and_vague() {
        let slot = RefCell::new(None);
        let mut provider = MockProvider::new();
        start_type2(&slot, 1);
        capture(&slot, &mut provider);

        let mut frame = DrawList::new();
        overlay_old_frame(&slot, 500, &mut frame, &mut provider);
        assert_eq!(frame.commands.len(), 1);
        let cmd = &frame.commands[0];
        // 旧帧本体不透明，溶解交给 shader。
        assert_eq!(cmd.opacity, 1.0);
        let effect = cmd.shader.as_ref().expect("rule 转场应携带 shader");
        assert_eq!(
            effect.name,
            crate::render_pipeline::shader::RULE_TRANS_SHADER
        );
        assert_eq!(effect.uniforms["progress"], vec![0.5]);
        assert!((effect.uniforms["vague"][0] - 32.0 / 255.0).abs() < 1e-6);
        // mask 纹理 = rule 灰度图。
        let rule_texture = effect.mask_texture.expect("应带 rule 纹理");
        assert_eq!(provider.name_of(rule_texture), "rule.png");
    }

    #[test]
    fn rule_transition_without_rule_falls_back_to_crossfade() {
        let slot = RefCell::new(None);
        let mut provider = MockProvider::new();
        start(
            &slot,
            0,
            TransitionRequest {
                trans_type: 2,
                time: Some(1000),
                rule: None,
                vague: None,
                input: 1,
            },
        );
        capture(&slot, &mut provider);

        let mut frame = DrawList::new();
        overlay_old_frame(&slot, 250, &mut frame, &mut provider);
        assert_eq!(frame.commands.len(), 1);
        let cmd = &frame.commands[0];
        assert!(cmd.shader.is_none());
        assert!((cmd.opacity - 0.75).abs() < 1e-6);
    }

    #[test]
    fn retained_files_include_rule_texture() {
        let slot = RefCell::new(None);
        let mut provider = MockProvider::new();
        start_type2(&slot, 1);
        capture(&slot, &mut provider);
        let files = retained_files(&slot);
        assert!(files.contains(&"__trans_capture__".to_string()));
        assert!(files.contains(&"rule.png".to_string()));
    }

    #[test]
    fn skip_by_input_respects_input_policy() {
        // input=1（缺省允许）：跳过并清除。
        let slot = RefCell::new(None);
        start_type2(&slot, 1);
        assert!(skip_by_input(&slot, false));
        assert!(slot.borrow().is_none());

        // input=0：禁止跳过。
        let slot = RefCell::new(None);
        start_type2(&slot, 0);
        assert!(!skip_by_input(&slot, false));
        assert!(slot.borrow().is_some());

        // input=2：仅已处于跳过状态时跳过。
        let slot = RefCell::new(None);
        start_type2(&slot, 2);
        assert!(!skip_by_input(&slot, false));
        assert!(slot.borrow().is_some());
        assert!(skip_by_input(&slot, true));
        assert!(slot.borrow().is_none());

        // 无转场：no-op。
        let slot: RefCell<Option<TransitionState>> = RefCell::new(None);
        assert!(!skip_by_input(&slot, true));
    }
}
