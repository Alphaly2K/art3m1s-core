use super::*;

pub const RUNTIME_PERSISTENCE_RESULT: MessageKind = MessageKind {
    domain: Domain::Runtime,
    kind: 5,
};

impl ScriptIoSession {
    pub(super) fn has_persistence_event(&self) -> bool {
        matches!(
            self.scenario_event,
            Some(Event::SaveGame { .. } | Event::LoadGame { .. })
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
        let (metadata, restored) = match progress {
            ScriptIoProgress::SaveReady => {
                let Some(SaveWork::Finished(Some(result))) = &self.save_work else {
                    return Err(ProtocolError::State);
                };
                let metadata = match result {
                    SaveResult::Written => json!({"operation":"save", "status":"written"}),
                    SaveResult::Failed {
                        kind,
                        files_written,
                        ..
                    } => json!({
                        "operation":"save", "status":"failed", "reason":kind.as_str(), "files_written":files_written
                    }),
                };
                (metadata, false)
            }
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
        if progress == ScriptIoProgress::SaveReady {
            self.take_save_result();
        } else {
            self.take_numbered_load_result();
        }
        if !restored {
            self.resume_scenario()?;
        }
        Ok(())
    }
}
