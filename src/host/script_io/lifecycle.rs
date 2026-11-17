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
    pub(in crate::host) fn submit_shutdown(&mut self, epoch: u64) -> Result<(), ProtocolError> {
        use crate::host::runtime::RuntimePhase;

        let state = self.runtime_state();
        if epoch != state.transport.epoch {
            return Err(ProtocolError::Stale);
        }
        if state.phase != RuntimePhase::Running {
            return Err(ProtocolError::State);
        }
        if self.dialog_request.is_some()
            || self.operation.is_some()
            || self.cancelled
            || self.interpreter.pending_host_io().is_some()
            || self.event_filter.is_some()
            || self.save_work.is_some()
            || self.numbered_load.is_some()
            || (matches!(self.scenario_event, Some(Event::Exit))
                && !matches!(self.shutdown, Shutdown::ExitNotified))
        {
            return Err(ProtocolError::WouldBlock);
        }
        self.frame_handler_due = false;
        self.shutdown = Shutdown::Saving;
        self.save_work = Some(SaveWork::Queued { key: None });
        Ok(())
    }

    pub(in crate::host) fn step_queued_frame(
        &mut self,
        frame: HostFrame,
    ) -> Result<ScriptIoProgress, ScriptIoError> {
        let mut progress = self.step_frame(frame)?;
        loop {
            let dispatched = match progress {
                ScriptIoProgress::Event if matches!(self.scenario_event, Some(Event::Exit)) => {
                    self.notify_exit().map(Some)
                }
                ScriptIoProgress::Event if self.has_timed_wait() => self.advance_timed_wait(frame),
                ScriptIoProgress::Event if self.sync_tween_finished() => {
                    self.resume_scenario().map(|()| None)
                }
                ScriptIoProgress::Event
                    if self.scene.is_some() && matches!(self.scenario_event, Some(Event::Flip)) =>
                {
                    self.resume_scenario()
                        .map(|()| Some(ScriptIoProgress::Event))
                }
                ScriptIoProgress::Event if self.has_persistence_event() => {
                    self.start_persistence().map(|()| None)
                }
                ScriptIoProgress::Event => self.dispatch_ui_event(),
                ScriptIoProgress::SaveReady if self.shutdown.is_closing_or_closed() => {
                    self.notify_closed().map(Some)
                }
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
