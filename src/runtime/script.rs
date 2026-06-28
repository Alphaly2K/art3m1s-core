use super::CoreRuntime;
use crate::render_pipeline::RenderPipeline;
use asb_interpreter::event::WaitReason;
use asb_interpreter::tags::call_lua_function;
use asb_interpreter::{Event, ExecutionResult};
use std::collections::HashMap;
use std::sync::atomic::Ordering;

impl CoreRuntime {
    pub(super) fn advance_script(&mut self, clicked: bool, delta_ms: u64) {
        // onEnterFrame
        if let Err(e) = self.interpreter.fire_enter_frame() {
            crate::core_error!("onEnterFrame 错误: {e:?}");
        }

        let has_tags = self.has_queued_tags();
        if has_tags {
            if let Some(reason @ WaitReason::Stop { .. }) = self.wait_reason.clone() {
                self.drain_queued_tags_while_stopped(reason);
            } else if let Some(reason) = self.wait_reason.clone() {
                self.drain_queued_tags_while_waiting(reason);
            } else {
                self.wait_reason = None;
            }
        }

        if self.wait_reason.is_none() {
            self.run_until_wait_or_complete();
        } else {
            self.advance_wait_state(clicked, delta_ms);
        }
    }

    fn has_queued_tags(&self) -> bool {
        let ctx = self.interpreter.engine_context();
        !ctx.lock().unwrap().tag_queue.is_empty()
    }

    fn run_until_wait_or_complete(&mut self) {
        loop {
            match self.interpreter.run() {
                Ok(ExecutionResult::Wait(Event::Wait { reason })) => {
                    match &reason {
                        WaitReason::Timed { milliseconds } => {
                            self.timed_remaining_ms = *milliseconds;
                        }
                        WaitReason::Stop { .. } => {}
                        _ => {}
                    }
                    self.wait_reason = Some(reason);
                    self.reset_control_wait_flags();
                    break;
                }
                Ok(ExecutionResult::Wait(Event::VideoPlay { .. })) => {
                    self.wait_reason = Some(WaitReason::Stop {
                        reason: Some("video".into()),
                    });
                    self.reset_control_wait_flags();
                    break;
                }
                Ok(ExecutionResult::Wait(Event::Trans { .. })) => {
                    self.wait_reason = Some(WaitReason::Stop {
                        reason: Some("trans".into()),
                    });
                    self.reset_control_wait_flags();
                    break;
                }
                Ok(ExecutionResult::Wait(_)) => {
                    self.wait_reason = Some(WaitReason::Generic);
                    self.reset_control_wait_flags();
                    break;
                }
                Ok(ExecutionResult::Completed) | Ok(_) => break,
                Err(e) => {
                    crate::core_error!("解释器错误: {e:?}");
                    break;
                }
            }
        }
    }

    fn advance_wait_state(&mut self, clicked: bool, delta_ms: u64) {
        let Some(reason) = self.wait_reason.clone() else {
            return;
        };
        if let WaitReason::Stop {
            reason: Some(stop_reason),
        } = &reason
        {
            if stop_reason == "exskip" {
                self.advance_exskip_stop(reason);
                return;
            }
        }

        let video_resume = matches!(&reason, WaitReason::Stop { .. })
            && self
                .video_finished
                .swap(false, std::sync::atomic::Ordering::SeqCst);
        let trans_resume = matches!(
            &reason,
            WaitReason::Stop {
                reason: Some(r)
            } if r == "trans"
        ) && !RenderPipeline::new(&self.compositor).is_transition_in_progress();
        if video_resume || trans_resume {
            self.wait_reason = None;
            return;
        }

        let advance = match reason {
            WaitReason::Timed { .. } => {
                if self.skip_active() {
                    if self.should_hold_for_skip_reveal() {
                        false
                    } else {
                        self.timed_remaining_ms = 0;
                        true
                    }
                } else if delta_ms >= self.timed_remaining_ms {
                    self.timed_remaining_ms = 0;
                    true
                } else {
                    self.timed_remaining_ms -= delta_ms;
                    false
                }
            }
            WaitReason::Stop { .. } => false,
            _ => {
                if clicked {
                    if !self.is_text_reveal_complete() {
                        self.reveal_text_now();
                        false
                    } else {
                        true
                    }
                } else if self.skip_active() {
                    !self.should_hold_for_skip_reveal()
                } else {
                    self.should_auto_advance(delta_ms)
                }
            }
        };
        if advance {
            self.advance_wait_line();
        }
    }

    fn advance_wait_line(&mut self) {
        self.wait_reason = None;
        self.reset_control_wait_flags();
        self.interpreter.advance_line();
    }

    fn advance_exskip_stop(&mut self, stop_reason: WaitReason) {
        if !self.debug_skip_active.swap(false, Ordering::SeqCst) {
            crate::core_debug!("[runtime] Stop:exskip without active debugSkip; skipping stop");
            self.advance_wait_line();
            return;
        }

        crate::core_debug!("[runtime] Stop:exskip; firing onDebugSkipOut");
        if let Err(e) = self.fire_named_event_handler("onDebugSkipOut") {
            crate::core_error!("onDebugSkipOut 错误: {e:?}");
            self.wait_reason = Some(stop_reason);
            return;
        }

        if self.has_queued_tags() {
            self.drain_queued_tags_while_stopped(stop_reason);
        } else {
            self.advance_wait_line();
        }
    }

    fn fire_named_event_handler(&mut self, event_name: &str) -> asb_interpreter::Result<()> {
        let handler = {
            let ctx = self.interpreter.engine_context();
            ctx.lock().unwrap().event_handlers.get(event_name).cloned()
        };
        if let Some(func) = handler {
            call_lua_function(self.interpreter.lua(), &func, &HashMap::new())?;
        }
        Ok(())
    }

    fn drain_queued_tags_while_stopped(&mut self, stop_reason: WaitReason) {
        let mut should_resume = false;
        for _ in 0..64 {
            let drain = match self.interpreter.drain_queued_tags_only() {
                Ok(drain) => drain,
                Err(e) => {
                    crate::core_error!("解释器错误: {e:?}");
                    self.wait_reason = Some(stop_reason);
                    return;
                }
            };
            should_resume |= drain.saw_return || drain.changed_position;
            if drain.wait.is_some() {
                self.interpreter.advance_line();
                continue;
            }
            break;
        }

        if should_resume {
            self.wait_reason = None;
        } else {
            self.wait_reason = Some(stop_reason);
        }
    }

    fn drain_queued_tags_while_waiting(&mut self, wait_reason: WaitReason) {
        let drain = match self.interpreter.drain_queued_tags_only() {
            Ok(drain) => drain,
            Err(e) => {
                crate::core_error!("解释器错误: {e:?}");
                self.wait_reason = Some(wait_reason);
                return;
            }
        };

        if drain.saw_return || drain.changed_position {
            self.wait_reason = None;
        } else if let Some(Event::Wait { reason }) = drain.wait {
            self.wait_reason = Some(reason);
        } else {
            self.wait_reason = Some(wait_reason);
        }
    }
}
