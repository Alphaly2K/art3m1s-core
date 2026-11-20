use super::*;

pub const RUNTIME_EXIT_REQUESTED: MessageKind = MessageKind {
    domain: Domain::Runtime,
    kind: 3,
};
pub const RUNTIME_CLOSED: MessageKind = MessageKind {
    domain: Domain::Runtime,
    kind: 4,
};

#[derive(Default)]
pub(in crate::host) enum Shutdown {
    #[default]
    Open,
    ExitNotified,
    Saving,
    Reporting(SaveResult),
    Closed,
}

impl Shutdown {
    pub(in crate::host) fn is_closing_or_closed(&self) -> bool {
        matches!(self, Self::Saving | Self::Reporting(_) | Self::Closed)
    }
}

impl ScriptIoSession {
    fn save_blocked(&self) -> bool {
        self.dialog_request.is_some()
            || self.http.is_pending()
            || self.screenshot.pending_request().is_some()
            || self.operation.is_some()
            || self.cancelled
            || self.interpreter.pending_host_io().is_some()
            || self.event_filter.is_some()
            || self.save_work.is_some()
            || self.save_file_work.is_some()
            || self.numbered_load.is_some()
    }

    pub(in crate::host) fn submit_shutdown(&mut self, epoch: u64) -> Result<(), ProtocolError> {
        use crate::host::runtime::RuntimePhase;

        let state = self.runtime_state();
        if epoch != state.transport.epoch {
            return Err(ProtocolError::Stale);
        }
        if state.phase != RuntimePhase::Running {
            return Err(ProtocolError::State);
        }
        if self.save_blocked()
            || (matches!(self.scenario_event, Some(Event::Exit))
                && !matches!(self.shutdown, Shutdown::ExitNotified))
        {
            return Err(ProtocolError::WouldBlock);
        }
        self.frame_handler_due = false;
        self.translations.clear(&mut self.transport);
        self.shutdown = Shutdown::Saving;
        self.save_work = Some(SaveWork::Queued(if self.control.autosave_allow == 1 {
            SaveTarget::Automatic
        } else {
            SaveTarget::System
        }));
        Ok(())
    }

    pub(in crate::host) fn step_background_autosave(
        &mut self,
    ) -> Result<ScriptIoProgress, ScriptIoError> {
        use crate::host::runtime::RuntimePhase;

        if self.runtime_state().phase != RuntimePhase::Running {
            return Err(ProtocolError::State.into());
        }
        if !self.background_autosave {
            if self.control.autosave_allow != 1 {
                return Ok(ScriptIoProgress::Completed);
            }
            if self.save_blocked()
                || self.has_persistence_event()
                || !matches!(self.shutdown, Shutdown::Open)
            {
                return Ok(ScriptIoProgress::WouldBlock);
            }
            if let Some(scene) = &self.scene {
                scene.with_input(|input| input.apply_window_state());
            }
            self.background_autosave = true;
            self.save_work = Some(SaveWork::Queued(SaveTarget::Automatic));
        }

        // Run the save continuation without consuming device input or advancing a frame.
        let progress = self.step_inner();
        self.script_print.flush(&mut self.transport);
        let progress = progress.inspect_err(|_| self.mark_faulted())?;
        if progress != ScriptIoProgress::SaveReady {
            return Ok(progress);
        }
        match self.notify_save_result("autosave") {
            Ok(()) => {
                self.save_work = None;
                self.background_autosave = false;
                Ok(ScriptIoProgress::Completed)
            }
            Err(ProtocolError::WouldBlock) => Ok(ScriptIoProgress::WouldBlock),
            Err(error) => Err(error.into()),
        }
    }

