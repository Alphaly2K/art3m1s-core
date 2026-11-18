use super::*;

pub const UI_HTTP_REQUEST: MessageKind = MessageKind {
    domain: Domain::Ui,
    kind: 5,
};
pub const UI_HTTP_CANCEL: MessageKind = MessageKind {
    domain: Domain::Ui,
    kind: 6,
};

#[derive(Default)]
pub(super) enum Http {
    #[default]
    Idle,
    Awaiting(RequestIdentity),
    Received(HostReply),
    Writing(String),
}

impl Http {
    pub(super) fn identity(&self) -> Option<RequestIdentity> {
        match self {
            Self::Awaiting(identity) => Some(*identity),
            _ => None,
        }
    }

    pub(super) fn is_pending(&self) -> bool {
        !matches!(self, Self::Idle)
    }

    pub(super) fn waits_for_host(&self) -> bool {
        matches!(self, Self::Awaiting(_) | Self::Received(_))
    }
}

const CANCEL_VARIABLE: &str = "s.http.cancel";

impl ScriptIoSession {
    pub(super) fn has_http_event(&self) -> bool {
        matches!(
            self.scenario_event,
            Some(Event::HttpGet { .. } | Event::HttpPost { .. })
        )
    }

    pub(super) fn dispatch_http(&mut self) -> Result<Option<ScriptIoProgress>, ProtocolError> {
        if matches!(self.http, Http::Received(_)) {
            let Http::Received(reply) = std::mem::take(&mut self.http) else {
                unreachable!()
            };
            let code = decode_reply(&reply)?;
            return self.finish_http(code, reply.body.blob());
        }
        if let Some(identity) = self.http.identity() {
            if self
                .interpreter
                .get_variable(CANCEL_VARIABLE)
                .and_then(|v| v.as_int())
                == Some(1)
            {
                let metadata = json!({"request_id": identity.request_id.to_string()});
                self.transport.notify(
                    UI_HTTP_CANCEL,
                    MessageBody::new(metadata.to_string(), vec![])?,
                )?;
                self.transport.cancel(identity)?;
                self.interpreter.set_variable(CANCEL_VARIABLE, 0_i64.into());
                return self.finish_http(0, &[]);
            }
            return Ok(Some(ScriptIoProgress::Waiting));
        }
        if !self.http_enabled {
            return self.finish_http(0, &[]);
        }

        let (method, url, headers, data, file_data) = match self.scenario_event.as_ref() {
            Some(Event::HttpGet { url, headers, .. }) => ("get", url, headers, &[][..], &[][..]),
            Some(Event::HttpPost {
                url,
                headers,
                data,
                file_data,
                ..
            }) => ("post", url, headers, data.as_slice(), file_data.as_slice()),
            _ => return Err(ProtocolError::State),
        };
        let file_data = file_data
            .iter()
            .map(|(field, path)| logical_key(path).map(|key| (field, key)))
            .collect::<Result<Vec<_>, _>>();
        let Ok(file_data) = file_data else {
            return self.finish_http(0, &[]);
        };
        let limits = self.transport.limits();
        // Reserve space for the response envelope and status metadata.
        let max_response_bytes = limits
            .blob_bytes
            .min(limits.replies.bytes.saturating_sub(128));
        let metadata = json!({
            "method": method, "url": url, "headers": headers, "data": data, "file_data": file_data,
            "max_response_bytes": max_response_bytes.to_string(),
        });
        match self.transport.request(
            UI_HTTP_REQUEST,
            MessageBody::new(metadata.to_string(), vec![])?,
        ) {
            Ok(identity) => self.http = Http::Awaiting(identity),
            Err(ProtocolError::Limit) => return self.finish_http(0, &[]),
            Err(error) => return Err(error),
        }
        self.interpreter.set_variable(CANCEL_VARIABLE, 0_i64.into());
        Ok(Some(ScriptIoProgress::Waiting))
    }

    pub(super) fn collect_http_reply(&mut self) {
        if let Some(identity) = self.http.identity()
            && let Some(reply) = self.transport.take_reply(identity)
        {
            self.http = Http::Received(reply);
        }
    }

    fn finish_http(
        &mut self,
        code: i32,
        body: &[u8],
    ) -> Result<Option<ScriptIoProgress>, ProtocolError> {
        self.http = Http::Idle;
        let (varname_code, varname_data, filename) = match self.scenario_event.as_ref() {
            Some(
                Event::HttpGet {
                    varname_code,
                    varname_data,
                    filename,
                    ..
                }
                | Event::HttpPost {
                    varname_code,
                    varname_data,
                    filename,
                    ..
                },
            ) => (varname_code, varname_data, filename),
            _ => return Err(ProtocolError::State),
        };
        if let Some(name) = varname_code {
            self.interpreter.set_variable(name, i64::from(code).into());
        }
        if let Some(filename) = filename {
            match self.save_key(filename) {
                Ok(key) => {
                    self.operation = Some(Operation::Write {
                        key: key.clone(),
                        data: body.to_vec(),
                    });
                    self.http = Http::Writing(key);
                    return Ok(None);
                }
                Err(_) => {
                    crate::core_warn!("invalid HTTP response save key");
                }
            }
        } else if let Some(name) = varname_data {
            self.interpreter
                .set_variable(name, String::from_utf8_lossy(body).into_owned().into());
        }
        self.resume_scenario()?;
        Ok(None)
    }

    pub(super) fn complete_http_write(
        &mut self,
        response: IoResponse,
    ) -> Result<(), ProtocolError> {
        let Http::Writing(key) = std::mem::take(&mut self.http) else {
            return Err(ProtocolError::State);
        };
        match response {
            IoResponse::Written => {
                if let Some(scene) = &self.scene {
                    scene.invalidate_texture(&format!("{}/{key}", self.save_prefix));
                }
            }
            IoResponse::Failed { kind, .. } => {
                crate::core_warn!("HTTP response write failed: {}", kind.as_str());
            }
            _ => return Err(ProtocolError::State),
        }
        self.resume_scenario()
    }
}

pub(super) fn decode_reply(reply: &HostReply) -> Result<i32, ProtocolError> {
    let invalid = || ProtocolError::Argument("invalid HTTP reply");
    let metadata: Value = serde_json::from_str(reply.body.metadata()).map_err(|_| invalid())?;
    match metadata["status"].as_str() {
        Some("ok") => metadata["status_code"]
            .as_i64()
            .filter(|code| (100..=599).contains(code))
            .map(|code| code as i32)
            .ok_or_else(invalid),
        Some("cancelled" | "denied" | "unsupported" | "io_error" | "timeout" | "limit")
            if reply.body.blob().is_empty() =>
        {
            Ok(0)
        }
        _ => Err(invalid()),
    }
}
