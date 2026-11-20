use super::*;

#[derive(Clone, Copy)]
pub(super) enum SaveFileCaller {
    Scenario,
    Lua,
}

pub(super) enum SaveFileWork {
    Reading {
        destination: String,
        remove: Option<String>,
    },
    Writing {
        destination: String,
        remove: Option<String>,
    },
    Deleting(String),
}

impl ScriptIoSession {
    pub(super) fn has_save_file_event(&self) -> bool {
        matches!(&self.scenario_event, Some(Event::FileOperation { command, .. })
            if matches!(command.as_str(), "copy" | "move" | "delete"))
    }

    pub(super) fn start_save_file_operation(&mut self) -> Result<(), ProtocolError> {
        let Some(Event::FileOperation {
            command,
            src,
            dst,
            target,
            ..
        }) = &self.scenario_event
        else {
            return Err(ProtocolError::State);
        };
        let key = |path: &Option<String>| path.as_deref().and_then(|path| self.save_key(path).ok());
        let work = match command.as_str() {
            "delete" => key(target).map(|key| {
                (
                    Operation::Delete { key: key.clone() },
                    SaveFileWork::Deleting(key),
                )
            }),
            "copy" | "move" => key(src).zip(key(dst)).and_then(|(source, destination)| {
                Self::save_transfer(source, destination, command == "move")
            }),
            _ => return Err(ProtocolError::State),
        };
        if let Some((operation, work)) = work {
            self.operation = Some(operation);
            self.save_file_work = Some((SaveFileCaller::Scenario, work));
            return Ok(());
        }
        self.resume_scenario()
    }

    pub(super) fn save_transfer(
        source: String,
        destination: String,
        remove_source: bool,
    ) -> Option<(Operation, SaveFileWork)> {
        if source == destination {
            return None;
        }
        let remove = remove_source.then(|| source.clone());
        Some((
            Operation::Stat {
                namespace: "save",
                key: source,
                purpose: StatPurpose::Read(None),
            },
            SaveFileWork::Reading {
                destination,
                remove,
            },
        ))
    }

    pub(super) fn advance_save_file_operation(
        &mut self,
        caller: SaveFileCaller,
        work: SaveFileWork,
        response: IoResponse,
    ) -> Result<Option<IoResponse>, ProtocolError> {
        match (work, response) {
            (
                SaveFileWork::Reading {
                    destination,
                    remove,
                },
                IoResponse::Read(data),
            ) => {
                self.operation = Some(Operation::Write {
                    key: destination.clone(),
                    data,
                });
                self.save_file_work = Some((
                    caller,
                    SaveFileWork::Writing {
                        destination,
                        remove,
                    },
                ));
                return Ok(None);
            }
            (
                SaveFileWork::Writing {
                    destination,
                    remove,
                },
                IoResponse::Written,
            ) => {
                self.invalidate_save_texture(&destination);
                if let Some(key) = remove {
                    self.operation = Some(Operation::Delete { key: key.clone() });
                    self.save_file_work = Some((caller, SaveFileWork::Deleting(key)));
                    return Ok(None);
                }
            }
            (SaveFileWork::Deleting(key), IoResponse::Deleted) => {
                self.invalidate_save_texture(&key)
            }
            (_, failure @ IoResponse::Failed { .. }) => return Ok(Some(failure)),
            _ => return Err(ProtocolError::State),
        }
        Ok(Some(IoResponse::Transferred))
    }

    pub(super) fn invalidate_save_texture(&self, key: &str) {
        if let Some(scene) = &self.scene {
            scene.invalidate_texture(&format!("{}/{key}", self.save_prefix));
        }
    }
}