    pub(in crate::host) fn step_queued_frame(
        &mut self,
        frame: HostFrame,
    ) -> Result<ScriptIoProgress, ScriptIoError> {
        let mut progress = self.step_frame(frame)?;
        loop {
            if let Err(error) = self.sync_audio_levels() {
                if error == ProtocolError::WouldBlock {
                    return Ok(if progress == ScriptIoProgress::Waiting {
                        progress
                    } else {
                        ScriptIoProgress::WouldBlock
                    });
                }
                self.mark_faulted();
                return Err(error.into());
            }
            if progress == ScriptIoProgress::Event && self.stop_auto_at_stop() {
                progress = self.step()?;
                continue;
            }
            let dispatched = match progress {
                ScriptIoProgress::Event if matches!(self.scenario_event, Some(Event::Exit)) => {
                    self.notify_exit().map(Some)
                }
                ScriptIoProgress::Event
                    if matches!(
                        self.scenario_event,
                        Some(Event::DebugConfig { .. } | Event::DebugPrint { .. })
                    ) =>
                {
                    if let Some(Event::DebugPrint { data, .. }) = &self.scenario_event {
                        crate::core_debug!("[debugprint] {}", data);
                    }
                    self.resume_scenario().map(|()| None)
                }
                ScriptIoProgress::Event if self.has_frame_wait() => self.advance_frame_wait(frame),
                ScriptIoProgress::Event if self.waiting_for_transition() => {
                    self.advance_transition_wait(frame)
                }
                ScriptIoProgress::Event if self.sync_tween_finished() => {
                    self.resume_scenario().map(|()| None)
                }
                ScriptIoProgress::Event
                    if self.scene.is_some() && matches!(self.scenario_event, Some(Event::Flip)) =>
                {
                    self.transition = None;
                    self.resume_scenario()
                        .map(|()| Some(ScriptIoProgress::Event))
                }
                ScriptIoProgress::Event
                    if matches!(self.scenario_event, Some(Event::Trans { .. })) =>
                {
                    self.start_transition(frame).map(Some)
                }
                ScriptIoProgress::Event if self.has_screenshot_event() => {
                    self.dispatch_screenshot()
                }
                ScriptIoProgress::Event if self.has_http_event() => self.dispatch_http(),
                ScriptIoProgress::Event if self.has_save_file_event() => {
                    self.start_save_file_operation().map(|()| None)
                }
                ScriptIoProgress::Event if self.has_persistence_event() => {
                    self.start_persistence().map(|()| None)
                }
                ScriptIoProgress::Event
                    if let (Some(scene), Some(Event::LayerDrag { id })) =
                        (self.scene.as_mut(), self.scenario_event.as_ref()) =>
                {
                    scene.force_drag(id);
                    self.resume_scenario().map(|()| None)
                }
                ScriptIoProgress::Event => self.dispatch_audio_event().and_then(|handled| {
                    if handled
                        || self.dispatch_video_event()?
                        || self.dispatch_control_event()?
                        || self.dispatch_text_event()?
                    {
                        Ok(None)
                    } else {
                        self.dispatch_ui_event()
                    }
                }),
                ScriptIoProgress::SaveReady if self.shutdown.is_closing_or_closed() => {
                    self.notify_closed().map(Some)
                }
                ScriptIoProgress::SaveReady if !self.has_persistence_event() => self
                    .finish_wait_autosave()
                    .map(|()| Some(ScriptIoProgress::Event)),
                ScriptIoProgress::SaveReady | ScriptIoProgress::LoadReady => {
                    self.finish_persistence(progress).map(|()| None)
                }
                _ => return Ok(progress),
            };
            match dispatched {
                Ok(Some(progress)) => return Ok(progress),
                Ok(None) => progress = self.step()?,
                Err(ProtocolError::WouldBlock) => return Ok(ScriptIoProgress::WouldBlock),
                Err(error) => {
                    self.mark_faulted();
                    return Err(error.into());
                }
            }
        }
    }

    fn notify_exit(&mut self) -> Result<ScriptIoProgress, ProtocolError> {
        if matches!(self.shutdown, Shutdown::Open) {
            self.transport
                .notify(RUNTIME_EXIT_REQUESTED, MessageBody::default())?;
            self.shutdown = Shutdown::ExitNotified;
        }
        Ok(ScriptIoProgress::Event)
    }

    fn notify_closed(&mut self) -> Result<ScriptIoProgress, ProtocolError> {
        if matches!(self.shutdown, Shutdown::Saving) {
            let result = self.take_save_result().ok_or(ProtocolError::State)?;
            self.save_work = None;
            self.shutdown = Shutdown::Reporting(result);
        }
        self.audio.stop_for_shutdown(&mut self.transport)?;
        self.video.stop_for_shutdown(&mut self.transport)?;
        let Shutdown::Reporting(result) = &self.shutdown else {
            return Err(ProtocolError::State);
        };
        let metadata = match result {
            SaveResult::Written => json!({"save_status":"written"}),
            SaveResult::Failed {
                kind,
                files_written,
                ..
            } => json!({
                "save_status":"failed", "reason": kind.as_str(), "files_written":files_written,
            }),
        };
        self.transport.notify(
            RUNTIME_CLOSED,
            MessageBody::new(metadata.to_string(), vec![])?,
        )?;
        self.shutdown = Shutdown::Closed;
        Ok(ScriptIoProgress::Completed)
    }
}
