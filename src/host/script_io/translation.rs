use super::*;
use crate::text::TextSpanToken;

pub const UI_TRANSLATE: MessageKind = MessageKind {
    domain: Domain::Ui,
    kind: 7,
};

const MAX_PENDING: usize = 64;
const MAX_TEXT_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub(super) struct Translations {
    pending: BTreeMap<u64, Pending>,
}

struct Pending {
    identity: RequestIdentity,
    span: TextSpanToken,
    text: Option<String>,
}

impl Translations {
    pub(super) fn request(
        &self,
        transport: &mut SessionTransport,
        text: &str,
        ruby: Option<&str>,
    ) -> Result<Option<RequestIdentity>, ProtocolError> {
        if text.is_empty()
            || text.len().saturating_add(ruby.map_or(0, str::len)) > MAX_TEXT_BYTES
            || self.pending.len() >= MAX_PENDING
            // A blocking HTTP request and onEnterFrame IO can be live at once.
            || transport.state().pending_requests.saturating_add(2) >= transport.limits().pending_requests
        {
            return Ok(None);
        }
        let body = MessageBody::new(json!({"text":text,"ruby":ruby}).to_string(), vec![])?;
        match transport.request(UI_TRANSLATE, body) {
            Ok(identity) => Ok(Some(identity)),
            Err(ProtocolError::Limit | ProtocolError::WouldBlock) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(super) fn track(&mut self, identity: RequestIdentity, span: TextSpanToken) {
        self.pending.insert(
            identity.request_id,
            Pending {
                identity,
                span,
                text: None,
            },
        );
    }

    pub(super) fn accepts(&self, identity: RequestIdentity) -> bool {
        self.pending
            .get(&identity.request_id)
            .is_some_and(|pending| pending.identity == identity && pending.text.is_none())
    }

    pub(super) fn advance(
        &mut self,
        scene: &scene::SceneIo,
        transport: &mut SessionTransport,
    ) -> Result<(), ProtocolError> {
        let mut ready = Vec::new();
        let mut discard = Vec::new();
        for (id, pending) in &mut self.pending {
            if let Some(reply) = transport.take_reply(pending.identity) {
                pending.text = decode_reply(&reply)?;
                if pending.text.is_none() {
                    discard.push(*id);
                    continue;
                }
            }
            match scene.translation_span_ready(&pending.span) {
                None => discard.push(*id),
                Some(true) if pending.text.is_some() => ready.push(*id),
                _ => {}
            }
        }
        for id in discard {
            let pending = self.pending.remove(&id).unwrap();
            let _ = transport.cancel(pending.identity);
        }
        for id in ready {
            let pending = self.pending.get(&id).unwrap();
            // Applying a longer translation may start revealing its new tail.
            if scene.translation_span_ready(&pending.span) != Some(true) {
                continue;
            }
            let Pending { span, text, .. } = self.pending.remove(&id).unwrap();
            if let Some(delta) = scene.replace_translation(&span, &text.unwrap())
                && delta != 0
            {
                for next in self.pending.values_mut().map(|pending| &mut pending.span) {
                    if next.layer_id == span.layer_id
                        && next.generation == span.generation
                        && next.page_tag_index > span.page_tag_index
                    {
                        next.start = next.start.saturating_add_signed(delta);
                        next.end = next.end.saturating_add_signed(delta);
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn clear(&mut self, transport: &mut SessionTransport) {
        for pending in std::mem::take(&mut self.pending).into_values() {
            if transport.cancel(pending.identity).is_err() {
                transport.take_reply(pending.identity);
            }
        }
    }
}

pub(super) fn decode_reply(reply: &HostReply) -> Result<Option<String>, ProtocolError> {
    let invalid = || ProtocolError::Argument("invalid translation reply");
    if !reply.body.blob().is_empty() {
        return Err(invalid());
    }
    let metadata: Value = serde_json::from_str(reply.body.metadata()).map_err(|_| invalid())?;
    match metadata["status"].as_str() {
        Some("ok") => metadata["text"]
            .as_str()
            .filter(|text| text.len() <= MAX_TEXT_BYTES)
            .map(|text| Some(text.to_owned()))
            .ok_or_else(invalid),
        Some("cancelled" | "denied" | "unsupported" | "io_error" | "timeout" | "limit") => Ok(None),
        _ => Err(invalid()),
    }
}
