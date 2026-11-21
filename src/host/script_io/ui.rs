use super::*;

pub const UI_CAPTION: MessageKind = MessageKind {
    domain: Domain::Ui,
    kind: 1,
};
pub const UI_DIALOG: MessageKind = MessageKind {
    domain: Domain::Ui,
    kind: 2,
};
pub const UI_WINDOW_ACTION: MessageKind = MessageKind {
    domain: Domain::Ui,
    kind: 3,
};
pub const UI_AVOID: MessageKind = MessageKind {
    domain: Domain::Ui,
    kind: 4,
};

pub const UI_OPEN_BROWSER: MessageKind = MessageKind {
    domain: Domain::Ui,
    kind: 8,
};

impl ScriptIoSession {
    // None means this event was consumed and the interpreter can continue.
    pub(super) fn dispatch_ui_event(&mut self) -> Result<Option<ScriptIoProgress>, ProtocolError> {
        match self.scenario_event.as_ref() {
            Some(Event::Caption { data }) => {
                let body = MessageBody::new(json!({"text": data}).to_string(), vec![])?;
                self.transport.notify(UI_CAPTION, body)?;
            }
            Some(Event::Exec { command, .. })
                if matches!(command.as_str(), "fullscreen" | "minimize") =>
            {
                let body = MessageBody::new(json!({"command": command}).to_string(), vec![])?;
                self.transport.notify(UI_WINDOW_ACTION, body)?;
            }
            Some(Event::OpenBrowser { url }) => {
                if self.open_browser_enabled && !url.is_empty() {
                    let body = MessageBody::new(json!({"url": url}).to_string(), vec![])?;
                    if body.metadata().len() <= self.limits().metadata_bytes {
                        self.transport.notify(UI_OPEN_BROWSER, body)?;
                    }
                }
            }
            Some(Event::CallNative { result, .. }) => {
                if let Some(name) = result {
                    self.interpreter
                        .set_variable(name, asb_interpreter::Value::String(String::new()));
                }
            }
            Some(Event::ShowDialog {
                title,
                message,
                varname,
                textfield,
                textfield_size,
            }) => {
                if self.dialog_request.is_none() {
                    let initial_text = textfield.as_deref().map(|name| {
                        self.interpreter
                            .get_variable(name)
                            .map(|value| value.as_string())
                            .unwrap_or_default()
                    });
                    let body = MessageBody::new(
                        json!({"title": title, "message": message, "allow_cancel": varname.is_some(),
                            "initial_text": initial_text,
                            "max_characters": textfield_size.map(|size| size.to_string())})
                        .to_string(),
                        vec![],
                    )?;
                    self.dialog_request = Some(self.transport.request(UI_DIALOG, body)?);
                    return Ok(Some(ScriptIoProgress::Waiting));
                }
                let Some(reply) = self.transport.pop_reply() else {
                    return Ok(Some(ScriptIoProgress::Waiting));
                };
                let (accepted, text) = decode_dialog_reply(&reply)?;
                if let Some(name) = varname {
                    self.interpreter
                        .set_variable(name, asb_interpreter::Value::Int(i64::from(accepted)));
                }
                if accepted && let Some(name) = textfield {
                    let mut text = text.unwrap_or_default();
                    if let Some(limit) = textfield_size
                        && let Some((end, _)) = text.char_indices().nth(*limit)
                    {
                        text.truncate(end);
                    }
                    self.interpreter
                        .set_variable(name, asb_interpreter::Value::String(text));
                }
                self.dialog_request = None;
            }
            _ => return Ok(Some(ScriptIoProgress::Event)),
        }
        self.resume_scenario()?;
        Ok(None)
    }
}

pub(super) fn decode_dialog_reply(
    reply: &HostReply,
) -> Result<(bool, Option<String>), ProtocolError> {
    let invalid = || ProtocolError::Argument("invalid dialog reply");
    if !reply.body.blob().is_empty() {
        return Err(invalid());
    }
    let metadata: Value = serde_json::from_str(reply.body.metadata()).map_err(|_| invalid())?;
    match metadata.get("status").and_then(Value::as_str) {
        Some("ok") => {
            let accepted = metadata
                .get("accepted")
                .and_then(Value::as_bool)
                .ok_or_else(invalid)?;
            let text = match metadata.get("text") {
                Some(value) => Some(value.as_str().ok_or_else(invalid)?.to_owned()),
                None => None,
            };
            Ok((accepted, text))
        }
        Some("cancelled" | "denied" | "unsupported" | "io_error") => Ok((false, None)),
        _ => Err(invalid()),
    }
}
