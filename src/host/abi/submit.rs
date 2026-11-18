use super::*;
use crate::host::input::{HostInput, TouchPhase};
use crate::host::media::{PlaybackStatus, VideoFrame};
use crate::host::protocol::{Domain, HostReply, MessageBody, MessageKind, RequestIdentity};
use serde_json::Value;

#[repr(C)]
pub(super) struct Message {
    struct_size: u32,
    domain: u16,
    schema_version: u16,
    kind: u32,
    flags: u32,
    session_epoch: u64,
    request_id: u64,
    metadata_utf8: Bytes,
    blob: Bytes,
}

pub(super) unsafe extern "C" fn submit(rt: u64, messages: *const Message, count: u32) -> i32 {
    boundary(|| {
        let rt = RuntimeHandle::from_raw(rt);
        let state = runtime::get_state(rt)?;
        if count == 0 {
            return Ok(OK);
        }
        let epoch = state.transport.epoch;
        let limits = TransportLimits::default();
        let count = count as usize;
        if count > limits.batch_messages {
            return Err(Failure::new(LIMIT, "too many messages"));
        }
        check_ptr(messages)?;
        let messages = unsafe { std::slice::from_raw_parts(messages, count) };
        let mut family = None;
        for message in messages {
            if message.struct_size as usize != size_of::<Message>() {
                return Err(Failure::new(ARGUMENT, "invalid message stride"));
            }
            if message.schema_version != 1 || message.flags != 0 {
                return Err(Failure::new(UNSUPPORTED, "unknown message schema or flags"));
            }
            if message.session_epoch != epoch {
                return Err(Failure::new(STALE, "message session epoch is stale"));
            }
            let class = classify(message)?;
            if let Some(previous) = family
                && previous != class
            {
                return Err(Failure::new(
                    ARGUMENT,
                    "submit batch mixes message families",
                ));
            }
            family = Some(class);
        }
        match family.unwrap() {
            Family::Reply => {
                let mut replies = Vec::with_capacity(count);
                let mut total = 0usize;
                for message in messages {
                    let metadata = unsafe { message.metadata_utf8.utf8(limits.metadata_bytes)? };
                    let blob = unsafe { message.blob.slice(limits.blob_bytes)? };
                    total = total
                        .checked_add(64 + metadata.len() + blob.len())
                        .filter(|bytes| *bytes <= limits.replies.bytes)
                        .ok_or(Failure::new(LIMIT, "reply batch exceeds byte limit"))?;
                    let body = MessageBody::new(metadata.to_owned(), blob.to_vec())
                        .map_err(RuntimeError::from)?;
                    replies.push(HostReply {
                        body,
                        identity: RequestIdentity {
                            runtime: rt.as_raw(),
                            epoch,
                            request_id: message.request_id,
                            service: MessageKind {
                                domain: match message.domain {
                                    3 => Domain::Io,
                                    4 => Domain::Ui,
                                    6 => Domain::Render,
                                    _ => unreachable!("validated reply domain"),
                                },
                                kind: message.kind,
                            },
                        },
                    });
                }
                runtime::submit_replies(rt, &replies)?;
            }
            Family::Input => {
                let events = messages
                    .iter()
                    .map(|message| unsafe { input(message) })
                    .collect::<Result<Vec<_>, _>>()?;
                runtime::submit_input(rt, epoch, &events)?;
            }
            Family::Control if count == 1 => unsafe { control(rt, epoch, &messages[0])? },
            Family::Control => {
                return Err(Failure::new(
                    ARGUMENT,
                    "control message must be submitted alone",
                ));
            }
        }
        Ok(OK)
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Reply,
    Input,
    Control,
}

fn classify(message: &Message) -> Result<Family, Failure> {
    match (message.domain, message.kind, message.request_id) {
        (3, 1 | 2 | 4 | 5, 1..) | (4, 2 | 5, 1..) | (6, 1 | 2, 1..) => Ok(Family::Reply),
        (2, 1..=5, 0) => Ok(Family::Input),
        (1 | 5, 16 | 17, 0) => Ok(Family::Control),
        _ => Err(Failure::new(UNSUPPORTED, "unknown submitted message")),
    }
}

fn invalid() -> Failure {
    Failure::new(ARGUMENT, "invalid submitted message metadata or blob")
}

unsafe fn metadata(message: &Message) -> Result<Value, Failure> {
    let text = unsafe { message.metadata_utf8.utf8(4096)? };
    let value: Value = serde_json::from_str(text).map_err(|_| invalid())?;
    if !value.is_object() {
        return Err(invalid());
    }
    Ok(value)
}

fn u32_at(value: &Value, key: &str) -> Result<u32, Failure> {
    value[key]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(invalid)
}

fn i32_at(value: &Value, key: &str) -> Result<i32, Failure> {
    value[key]
        .as_i64()
        .and_then(|n| i32::try_from(n).ok())
        .ok_or_else(invalid)
}

fn u64_at(value: &Value, key: &str) -> Result<u64, Failure> {
    let raw = value[key].as_str().ok_or_else(invalid)?;
    let id = raw.parse::<u64>().map_err(|_| invalid())?;
    if raw != id.to_string() {
        return Err(invalid());
    }
    Ok(id)
}

unsafe fn input(message: &Message) -> Result<HostInput, Failure> {
    let data = unsafe { metadata(message)? };
    if message.blob.size != 0 {
        return Err(invalid());
    }
    match message.kind {
        1 => Ok(HostInput::Key {
            code: u32_at(&data, "code")?,
            down: data["down"].as_bool().ok_or_else(invalid)?,
        }),
        2 => Ok(HostInput::Pointer {
            x: i32_at(&data, "x")?,
            y: i32_at(&data, "y")?,
        }),
        3 => Ok(HostInput::Touch {
            id: u32_at(&data, "id")?,
            x: i32_at(&data, "x")?,
            y: i32_at(&data, "y")?,
            phase: match data["phase"].as_str() {
                Some("down") => TouchPhase::Down,
                Some("move") => TouchPhase::Move,
                Some("up") => TouchPhase::Up,
                _ => return Err(invalid()),
            },
        }),
        4 => Ok(HostInput::FocusLost),
        5 => Ok(HostInput::WindowButton {
            button: u32_at(&data, "button")?,
        }),
        _ => unreachable!(),
    }
}

unsafe fn control(rt: RuntimeHandle, epoch: u64, message: &Message) -> Result<(), Failure> {
    let data = unsafe { metadata(message)? };
    match (message.domain, message.kind) {
        (1, 16) if message.blob.size == 0 => runtime::submit_shutdown(rt, epoch)?,
        (1, 17) => {
            runtime::set_default_font(rt, epoch, unsafe { message.blob.slice(MAX_FILE_BYTES)? })?
        }
        (5, 16) if message.blob.size == 0 => {
            let status = match data["status"].as_str() {
                Some("started") => PlaybackStatus::Started,
                Some("ended") => PlaybackStatus::Ended,
                Some("failed") => PlaybackStatus::Failed,
                Some("cancelled") => PlaybackStatus::Cancelled,
                _ => return Err(invalid()),
            };
            runtime::submit_playback_status(rt, epoch, u64_at(&data, "playback_id")?, status)?;
        }
        (5, 17) => runtime::submit_video_frame(
            rt,
            epoch,
            VideoFrame {
                playback_id: u64_at(&data, "playback_id")?,
                frame_id: u64_at(&data, "frame_id")?,
                width: u32_at(&data, "width")?,
                height: u32_at(&data, "height")?,
                rgba: unsafe { message.blob.slice(MAX_FILE_BYTES)? },
            },
        )?,
        _ => return Err(invalid()),
    }
    Ok(())
}
