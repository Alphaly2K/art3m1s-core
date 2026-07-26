//! Visual/interaction event boundary for the compositor.
//!
//! The runtime may receive many interpreter events: audio, video, save/load,
//! file IO, waits, and visual scene changes.  The compositor should only see
//! the subset that mutates visual or interaction state.  `CompositorEvent`
//! is that narrowed boundary.

use asb_interpreter::event::{Event, LayerEvent};
use std::collections::HashMap;

pub enum CompositorEvent<'a> {
    Layer(&'a LayerEvent),
    LayerRename {
        id: &'a str,
        to: &'a str,
    },
    LayerTween {
        id: &'a str,
        param: &'a str,
        from: Option<&'a str>,
        to: Option<&'a str>,
        ease: Option<&'a str>,
        time: Option<u64>,
        delay: Option<u64>,
        loop_count: Option<i32>,
        yoyo: Option<i32>,
        loop_delay: Option<u64>,
        sync: bool,
        delete: bool,
        handler_file: Option<&'a str>,
        handler_label: Option<&'a str>,
        handler_handler: Option<&'a str>,
    },
    LayerTweenDelete {
        id: &'a str,
    },
    LayerEventHandler {
        id: &'a str,
        event_type: &'a str,
        mode: &'a str,
        file: Option<&'a str>,
        label: Option<&'a str>,
        call: bool,
        handler: Option<&'a str>,
        penetration: bool,
        extra_params: &'a HashMap<String, String>,
    },
    SetInputHandler {
        event_name: &'a str,
        file: Option<&'a str>,
        label: Option<&'a str>,
        call: bool,
        handler: Option<&'a str>,
        extra_params: &'a HashMap<String, String>,
    },
    DelInputHandler {
        event_name: &'a str,
        key: Option<&'a str>,
    },
    Anime {
        id: &'a str,
        mode: &'a str,
        file: Option<&'a str>,
        mask: Option<&'a str>,
        time: Option<u64>,
        loop_count: Option<i32>,
        props: &'a HashMap<String, String>,
    },
    Trans {
        trans_type: i32,
        time: Option<u64>,
        rule: Option<&'a str>,
        vague: Option<i32>,
        input: i32,
    },
    Flip,
    /// `[lyedit]` 像素级图像加工。
    LayerEdit {
        id: &'a str,
        mode: &'a str,
        color: Option<&'a str>,
        file: Option<&'a str>,
        left: Option<i32>,
        top: Option<i32>,
    },
    /// `[tweenset]`：开始收集组内 lytween。
    TweenSetStart,
    /// `[/tweenset]`：结束收集并按顺序启动组内 lytween。
    TweenSetEnd,
}

impl<'a> CompositorEvent<'a> {
    pub fn from_interpreter(event: &'a Event) -> Option<Self> {
        match event {
            Event::Layer(layer_event) => Some(Self::Layer(layer_event)),
            Event::LayerRename { id, to } => Some(Self::LayerRename { id, to }),
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
            } => Some(Self::LayerTween {
                id,
                param,
                from: from.as_deref(),
                to: to.as_deref(),
                ease: ease.as_deref(),
                time: *time,
                delay: *delay,
                loop_count: *loop_count,
                yoyo: *yoyo,
                loop_delay: *loop_delay,
                sync: *sync,
                delete: *delete,
                handler_file: handler_file.as_deref(),
                handler_label: handler_label.as_deref(),
                handler_handler: handler_handler.as_deref(),
            }),
            Event::LayerTweenDelete { id } => Some(Self::LayerTweenDelete { id }),
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
            } => Some(Self::LayerEventHandler {
                id,
                event_type,
                mode,
                file: file.as_deref(),
                label: label.as_deref(),
                call: *call,
                handler: handler.as_deref(),
                penetration: *penetration,
                extra_params,
            }),
            Event::SetEventHandler {
                event_name,
                file,
                label,
                call,
                handler,
                extra_params,
            } => Some(Self::SetInputHandler {
                event_name,
                file: file.as_deref(),
                label: label.as_deref(),
                call: *call,
                handler: handler.as_deref(),
                extra_params,
            }),
            Event::DelEventHandler { event_name, key } => Some(Self::DelInputHandler {
                event_name,
                key: key.as_deref(),
            }),
            Event::Anime {
                id,
                mode,
                file,
                mask,
                time,
                loop_count,
                props,
            } => Some(Self::Anime {
                id,
                mode,
                file: file.as_deref(),
                mask: mask.as_deref(),
                time: *time,
                loop_count: *loop_count,
                props,
            }),
            Event::Trans {
                trans_type,
                time,
                rule,
                vague,
                input,
            } => Some(Self::Trans {
                trans_type: *trans_type,
                time: *time,
                rule: rule.as_deref(),
                vague: *vague,
                input: *input,
            }),
            Event::Flip => Some(Self::Flip),
            Event::LayerEdit {
                id,
                mode,
                color,
                file,
                left,
                top,
            } => Some(Self::LayerEdit {
                id,
                mode,
                color: color.as_deref(),
                file: file.as_deref(),
                left: *left,
                top: *top,
            }),
            Event::TweenSetStart => Some(Self::TweenSetStart),
            Event::TweenSetEnd => Some(Self::TweenSetEnd),
            _ => None,
        }
    }
}

pub trait IntoCompositorEvent<'a> {
    fn into_compositor_event(self) -> Option<CompositorEvent<'a>>;
}

impl<'a> IntoCompositorEvent<'a> for CompositorEvent<'a> {
    fn into_compositor_event(self) -> Option<CompositorEvent<'a>> {
        Some(self)
    }
}

impl<'a> IntoCompositorEvent<'a> for &'a Event {
    fn into_compositor_event(self) -> Option<CompositorEvent<'a>> {
        CompositorEvent::from_interpreter(self)
    }
}
