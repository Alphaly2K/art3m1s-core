use super::CoreRuntime;
use crate::render_pipeline::RenderPipeline;
use asb_interpreter::event::WaitReason;
use asb_interpreter::{Event, ExecutionResult};

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
            } else {
                self.wait_reason = None;
            }
        }

        if self.wait_reason.is_none() {
            self.run_until_wait_or_complete();
        } else {
            self.advance_wait_state(clicked, delta_ms, has_tags);
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
                    break;
                }
                Ok(ExecutionResult::Wait(Event::VideoPlay { .. })) => {
                    self.wait_reason = Some(WaitReason::Stop {
                        reason: Some("video".into()),
                    });
                    break;
                }
                Ok(ExecutionResult::Wait(Event::Trans { .. })) => {
                    self.wait_reason = Some(WaitReason::Stop {
                        reason: Some("trans".into()),
                    });
                    break;
                }
                Ok(ExecutionResult::Wait(_)) => {
                    self.wait_reason = Some(WaitReason::Generic);
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

    fn advance_wait_state(&mut self, clicked: bool, delta_ms: u64, has_tags: bool) {
        let Some(ref reason) = self.wait_reason else {
            return;
        };
        let video_resume = matches!(reason, WaitReason::Stop { .. })
            && self
                .video_finished
                .swap(false, std::sync::atomic::Ordering::SeqCst);
        let trans_resume = matches!(
            reason,
            WaitReason::Stop {
                reason: Some(r)
            } if r == "trans"
        ) && !RenderPipeline::new(&self.compositor)
            .is_transition_in_progress();
        if video_resume || trans_resume {
            self.wait_reason = None;
            return;
        }

        let advance = match reason {
            WaitReason::Timed { .. } => {
                if delta_ms >= self.timed_remaining_ms {
                    self.timed_remaining_ms = 0;
                    true
                } else {
                    self.timed_remaining_ms -= delta_ms;
                    false
                }
            }
            WaitReason::Stop { .. } => false,
            _ => !has_tags && clicked,
        };
        if advance {
            self.wait_reason = None;
            self.interpreter.advance_line();
        }
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
}
