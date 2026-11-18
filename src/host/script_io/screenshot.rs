use super::*;
use crate::compositor::Scene;
use crate::save::MessageTextSnapshot;
use std::collections::BTreeMap;

pub const RENDER_CAPTURE: MessageKind = MessageKind {
    domain: Domain::Render,
    kind: 1,
};
pub const RENDER_SAVE_CAPTURE: MessageKind = MessageKind {
    domain: Domain::Render,
    kind: 2,
};

#[derive(Default)]
pub(super) struct Screenshot {
    pending: Option<(RequestIdentity, CaptureWork)>,
    pub(super) captured: Option<CapturedScene>,
}

enum CaptureWork {
    Take(Box<CapturedScene>),
    Save(String),
}

pub(super) struct CapturedScene {
    id: u64,
    pub(super) scene: Scene,
    pub(super) text: Option<MessageTextSnapshot>,
    pub(super) shaders: Option<BTreeMap<String, String>>,
    pub(super) edits: Result<scene::EditSnapshot, ProtocolError>,
}

impl Screenshot {
    pub(super) fn pending_request(&self) -> Option<RequestIdentity> {
        self.pending.as_ref().map(|(identity, _)| *identity)
    }
}

impl ScriptIoSession {
    pub(super) fn has_screenshot_event(&self) -> bool {
        matches!(
            self.scenario_event,
            Some(Event::TakeScreenshot | Event::SaveScreenshot { .. })
        )
    }

    pub(super) fn dispatch_screenshot(
        &mut self,
    ) -> Result<Option<ScriptIoProgress>, ProtocolError> {
        if let Some(identity) = self.screenshot.pending_request() {
            let Some(reply) = self.transport.pop_reply() else {
                return Ok(Some(ScriptIoProgress::Waiting));
            };
            if reply.identity != identity {
                return Err(ProtocolError::State);
            }
            let saved = decode_screenshot_reply(&reply)?;
            let (_, work) = self.screenshot.pending.take().unwrap();
            if saved {
                match work {
                    CaptureWork::Take(captured) => self.screenshot.captured = Some(*captured),
                    CaptureWork::Save(key) => {
                        if let Some(scene) = &self.scene {
                            scene.invalidate_texture(&format!("{}/{}", self.save_prefix, key));
                        }
                    }
                }
            }
            self.resume_scenario()?;
            return Ok(None);
        }

        if self.screenshots_enabled && self.scene.is_some() {
            match self.scenario_event.as_ref() {
                Some(Event::TakeScreenshot) => {
                    self.publish_frame(ScriptIoProgress::Event)?;
                    if let Some(frame_id) = self.frames.latest_id() {
                        let body = MessageBody::new(
                            json!({"render_frame_id": frame_id.to_string()}).to_string(),
                            vec![],
                        )?;
                        let identity = self.transport.request(RENDER_CAPTURE, body)?;
                        let scene = self.scene.as_ref().unwrap();
                        self.screenshot.pending = Some((
                            identity,
                            CaptureWork::Take(Box::new(CapturedScene {
                                id: identity.request_id,
                                scene: self.capture_scene_snapshot(scene),
                                text: scene.capture_text(),
                                shaders: scene.capture_shader_assets(),
                                edits: scene.capture_layer_edits(),
                            })),
                        ));
                        return Ok(Some(ScriptIoProgress::Waiting));
                    }
                }
                Some(Event::SaveScreenshot {
                    file,
                    width,
                    height,
                }) => {
                    if let Some(capture) = &self.screenshot.captured
                        && let Ok(mut key) = self.save_key(file)
                    {
                        if !key.ends_with(".png") {
                            key.push_str(".png");
                        }
                        let body = MessageBody::new(json!({
                            "capture_id": capture.id.to_string(), "namespace": "save", "key": key,
                            "width": width.map(|w| w.max(1)), "height": height.map(|h| h.max(1)),
                        }).to_string(), vec![])?;
                        let identity = self.transport.request(RENDER_SAVE_CAPTURE, body)?;
                        self.screenshot.pending = Some((identity, CaptureWork::Save(key)));
                        return Ok(Some(ScriptIoProgress::Waiting));
                    }
                }
                _ => return Err(ProtocolError::State),
            }
        }
        self.resume_scenario()?;
        Ok(None)
    }
}

pub(super) fn decode_screenshot_reply(reply: &HostReply) -> Result<bool, ProtocolError> {
    let invalid = || ProtocolError::Argument("invalid screenshot reply");
    if !reply.body.blob().is_empty() {
        return Err(invalid());
    }
    let metadata: Value = serde_json::from_str(reply.body.metadata()).map_err(|_| invalid())?;
    match metadata["status"].as_str() {
        Some("ok") => Ok(true),
        Some("cancelled" | "denied" | "unsupported" | "io_error") => Ok(false),
        _ => Err(invalid()),
    }
}
