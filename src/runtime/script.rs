use super::CoreRuntime;
use crate::render_pipeline::RenderPipeline;
use asb_interpreter::event::WaitReason;
use asb_interpreter::tags::call_lua_function;
use asb_interpreter::{Event, ExecutionResult};
use std::collections::HashMap;
use std::sync::atomic::Ordering;

impl CoreRuntime {
    pub(super) fn advance_script(&mut self, clicked: bool, delta_ms: u64) {
        self.refresh_inline_event_frame();
        // Native dialogs are modal from the scenario script's point of view. The dialog event can
        // be emitted from an estag queue that already contains its continuation; draining that
        // queue before the host responds runs stop/wait tags behind the dialog and corrupts the
        // scenario wait state.
        if self.pending_dialog.is_some() {
            return;
        }

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
            } else if self.active_inline_event_frame.is_some() {
                self.drain_inline_event_tags_without_wait();
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
                        WaitReason::Timed { milliseconds, .. } => {
                            self.timed_remaining_ms = *milliseconds;
                        }
                        WaitReason::Stop { .. } => {}
                        _ => {}
                    }
                    self.wait_reason = Some(reason);
                    self.reset_control_wait_flags();
                    break;
                }
                Ok(ExecutionResult::Wait(Event::VideoPlay { id: None, .. })) => {
                    self.wait_reason = Some(WaitReason::Stop {
                        reason: Some("video".into()),
                    });
                    self.reset_control_wait_flags();
                    break;
                }
                Ok(ExecutionResult::Wait(Event::VideoPlay { id: Some(_), .. })) => {
                    continue;
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
                Ok(ExecutionResult::Completed) => {
                    crate::core_debug!(
                        "[runtime] script completed at {:?}:{} stack_depth={}",
                        self.interpreter.current_script(),
                        self.interpreter.current_line(),
                        self.interpreter.call_stack().len()
                    );
                    break;
                }
                Ok(other) => {
                    crate::core_debug!("[runtime] 未处理的 ExecutionResult，按停帧处理: {other:?}");
                    break;
                }
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
        let scripted_decide = self.script_decide_edge();
        let clicked = clicked || scripted_decide;
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
            WaitReason::Timed { input, .. } => {
                if timed_wait_accepts_click(input, clicked) {
                    self.timed_remaining_ms = 0;
                    true
                } else if self.skip_active() {
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
            // A physical click must not skip [stop]. Artemis scripts can,
            // however, explicitly wake it by injecting a key edge with
            // e:overrideKey(..., status=32), as UI return paths commonly do.
            WaitReason::Stop { .. } => stop_wait_accepts_scripted_decide(scripted_decide),
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

    pub(super) fn advance_wait_line(&mut self) {
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
        /// 单帧排水上限：防止排队标签互相续接造成死循环。正常脚本远达不到。
        const MAX_DRAIN_ROUNDS: usize = 64;
        let mut should_resume = false;
        let mut rounds = 0;
        for _ in 0..MAX_DRAIN_ROUNDS {
            rounds += 1;
            let drain = match self.interpreter.drain_queued_tags_only() {
                Ok(drain) => drain,
                Err(e) => {
                    crate::core_error!("解释器错误: {e:?}");
                    self.finish_inline_event_frame(false);
                    self.wait_reason = Some(stop_reason);
                    return;
                }
            };
            self.finish_inline_event_frame(drain.changed_position);
            should_resume |= drain.saw_return || drain.changed_position;
            if drain.wait.is_some() {
                self.interpreter.advance_line();
                continue;
            }
            break;
        }
        if rounds >= MAX_DRAIN_ROUNDS {
            crate::core_warn!("[runtime] 排队标签单帧排水达到上限 {MAX_DRAIN_ROUNDS}，剩余延后到下一帧");
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
                self.finish_inline_event_frame(false);
                self.wait_reason = Some(wait_reason);
                return;
            }
        };
        self.finish_inline_event_frame(drain.changed_position);

        if drain.saw_return || drain.changed_position {
            self.wait_reason = None;
        } else if let Some(Event::Wait { reason }) = drain.wait {
            self.wait_reason = Some(reason);
        } else {
            self.wait_reason = Some(wait_reason);
        }
    }

    fn drain_inline_event_tags_without_wait(&mut self) {
        let drain = match self.interpreter.drain_queued_tags_only() {
            Ok(drain) => drain,
            Err(error) => {
                crate::core_error!("解释器错误: {error:?}");
                self.finish_inline_event_frame(false);
                return;
            }
        };
        self.finish_inline_event_frame(drain.changed_position);
        if let Some(event) = drain.wait {
            self.wait_reason = Some(match event {
                Event::Wait { reason } => reason,
                _ => WaitReason::Generic,
            });
        }
    }

    fn finish_inline_event_frame(&mut self, changed_position: bool) {
        if changed_position {
            self.refresh_inline_event_frame();
            return;
        }
        let Some(frame) = self.active_inline_event_frame.take() else {
            return;
        };
        if let Err(error) =
            self.interpreter
                .restore_position(&frame.script, frame.line, frame.stack)
        {
            crate::core_error!("移除未使用的事件返回帧失败: {error:?}");
        }
    }
}

fn timed_wait_accepts_click(input: i32, clicked: bool) -> bool {
    input == 1 && clicked
}

fn stop_wait_accepts_scripted_decide(scripted_decide: bool) -> bool {
    scripted_decide
}

#[cfg(test)]
mod tests {
    use super::{stop_wait_accepts_scripted_decide, timed_wait_accepts_click};

    #[test]
    fn timed_wait_only_accepts_click_for_input_one() {
        assert!(!timed_wait_accepts_click(0, true));
        assert!(timed_wait_accepts_click(1, true));
        assert!(!timed_wait_accepts_click(2, true));
        assert!(!timed_wait_accepts_click(1, false));
    }

    #[test]
    fn stop_wait_only_accepts_a_scripted_decide_edge() {
        assert!(stop_wait_accepts_scripted_decide(true));
        assert!(!stop_wait_accepts_scripted_decide(false));
    }
}
