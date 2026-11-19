use super::*;

pub const RUNTIME_PERSISTENCE_RESULT: MessageKind = MessageKind {
    domain: Domain::Runtime,
    kind: 5,
};

impl ScriptIoSession {
    pub(super) fn capture_gameplay_wait(&mut self) {
        let Some(script) = self.interpreter.current_script() else {
            return;
        };
        let call_stack: Vec<_> = self
            .interpreter
            .call_stack()
            .iter()
            .map(crate::save::CallFrameSnapshot::from)
            .collect();
        if self.gameplay_checkpoint.as_ref().is_some_and(|checkpoint| {
            checkpoint.is_ancestor_of(script, &call_stack)
                && call_stack[checkpoint.call_stack.len()].return_line == checkpoint.line
        }) {
            return;
        }
        self.gameplay_checkpoint = Some(crate::save::GameplayCheckpoint {
            script: script.to_string(),
            line: self.interpreter.position_after_event(),
            call_stack,
            variables: self.interpreter.variables().local_snapshot(),
            waiting_for_input: true,
        });
    }

    pub(super) fn has_persistence_event(&self) -> bool {
        matches!(
            self.scenario_event,
            Some(Event::SaveGame { .. } | Event::LoadGame { .. })
        )
    }

    pub(super) fn finish_wait_autosave(&mut self) -> Result<(), ProtocolError> {
        if let SaveResult::Failed { kind, .. } =
            self.take_save_result().ok_or(ProtocolError::State)?
        {
            crate::core_warn!("automatic save failed: {}", kind.as_str());
        }
        self.save_work = None;
        Ok(())
    }

    pub(super) fn notify_save_result(&mut self, operation: &str) -> Result<(), ProtocolError> {
        let Some(SaveWork::Finished(Some(result))) = &self.save_work else {
            return Err(ProtocolError::State);
        };
        let metadata = match result {
            SaveResult::Written => json!({"operation":operation, "status":"written"}),
            SaveResult::Failed {
                kind,
                files_written,
                ..
            } => json!({
                "operation":operation, "status":"failed", "reason":kind.as_str(),
                "files_written":files_written,
            }),
        };
        self.transport.notify(
            RUNTIME_PERSISTENCE_RESULT,
            MessageBody::new(metadata.to_string(), vec![])?,
        )
    }

    pub(super) fn start_persistence(&mut self) -> Result<(), ProtocolError> {
        let saving = matches!(self.scenario_event, Some(Event::SaveGame { .. }));
        let result = if saving {
            self.submit_save()
        } else {
            self.submit_numbered_load()
        };
        match result {
            Err(error @ ProtocolError::Argument(_)) => {
                if saving {
                    self.finish_save(SaveResult::Failed {
                        kind: IoFailure::Denied,
                        message: error.to_string(),
                        files_written: 0,
                    });
                } else {
                    self.fail_numbered_load(IoFailure::Denied, error.to_string());
                }
                Ok(())
            }
            result => result,
        }
    }

    pub(super) fn finish_persistence(
        &mut self,
        progress: ScriptIoProgress,
    ) -> Result<(), ProtocolError> {
        if progress == ScriptIoProgress::SaveReady {
            self.notify_save_result("save")?;
            self.take_save_result();
            return self.resume_scenario();
        }
        let (metadata, restored) = match progress {
            ScriptIoProgress::LoadReady => {
                let Some(NumberedLoadWork::Finished(Some(result))) = &self.numbered_load else {
                    return Err(ProtocolError::State);
                };
                match result {
                    NumberedLoadResult::Restored => {
                        (json!({"operation":"load", "status":"restored"}), true)
                    }
                    NumberedLoadResult::Failed { kind, .. } => (
                        json!({
                            "operation":"load", "status":"failed", "reason":kind.as_str()
                        }),
                        false,
                    ),
                }
            }
            _ => return Err(ProtocolError::State),
        };
        self.transport.notify(
            RUNTIME_PERSISTENCE_RESULT,
            MessageBody::new(metadata.to_string(), vec![])?,
        )?;
        self.take_numbered_load_result();
        if !restored {
            self.resume_scenario()?;
        }
        Ok(())
    }
}
