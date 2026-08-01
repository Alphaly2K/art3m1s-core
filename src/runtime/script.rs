use super::CoreRuntime;
use crate::render_pipeline::RenderPipeline;
use asb_interpreter::event::WaitReason;
use asb_interpreter::tags::call_lua_function;
use asb_interpreter::{Event, ExecutionResult};
use std::collections::HashMap;
use std::sync::atomic::Ordering;

impl CoreRuntime {
    pub(super) fn advance_script(&mut self, clicked: bool, delta_ms: u64) {
        // 安装 var system=file_*/get_sound_info 的宿主查询钩子（每 runtime 一次；
        // 放在这里保证 load_project 之后、脚本第一次执行之前完成）。
        self.ensure_host_query_hooks();
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

        // httpget/httppost 挂起：宿主回填结果前脚本不推进。放在 onEnterFrame
        // 之后——文档允许脚本在 Lua 每帧处理里把 s.http.cancel 置 1 中断请求。
        if self.http_request_pending() {
            self.poll_pending_http_cancel();
            if self.http_request_pending() {
                return;
            }
        }

        // e:setScriptStatus 强制停止（如状态 4「停止，不接受用户输入」）：脚本被强制
        // 暂停，剧情不推进；同样放在 onEnterFrame 之后，好让 Lua 每帧处理里
        // e:setScriptStatus(0) 自我恢复。
        if self.script_forced_stop {
            return;
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
            // [autosave allow=2]：每次进入用户输入等待时自动保存。
            if wait_reason_is_input_wait(self.wait_reason.as_ref()) {
                self.maybe_autosave_on_input_wait();
            }
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
        let physical_clicked = clicked;
        let scripted_decide = self.script_decide_edge();
        let advance_requested = physical_clicked || scripted_decide;
        // stopbyclick 只响应宿主真实点击。overrideKey 注入的 decide 边沿是脚本内部
        // 推进信号，若也视作点击，自动模式会被自己的 mainloop 立即关闭。
        if physical_clicked && self.automode_stops_on_click() {
            self.set_automode_mode(false);
        }
        if automode_stop_by_stop_wait(&reason) && self.automode_stops_on_stop() {
            self.set_automode_mode(false);
        }
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
        let is_trans_wait = matches!(
            &reason,
            WaitReason::Stop {
                reason: Some(r)
            } if r == "trans"
        );
        // 转场等待期间收到点击：按 [trans] 的 input 参数策略尝试提前结束转场
        // （input=0 禁止；input=2 仅在已处于跳过态时放行；缺省/1 允许）。
        // 跳过态（skip）同样视作输入意图，让 input=2 生效。
        if trans_input_skip_requested(
            is_trans_wait,
            advance_requested,
            self.skip_active(),
            RenderPipeline::new(&self.compositor).is_transition_in_progress(),
        ) {
            RenderPipeline::new(&self.compositor).skip_transition_by_input(self.skip_active());
        }
        let trans_resume =
            is_trans_wait && !RenderPipeline::new(&self.compositor).is_transition_in_progress();
        if video_resume || trans_resume {
            self.wait_reason = None;
            return;
        }

        let advance = match reason {
            WaitReason::Timed { input, .. } => {
                if timed_wait_accepts_click(input, advance_requested) {
                    self.timed_remaining_ms = 0;
                    true
                } else if self.skip_active() {
                    self.reveal_text_for_skip();
                    self.timed_remaining_ms = 0;
                    true
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
            // [wait se=ID (time=N)]：等待该 SE 播放结束；带 time 时等待
            // "从 SE 开播起 N 毫秒"。变体未携带 input 参数，按缺省 input=0
            // 处理点击（不解除）；跳过态直接放行以免锁死（近似 input=2）。
            WaitReason::Se { ref id, time } => {
                self.skip_active() || self.se_wait_finished(id, time)
            }
            // [wait video=层ID]：等待该视频层播放结束（宿主经
            // notify_video_finished(id) 或状态后端自然完成解除）。
            WaitReason::VideoLayer { ref id } => {
                self.skip_active() || !self.video.is_layer_playing(id)
            }
            // [wait scenario=1|2]：等待场景文本出现/隐藏的 Tween 完成。
            // 本实现里隐藏（mode=2）是瞬时的，等待立即解除；
            // 出现（mode=1）等逐字揭示完成。
            WaitReason::ScenarioTween { mode } => {
                self.skip_active() || mode != 1 || self.is_text_reveal_complete()
            }
            _ => {
                if advance_requested {
                    if !self.is_text_reveal_complete() {
                        self.reveal_text_now();
                        false
                    } else {
                        true
                    }
                } else if self.skip_active() {
                    self.reveal_text_for_skip();
                    true
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

    /// `[wait se=ID (time=N)]` 的解除条件。
    ///
    /// - SE 不存在或已停止 → 解除（含宿主经 notify_sound_finished 报告完成）；
    /// - 带 time → 从 SE 开播时刻起满 N 毫秒解除（文档：与 time 并用时从
    ///   SE 播放开始时间起算）；
    /// - 不带 time 且没有宿主音频后端 → 立即解除（状态后端不模拟真实时长，
    ///   避免开发环境死等）。
    fn se_wait_finished(&self, id: &str, time: Option<u64>) -> bool {
        let state = self.audio.audio_state();
        let channel = state
            .se_channels
            .get(id)
            .or_else(|| state.voice_channels.get(id));
        let Some(channel) = channel else {
            return true;
        };
        if !channel.playing {
            return true;
        }
        match time {
            Some(time) => state.clock_ms >= channel.started_at_ms.saturating_add(time),
            None => !crate::ffi::media_command_callback_registered(),
        }
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

    /// 检测点击等待的进入/退出边沿，触发 onClickWaitIn / onClickWaitOut 处理器
    /// （e:setEventHandler 注册）。每帧调用一次。
    pub(super) fn sync_click_wait_handlers(&mut self) {
        let now = matches!(
            self.wait_reason,
            Some(WaitReason::Generic) | Some(WaitReason::Generic0)
        );
        if now == self.was_click_wait {
            return;
        }
        self.was_click_wait = now;
        let handler = if now {
            "onClickWaitIn"
        } else {
            "onClickWaitOut"
        };
        if let Err(e) = self.fire_named_event_handler(handler) {
            crate::core_error!("{handler} 错误: {e:?}");
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
                    self.finish_inline_event_frame(false, false, false);
                    self.wait_reason = Some(stop_reason);
                    return;
                }
            };
            self.finish_inline_event_frame(drain.wait.is_some(), drain.saw_call, drain.saw_jump);
            should_resume |= drain.saw_return || drain.changed_position;
            if drain.wait.is_some() {
                self.interpreter.advance_line();
                continue;
            }
            break;
        }
        if rounds >= MAX_DRAIN_ROUNDS {
            crate::core_warn!(
                "[runtime] 排队标签单帧排水达到上限 {MAX_DRAIN_ROUNDS}，剩余延后到下一帧"
            );
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
                self.finish_inline_event_frame(false, false, false);
                self.wait_reason = Some(wait_reason);
                return;
            }
        };
        self.finish_inline_event_frame(drain.wait.is_some(), drain.saw_call, drain.saw_jump);

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
                self.finish_inline_event_frame(false, false, false);
                return;
            }
        };
        self.finish_inline_event_frame(drain.wait.is_some(), drain.saw_call, drain.saw_jump);
        if let Some(event) = drain.wait {
            self.wait_reason = Some(match event {
                Event::Wait { reason } => reason,
                _ => WaitReason::Generic,
            });
        }
    }

    fn finish_inline_event_frame(
        &mut self,
        paused: bool,
        saw_queued_call: bool,
        saw_queued_jump: bool,
    ) {
        let has_queued_tags = self.has_queued_tags();
        if let Err(error) = settle_inline_event_frame(
            &mut self.interpreter,
            &mut self.active_inline_event_frame,
            paused,
            has_queued_tags,
            saw_queued_call,
            saw_queued_jump,
        ) {
            crate::core_error!("移除未使用的事件返回帧失败: {error:?}");
        }
    }
}

fn settle_inline_event_frame(
    interpreter: &mut asb_interpreter::Interpreter,
    active_frame: &mut Option<super::InlineEventFrame>,
    paused: bool,
    has_queued_tags: bool,
    saw_queued_call: bool,
    saw_queued_jump: bool,
) -> asb_interpreter::Result<()> {
    let marker_active = active_frame.as_ref().is_some_and(|frame| {
        super::input::inline_event_marker_is_active(frame, &interpreter.call_stack())
    });
    if !marker_active {
        *active_frame = None;
        return Ok(());
    }

    // A queued [call] from the original event has established a real return
    // frame above the synthetic marker. Remove only the marker and leave that
    // call frame intact. Once a queued [jump] has claimed the marker, however,
    // calls made inside that helper are nested and must not remove it.
    let claimed_by_jump = active_frame
        .as_ref()
        .is_some_and(|frame| frame.claimed_by_jump);
    if saw_queued_call && !claimed_by_jump {
        let frame = active_frame.take().unwrap();
        let Some(script) = interpreter.current_script().map(str::to_string) else {
            return Ok(());
        };
        let line = interpreter.current_line();
        let mut stack = interpreter.call_stack();
        super::input::detach_inline_event_marker(&frame, &mut stack);
        return interpreter.restore_position(&script, line, stack);
    }

    // Lua UI helpers commonly enqueue a jump to system/script.asb
    // (fn.push/popfuncXX). Remember that ownership across waits and queue
    // drains: the helper may pause midway before its trailing [return].
    if saw_queued_jump {
        active_frame.as_mut().unwrap().claimed_by_jump = true;
    }
    if active_frame
        .as_ref()
        .is_some_and(|frame| frame.claimed_by_jump)
        || paused
        || has_queued_tags
    {
        return Ok(());
    }

    let frame = active_frame.take().unwrap();
    interpreter.restore_position(&frame.script, frame.line, frame.stack)
}

fn timed_wait_accepts_click(input: i32, clicked: bool) -> bool {
    input == 1 && clicked
}

fn stop_wait_accepts_scripted_decide(scripted_decide: bool) -> bool {
    scripted_decide
}

fn automode_stop_by_stop_wait(reason: &WaitReason) -> bool {
    match reason {
        WaitReason::Stop { reason } => !reason.as_deref().is_some_and(|reason| {
            reason == "video" || reason == "trans" || reason.starts_with("tween:")
        }),
        _ => false,
    }
}

/// 转场等待期间是否应尝试用输入提前结束转场。
///
/// 仅当处于转场等待（`is_trans_wait`）、有转场正在进行（`transition_in_progress`）、
/// 且存在输入意图（物理点击 `clicked` 或已处于跳过态 `skip_active`）时才尝试。
/// 是否真正跳过再由 [`RenderPipeline::skip_transition_by_input`] 按 `[trans]`
/// 的 input 参数（0/1/2）裁决。
fn trans_input_skip_requested(
    is_trans_wait: bool,
    clicked: bool,
    skip_active: bool,
    transition_in_progress: bool,
) -> bool {
    is_trans_wait && transition_in_progress && (clicked || skip_active)
}

/// 是否属于"用户输入等待"（[autosave allow=2] 的自动保存触发点）：
/// 点击等待（Generic/Generic0）与按键等待（exkey）算；
/// 定时/停止/媒体同步类等待不算。
fn wait_reason_is_input_wait(reason: Option<&WaitReason>) -> bool {
    matches!(
        reason,
        Some(WaitReason::Generic) | Some(WaitReason::Generic0) | Some(WaitReason::KeyWait { .. })
    )
}

#[cfg(test)]
mod tests {
    use super::{
        automode_stop_by_stop_wait, settle_inline_event_frame, stop_wait_accepts_scripted_decide,
        timed_wait_accepts_click, trans_input_skip_requested, wait_reason_is_input_wait,
    };
    use crate::runtime::InlineEventFrame;
    use asb_interpreter::event::WaitReason;
    use asb_interpreter::{CallFrame, CallbackResult, Event, ExecutionResult, InterpreterConfig};
    use std::collections::HashMap;

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

    #[test]
    fn automode_stopbystop_ignores_internal_media_and_transition_waits() {
        assert!(automode_stop_by_stop_wait(&WaitReason::Stop {
            reason: None
        }));
        assert!(automode_stop_by_stop_wait(&WaitReason::Stop {
            reason: Some("menu".into())
        }));
        for reason in ["video", "trans", "tween:mw"] {
            assert!(!automode_stop_by_stop_wait(&WaitReason::Stop {
                reason: Some(reason.into())
            }));
        }
        assert!(!automode_stop_by_stop_wait(&WaitReason::Generic));
    }

    #[test]
    fn queued_jump_returns_from_helper_to_waiting_story() {
        let mut interpreter = asb_interpreter::Interpreter::new(InterpreterConfig::default());
        interpreter.load_script("story", "*main\n[wait]\n").unwrap();
        interpreter
            .load_script("system/script.asb", "*popfunc01\n[return]\n")
            .unwrap();
        interpreter.set_callback(|event| match event {
            Event::Wait { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        });
        interpreter.start("story", "main").unwrap();
        assert!(matches!(
            interpreter.run().unwrap(),
            ExecutionResult::Wait(_)
        ));

        let line = interpreter.current_line();
        let mut stack = interpreter.call_stack();
        stack.push(CallFrame {
            script: "story".into(),
            return_line: line,
        });
        interpreter.restore_position("story", line, stack).unwrap();
        let mut frame = Some(InlineEventFrame {
            script: "story".into(),
            line,
            stack: Vec::new(),
            claimed_by_jump: false,
        });
        interpreter
            .engine_context()
            .lock()
            .unwrap()
            .tag_queue
            .push((
                "jump".into(),
                HashMap::from([
                    ("file".into(), "system/script.asb".into()),
                    ("label".into(), "popfunc01".into()),
                ]),
            ));

        let drain = interpreter.drain_queued_tags_only().unwrap();
        assert!(drain.changed_position);
        assert!(drain.saw_jump);
        assert!(!drain.saw_call);
        settle_inline_event_frame(
            &mut interpreter,
            &mut frame,
            drain.wait.is_some(),
            false,
            drain.saw_call,
            drain.saw_jump,
        )
        .unwrap();
        assert!(frame.is_some(), "helper jump must retain the return marker");
        assert!(
            frame.as_ref().unwrap().claimed_by_jump,
            "helper jump must claim the marker across later queue drains"
        );

        settle_inline_event_frame(&mut interpreter, &mut frame, false, false, false, false)
            .unwrap();
        assert!(
            frame.is_some(),
            "a paused helper must retain its marker after the queue empties"
        );

        assert!(matches!(
            interpreter.run().unwrap(),
            ExecutionResult::Wait(_)
        ));
        assert_eq!(interpreter.current_script(), Some("story"));
        assert_eq!(interpreter.current_line(), line);
        assert!(interpreter.call_stack().is_empty());
    }

    #[test]
    fn queued_call_replaces_the_synthetic_event_return_frame() {
        let mut interpreter = asb_interpreter::Interpreter::new(InterpreterConfig::default());
        interpreter.load_script("title", "*main\n[stop]\n").unwrap();
        interpreter
            .load_script("system/script.asb", "*estag01\n[return]\n")
            .unwrap();
        interpreter.set_callback(|event| match event {
            Event::Wait { .. } => CallbackResult::Pause,
            _ => CallbackResult::Continue,
        });
        interpreter.start("title", "main").unwrap();
        assert!(matches!(
            interpreter.run().unwrap(),
            ExecutionResult::Wait(_)
        ));

        let line = interpreter.current_line();
        let mut stack = interpreter.call_stack();
        stack.push(CallFrame {
            script: "title".into(),
            return_line: line,
        });
        interpreter.restore_position("title", line, stack).unwrap();
        let mut frame = Some(InlineEventFrame {
            script: "title".into(),
            line,
            stack: Vec::new(),
            claimed_by_jump: false,
        });
        interpreter
            .engine_context()
            .lock()
            .unwrap()
            .tag_queue
            .push((
                "call".into(),
                HashMap::from([
                    ("file".into(), "system/script.asb".into()),
                    ("label".into(), "estag01".into()),
                ]),
            ));

        let drain = interpreter.drain_queued_tags_only().unwrap();
        assert!(drain.saw_call);
        settle_inline_event_frame(
            &mut interpreter,
            &mut frame,
            drain.wait.is_some(),
            false,
            drain.saw_call,
            drain.saw_jump,
        )
        .unwrap();

        assert!(frame.is_none());
        assert_eq!(interpreter.call_stack().len(), 1);
        assert!(matches!(
            interpreter.run().unwrap(),
            ExecutionResult::Wait(_)
        ));
        assert_eq!(interpreter.current_script(), Some("title"));
        assert_eq!(interpreter.current_line(), line);
        assert!(interpreter.call_stack().is_empty());
    }

    #[test]
    fn trans_input_skip_requires_trans_wait_progress_and_input() {
        // 非转场等待：永不尝试。
        assert!(!trans_input_skip_requested(false, true, true, true));
        // 转场已结束（无进行中）：无需尝试。
        assert!(!trans_input_skip_requested(true, true, false, false));
        // 转场进行中但无输入意图：不尝试。
        assert!(!trans_input_skip_requested(true, false, false, true));
        // 转场进行中 + 物理点击：尝试。
        assert!(trans_input_skip_requested(true, true, false, true));
        // 转场进行中 + 跳过态（无物理点击）：尝试（让 input=2 生效）。
        assert!(trans_input_skip_requested(true, false, true, true));
    }

    #[test]
    fn trans_skip_by_input_respects_trans_input_policy() {
        use crate::compositor::Compositor;
        use crate::render_pipeline::RenderPipeline;
        use asb_interpreter::Event;

        // input=0：禁止输入跳过，点击也不结束转场。
        let mut c = Compositor::new();
        c.apply_event(&Event::Trans {
            trans_type: 1,
            time: Some(1000),
            rule: None,
            vague: None,
            input: 0,
        });
        assert!(RenderPipeline::new(&c).is_transition_in_progress());
        assert!(!RenderPipeline::new(&c).skip_transition_by_input(false));
        assert!(RenderPipeline::new(&c).is_transition_in_progress());

        // input=2：仅在已处于跳过态时结束转场。
        let mut c = Compositor::new();
        c.apply_event(&Event::Trans {
            trans_type: 1,
            time: Some(1000),
            rule: None,
            vague: None,
            input: 2,
        });
        assert!(!RenderPipeline::new(&c).skip_transition_by_input(false));
        assert!(RenderPipeline::new(&c).is_transition_in_progress());
        assert!(RenderPipeline::new(&c).skip_transition_by_input(true));
        assert!(!RenderPipeline::new(&c).is_transition_in_progress());

        // input=1（缺省）：点击直接结束转场。
        let mut c = Compositor::new();
        c.apply_event(&Event::Trans {
            trans_type: 1,
            time: Some(1000),
            rule: None,
            vague: None,
            input: 1,
        });
        assert!(RenderPipeline::new(&c).skip_transition_by_input(false));
        assert!(!RenderPipeline::new(&c).is_transition_in_progress());
    }

    #[test]
    fn autosave_triggers_only_on_user_input_waits() {
        // allow=2 语义：每次出现"用户输入等待"时保存——点击/按键等待算，
        // 定时等待、stop、SE/视频/场景 Tween 等媒体同步等待不算。
        assert!(wait_reason_is_input_wait(Some(&WaitReason::Generic)));
        assert!(wait_reason_is_input_wait(Some(&WaitReason::Generic0)));
        assert!(wait_reason_is_input_wait(Some(&WaitReason::KeyWait {
            buttons: vec![]
        })));
        assert!(!wait_reason_is_input_wait(Some(&WaitReason::Timed {
            milliseconds: 100,
            input: 1
        })));
        assert!(!wait_reason_is_input_wait(Some(&WaitReason::Stop {
            reason: None
        })));
        assert!(!wait_reason_is_input_wait(Some(&WaitReason::Se {
            id: "bar".into(),
            time: None
        })));
        assert!(!wait_reason_is_input_wait(Some(&WaitReason::VideoLayer {
            id: "mv".into()
        })));
        assert!(!wait_reason_is_input_wait(Some(
            &WaitReason::ScenarioTween { mode: 1 }
        )));
        assert!(!wait_reason_is_input_wait(None));
    }
}
