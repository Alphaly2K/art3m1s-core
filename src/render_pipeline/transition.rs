use crate::render_pipeline::draw::{
    BlendMode, ClipRect, ColorFilter, DrawCommand, DrawList, TextureId, TextureInfo,
    TextureProvider,
};
use std::cell::RefCell;

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
    #[allow(dead_code)]
    rule: Option<String>,
    #[allow(dead_code)]
    vague: Option<i32>,
    #[allow(dead_code)]
    input: i32,
}

pub(crate) fn start(
    slot: &RefCell<Option<TransitionState>>,
    clock_ms: u64,
    request: TransitionRequest<'_>,
) {
    if request.trans_type == 0 {
        clear(slot);
        return;
    }

    *slot.borrow_mut() = Some(TransitionState {
        trans_type: request.trans_type,
        start_ms: clock_ms,
        duration_ms: request.time.unwrap_or(1000),
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
    if let Some((texture, info)) = provider.upload_rgba(CAPTURE_TEXTURE_NAME, width, height, pixels)
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
        .map(|_| vec![CAPTURE_TEXTURE_NAME.to_string()])
        .unwrap_or_default()
}

pub(crate) fn overlay_old_frame(
    slot: &RefCell<Option<TransitionState>>,
    clock_ms: u64,
    frame: &mut DrawList,
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
    match transition.trans_type {
        1 => {
            frame.commands.push(DrawCommand {
                texture,
                size: info,
                transform: glam::Affine2::IDENTITY,
                opacity: 1.0 - progress,
                blend: BlendMode::Alpha,
                color: ColorFilter::default(),
                clip: ClipRect::full(info),
            });
        }
        2 => {
            // Rule transitions are not implemented yet; fall back to instant switch.
        }
        _ => {}
    }
}
