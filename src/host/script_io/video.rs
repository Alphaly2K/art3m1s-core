use super::*;
use crate::host::media::{PlaybackStatus, VideoFrame};

pub const MEDIA_VIDEO_COMMAND: MessageKind = MessageKind {
    domain: Domain::Media,
    kind: 2,
};
pub(super) const VIDEO_ID_BIT: u64 = 1 << 63;
const MAX_TRACKS: usize = 16;

type Tag = (String, HashMap<String, String>);

#[derive(Default)]
pub(super) struct Video {
    serial: u64,
    tracks: HashMap<Option<String>, Track>,
    feedback: Vec<(u64, PlaybackStatus)>,
    handlers: HashMap<Option<String>, Vec<Tag>>,
    pub(super) ready_handlers: Vec<Tag>,
}

struct Track {
    id: u64,
    started: bool,
    terminal: bool,
    last_frame: Option<u64>,
}

impl Video {
    pub(super) fn submit_status(
        &mut self,
        id: u64,
        status: PlaybackStatus,
    ) -> Result<(), ProtocolError> {
        let track = self
            .tracks
            .values_mut()
            .find(|track| track.id == id)
            .ok_or(ProtocolError::Stale)?;
        if track.terminal || (status == PlaybackStatus::Started && track.started) {
            return Err(ProtocolError::Stale);
        }
        if status == PlaybackStatus::Started {
            track.started = true;
        } else {
            track.terminal = true;
            self.feedback.push((id, status));
        }
        Ok(())
    }

    pub(super) fn advance(&mut self, scene: &scene::SceneIo) {
        for (id, status) in self.feedback.drain(..) {
            let Some(layer) = self
                .tracks
                .iter()
                .find_map(|(layer, track)| (track.id == id).then(|| layer.clone()))
            else {
                continue;
            };
            self.tracks.remove(&layer);
            if let Some(layer) = &layer {
                scene.clear_video_layer(layer);
            }
            if status == PlaybackStatus::Ended
                && let Some(tags) = self
                    .handlers
                    .get(&layer)
                    .or_else(|| self.handlers.get(&None))
            {
                self.ready_handlers.extend(tags.iter().cloned());
            }
        }
        scene.advance_video_frames();
    }

    pub(super) fn finished(&self, layer: Option<&str>) -> bool {
        !self.tracks.contains_key(&layer.map(str::to_owned))
    }

    pub(super) fn is_active(&self) -> bool {
        !self.tracks.is_empty()
    }

    pub(super) fn reset_playback(&mut self) {
        self.tracks.clear();
        self.feedback.clear();
        self.ready_handlers.clear();
    }

    pub(super) fn stop_for_shutdown(
        &mut self,
        transport: &mut SessionTransport,
    ) -> Result<(), ProtocolError> {
        if self.is_active() {
            emit(transport, json!({"op":"stop_all"}))?;
            self.reset_playback();
        }
        Ok(())
    }
}

impl ScriptIoSession {
    pub(in crate::host) fn submit_video_frame(
        &mut self,
        epoch: u64,
        frame: VideoFrame<'_>,
    ) -> Result<(), ProtocolError> {
        self.check_playback_epoch(epoch)?;
        let (layer, track) = self
            .video
            .tracks
            .iter_mut()
            .find(|(_, track)| track.id == frame.playback_id)
            .ok_or(ProtocolError::Stale)?;
        if track.terminal || track.last_frame.is_some_and(|last| frame.frame_id <= last) {
            return Err(ProtocolError::Stale);
        }
        let layer = layer.as_deref().ok_or(ProtocolError::Argument(
            "fullscreen video is presented by the Host",
        ))?;
        self.scene
            .as_ref()
            .unwrap()
            .submit_video_pixels(layer, frame)?;
        track.last_frame = Some(frame.frame_id);
        Ok(())
    }

    pub(super) fn dispatch_video_event(&mut self) -> Result<bool, ProtocolError> {
        let (Some(scene), Some(event)) = (&self.scene, &self.scenario_event) else {
            return Ok(false);
        };
        match event {
            Event::VideoPlay { .. } if !self.media_enabled => {}
            Event::VideoPlay {
                id,
                file,
                skip,
                loop_play,
                delay_margin_ms,
                ..
            } => {
                let Ok(key) = scene.resolve_asset_key(file) else {
                    crate::core_warn!("video playback skipped: invalid asset key");
                    self.resume_scenario()?;
                    return Ok(true);
                };
                let serial = self
                    .video
                    .serial
                    .checked_add(1)
                    .filter(|id| *id < VIDEO_ID_BIT)
                    .ok_or(ProtocolError::Limit)?;
                let playback_id = serial | VIDEO_ID_BIT;
                let replaces = self.video.tracks.get(id).map(|track| track.id.to_string());
                if replaces.is_none() && self.video.tracks.len() >= MAX_TRACKS {
                    return Err(ProtocolError::Limit);
                }
                emit(
                    &mut self.transport,
                    json!({
                        "op":"play", "playback_id":playback_id.to_string(), "replaces":replaces,
                        "layer":id, "key":key, "skip":skip, "loop":loop_play, "delay_margin_ms":delay_margin_ms,
                    }),
                )?;
                self.video.serial = serial;
                self.video.tracks.insert(
                    id.clone(),
                    Track {
                        id: playback_id,
                        started: false,
                        terminal: false,
                        last_frame: None,
                    },
                );
                if let Some(id) = id {
                    scene.attach_video_layer(id);
                } else {
                    self.scenario_event = Some(Event::Wait {
                        reason: asb_interpreter::WaitReason::FullscreenVideo,
                    });
                    self.wait_start = None;
                    return Ok(true);
                }
            }
            Event::VideoFinishHandler {
                id,
                file,
                label,
                call,
                handler,
            } => {
                self.video.handlers.insert(
                    id.clone(),
                    crate::script_handler::handler_tags(
                        handler.as_deref(),
                        file.as_deref(),
                        label.as_deref(),
                        *call,
                        &Default::default(),
                        &[],
                    ),
                );
            }
            Event::VideoFinishHandlerDel { id } => {
                self.video.handlers.remove(id);
            }
            _ => return Ok(false),
        }
        self.resume_scenario()?;
        Ok(true)
    }
}

fn emit(transport: &mut SessionTransport, metadata: Value) -> Result<(), ProtocolError> {
    transport.notify(
        MEDIA_VIDEO_COMMAND,
        MessageBody::new(metadata.to_string(), vec![])?,
    )
}
