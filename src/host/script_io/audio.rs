use super::*;
use crate::host::media::{PlaybackStatus, ab_loop_file};
use crate::save::{AudioChannelSnapshot, AudioSnapshot};
use std::collections::BTreeMap;

pub const MEDIA_AUDIO_COMMAND: MessageKind = MessageKind {
    domain: Domain::Media,
    kind: 1,
};

const MAX_PLAYBACKS: usize = 1024;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Channel {
    Bgm,
    Se(String),
    Voice(String),
}

impl Channel {
    fn describe(&self) -> (&'static str, Option<&str>) {
        match self {
            Self::Bgm => ("bgm", None),
            Self::Se(id) => ("se", Some(id)),
            Self::Voice(id) => ("voice", Some(id)),
        }
    }
}

type Tag = (String, HashMap<String, String>);

#[derive(Default)]
pub(super) struct Audio {
    serial: u64,
    voice_serial: u64,
    active: BTreeMap<Channel, Playback>,
    finish_handlers: HashMap<String, Vec<Tag>>,
    pending: Vec<(u64, PlaybackStatus)>,
    pub(super) ready_handlers: Vec<Tag>,
}

struct Playback {
    id: u64,
    snapshot: AudioChannelSnapshot,
    start_ns: Option<u64>,
    start_received: bool,
    terminal_received: bool,
}

pub(super) struct AudioRestore {
    body: MessageBody,
    active: BTreeMap<Channel, Playback>,
    serial: u64,
    voice_serial: u64,
}

impl Audio {
    pub(super) fn snapshot(&self) -> AudioSnapshot {
        let mut snapshot = AudioSnapshot::default();
        for (channel, track) in &self.active {
            match channel {
                Channel::Bgm => snapshot.bgm = Some(track.snapshot.clone()),
                Channel::Se(_) => snapshot.se.push(track.snapshot.clone()),
                Channel::Voice(_) => snapshot.voice.push(track.snapshot.clone()),
            }
        }
        snapshot
    }

    pub(super) fn prepare_restore(
        &self,
        scene: &scene::SceneIo,
        snapshot: Option<&AudioSnapshot>,
        stop_video: bool,
        transport: &SessionTransport,
    ) -> Result<Option<AudioRestore>, ProtocolError> {
        let mut active = BTreeMap::new();
        let mut plays = Vec::new();
        let mut serial = self.serial;
        let mut voice_serial = self.voice_serial;
        let tracks = snapshot.into_iter().flat_map(|audio| {
            audio
                .bgm
                .iter()
                .map(|bgm| (Channel::Bgm, bgm))
                .chain(audio.se.iter().map(|se| (Channel::Se(se.id.clone()), se)))
                .chain(
                    audio
                        .voice
                        .iter()
                        .map(|voice| (Channel::Voice(voice.id.clone()), voice)),
                )
        });
        for (channel, track) in tracks {
            if active.len() >= MAX_PLAYBACKS {
                return Err(ProtocolError::Limit);
            }
            if active.contains_key(&channel) {
                return Err(ProtocolError::Argument("duplicate saved audio channel"));
            }
            if let Channel::Voice(id) = &channel
                && let Some(number) = id
                    .strip_prefix("voice:")
                    .and_then(|number| number.parse().ok())
            {
                voice_serial = voice_serial.max(number);
            }
            serial = next_audio_id(serial)?;
            let play = Play {
                file: &track.file,
                loop_play: track.loop_play,
                gain: Some(track.gain),
                pan: Some(track.pan),
                fade_ms: 0,
                buffer_ms: None,
                skippable: track.skippable,
                crossfade: false,
            };
            let (record, playback) = play.build(scene, &channel, serial, None)?;
            plays.push(record);
            active.insert(channel, playback);
        }
        if plays.is_empty() && self.active.is_empty() && !stop_video {
            return Ok(None);
        }
        let body = MessageBody::new(
            json!({"op":"restore","plays":plays,"stop_video":stop_video}).to_string(),
            vec![],
        )?;
        transport.check_notify(MEDIA_AUDIO_COMMAND, &body)?;
        Ok(Some(AudioRestore {
            body,
            active,
            serial,
            voice_serial,
        }))
    }

    pub(super) fn commit_restore(
        &mut self,
        restore: Option<AudioRestore>,
        transport: &mut SessionTransport,
    ) -> Result<(), ProtocolError> {
        if let Some(restore) = restore {
            transport.notify(MEDIA_AUDIO_COMMAND, restore.body)?;
            self.active = restore.active;
            self.serial = restore.serial;
            self.voice_serial = restore.voice_serial;
        }
        self.pending.clear();
        self.ready_handlers.clear();
        Ok(())
    }

    fn submit(&mut self, id: u64, status: PlaybackStatus) -> Result<(), ProtocolError> {
        let track = self
            .active
            .values_mut()
            .find(|track| track.id == id)
            .ok_or(ProtocolError::Stale)?;
        if track.terminal_received || (status == PlaybackStatus::Started && track.start_received) {
            return Err(ProtocolError::Stale);
        }
        if status == PlaybackStatus::Started {
            track.start_received = true;
        } else {
            track.terminal_received = true;
        }
        self.pending.push((id, status));
        Ok(())
    }

    pub(super) fn advance(&mut self, frame: HostFrame) {
        for (id, status) in self.pending.drain(..) {
            if status == PlaybackStatus::Started {
                if let Some(track) = self.active.values_mut().find(|track| track.id == id) {
                    track.start_ns = Some(frame.monotonic_ns);
                }
                continue;
            }
            // Feedback can be queued before a same-frame continuation replaces the track.
            let channel = self
                .active
                .iter()
                .find_map(|(channel, track)| (track.id == id).then(|| channel.clone()));
            if let Some(channel) = channel {
                self.active.remove(&channel);
                if status == PlaybackStatus::Ended
                    && let Some(tags) = self.finish_handlers.get(channel.describe().1.unwrap_or(""))
                {
                    self.ready_handlers.extend(tags.iter().cloned());
                }
            }
        }
    }

    pub(super) fn wait_finished(&self, id: &str, time: Option<u64>, frame: HostFrame) -> bool {
        let track = self
            .active
            .get(&Channel::Se(id.to_owned()))
            .or_else(|| self.active.get(&Channel::Voice(id.to_owned())));
        track.is_none_or(|track| {
            track
                .start_ns
                .zip(time)
                .is_some_and(|(start, time)| (frame.monotonic_ns - start) / 1_000_000 >= time)
        })
    }

    pub(super) fn auto_ready(&self, sync: &[String], frame: HostFrame) -> bool {
        if sync.is_empty() {
            !self
                .active
                .keys()
                .any(|channel| matches!(channel, Channel::Voice(_)))
        } else {
            sync.iter().all(|id| self.wait_finished(id, None, frame))
        }
    }

    pub(super) fn stop_for_shutdown(
        &mut self,
        transport: &mut SessionTransport,
    ) -> Result<(), ProtocolError> {
        if !self.active.is_empty() {
            emit(transport, json!({"op":"stop_all","fade_ms":"0"}))?;
            self.active.clear();
            self.pending.clear();
        }
        Ok(())
    }
}

impl ScriptIoSession {
    pub(in crate::host) fn submit_playback_status(
        &mut self,
        epoch: u64,
        id: u64,
        status: PlaybackStatus,
    ) -> Result<(), ProtocolError> {
        self.check_playback_epoch(epoch)?;
        if id & video::VIDEO_ID_BIT != 0 {
            self.video.submit_status(id, status)
        } else {
            self.audio.submit(id, status)
        }
    }

    pub(super) fn check_playback_epoch(&self, epoch: u64) -> Result<(), ProtocolError> {
        if epoch != self.transport.state().epoch {
            return Err(ProtocolError::Stale);
        }
        if self.faulted || self.shutdown.is_closing_or_closed() || self.scene.is_none() {
            return Err(ProtocolError::State);
        }
        Ok(())
    }

    pub(super) fn dispatch_audio_event(&mut self) -> Result<bool, ProtocolError> {
        let (Some(scene), Some(event)) = (&self.scene, &self.scenario_event) else {
            return Ok(false);
        };
        match event {
            Event::BgmPlay {
                file,
                loop_play,
                gain,
                pan,
                fade_time,
                buffer,
            } => {
                let spec = Play {
                    file,
                    loop_play: *loop_play,
                    gain: *gain,
                    pan: *pan,
                    fade_ms: fade_time.unwrap_or(0),
                    buffer_ms: *buffer,
                    skippable: false,
                    crossfade: false,
                };
                self.audio
                    .play(&mut self.transport, scene, Channel::Bgm, spec)?;
            }
            Event::BgmCrossFade {
                file,
                loop_play,
                gain,
                pan,
                time,
            } => {
                let spec = Play {
                    file,
                    loop_play: *loop_play,
                    gain: *gain,
                    pan: *pan,
                    fade_ms: *time,
                    buffer_ms: None,
                    skippable: false,
                    crossfade: true,
                };
                self.audio
                    .play(&mut self.transport, scene, Channel::Bgm, spec)?;
            }
            Event::SePlay {
                id,
                file,
                loop_play,
                gain,
                pan,
                fade_time,
                skippable,
            } => {
                let spec = Play {
                    file,
                    loop_play: *loop_play,
                    gain: self.se_gain(id).or(*gain),
                    pan: *pan,
                    fade_ms: fade_time.unwrap_or(0),
                    buffer_ms: None,
                    skippable: *skippable,
                    crossfade: false,
                };
                self.audio
                    .play(&mut self.transport, scene, Channel::Se(id.clone()), spec)?;
            }
            Event::VoicePlay {
                id,
                file,
                loop_play,
                gain,
                pan,
                fade_time,
                skippable,
            } => {
                let auto_id = id.as_ref().is_none_or(String::is_empty);
                let next_voice = self.audio.voice_serial.saturating_add(1);
                let id = if auto_id {
                    format!("voice:{next_voice}")
                } else {
                    id.clone().unwrap()
                };
                let spec = Play {
                    file,
                    loop_play: *loop_play,
                    gain: self.se_gain(&id).or(*gain),
                    pan: *pan,
                    fade_ms: fade_time.unwrap_or(0),
                    buffer_ms: None,
                    skippable: *skippable,
                    crossfade: false,
                };
                self.audio
                    .play(&mut self.transport, scene, Channel::Voice(id), spec)?;
                if auto_id {
                    self.audio.voice_serial = next_voice;
                }
            }
            Event::BgmStop { fade_time } => self.audio.control(
                &mut self.transport,
                None,
                Control::Stop(fade_time.unwrap_or(0)),
            )?,
            Event::BgmFade { gain, time } => {
                self.audio
                    .control(&mut self.transport, None, Control::Gain(*gain, *time))?
            }
            Event::BgmPan { pan, time } => self.audio.control(
                &mut self.transport,
                None,
                Control::Pan(*pan, time.unwrap_or(0)),
            )?,
            Event::SeStop { id, fade_time } => self.audio.control(
                &mut self.transport,
                Some(id),
                Control::Stop(fade_time.unwrap_or(0)),
            )?,
            Event::SeFade { id, gain, time } => {
                self.audio
                    .control(&mut self.transport, Some(id), Control::Gain(*gain, *time))?
            }
            Event::SePan { id, pan, time } => self.audio.control(
                &mut self.transport,
                Some(id),
                Control::Pan(*pan, time.unwrap_or(0)),
            )?,
            Event::StopAllSounds { duration } => {
                emit(
                    &mut self.transport,
                    json!({"op":"stop_all","fade_ms":duration.to_string()}),
                )?;
                if *duration == 0 {
                    self.audio.active.clear();
                }
            }
            Event::SoundFinishHandler {
                id,
                file,
                label,
                call,
                handler,
            } => {
                self.audio.finish_handlers.insert(
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
            Event::SoundFinishHandlerDel { id } => {
                self.audio.finish_handlers.remove(id);
            }
            _ => return Ok(false),
        }
        self.resume_scenario()?;
        Ok(true)
    }

    fn se_gain(&self, id: &str) -> Option<i32> {
        self.interpreter
            .get_variable(&format!("s.segain.{id}"))?
            .as_string()
            .parse()
            .ok()
    }
}

struct Play<'a> {
    file: &'a str,
    loop_play: bool,
    gain: Option<i32>,
    pan: Option<i32>,
    fade_ms: u64,
    buffer_ms: Option<i32>,
    skippable: bool,
    crossfade: bool,
}

enum Control {
    Stop(u64),
    Gain(i32, u64),
    Pan(i32, u64),
}

impl Audio {
    fn play(
        &mut self,
        transport: &mut SessionTransport,
        scene: &scene::SceneIo,
        channel: Channel,
        spec: Play<'_>,
    ) -> Result<(), ProtocolError> {
        let id = next_audio_id(self.serial)?;
        let replaces = self.active.get(&channel).map(|track| track.id);
        if replaces.is_none() && self.active.len() >= MAX_PLAYBACKS {
            return Err(ProtocolError::Limit);
        }
        let (record, playback) = spec.build(scene, &channel, id, replaces)?;
        emit(transport, record)?;
        self.serial = id;
        self.active.insert(channel, playback);
        Ok(())
    }

    fn control(
        &mut self,
        transport: &mut SessionTransport,
        script_id: Option<&str>,
        command: Control,
    ) -> Result<(), ProtocolError> {
        let matches = |channel: &Channel| channel.describe().1 == script_id;
        let ids: Vec<_> = self
            .active
            .iter()
            .filter(|(channel, _)| matches(channel))
            .map(|(_, track)| track.id.to_string())
            .collect();
        if ids.is_empty() {
            return Ok(());
        }
        let (op, value, time) = match command {
            Control::Stop(time) => ("stop", None, time),
            Control::Gain(value, time) => ("gain", Some(value), time),
            Control::Pan(value, time) => ("pan", Some(value), time),
        };
        emit(
            transport,
            json!({"op":op,"playback_ids":ids,"value":value,"time_ms":time.to_string()}),
        )?;
        match command {
            Control::Stop(0) => self.active.retain(|channel, _| !matches(channel)),
            Control::Stop(_) => {}
            Control::Gain(value, _) | Control::Pan(value, _) => {
                for (channel, track) in &mut self.active {
                    if matches(channel) {
                        if let Control::Gain(..) = command {
                            track.snapshot.gain = value;
                        } else {
                            track.snapshot.pan = value;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl Play<'_> {
    fn build(
        &self,
        scene: &scene::SceneIo,
        channel: &Channel,
        id: u64,
        replaces: Option<u64>,
    ) -> Result<(Value, Playback), ProtocolError> {
        let key = scene.resolve_media_asset(self.file)?;
        let loop_key = if *channel == Channel::Bgm && self.loop_play {
            ab_loop_file(self.file)
                .map(|file| scene.resolve_media_asset(&file))
                .transpose()?
        } else {
            None
        };
        let (kind, script_id) = channel.describe();
        let record = json!({
            "op":"play", "playback_id":id.to_string(), "replaces":replaces.map(|id| id.to_string()),
            "channel":kind, "id":script_id, "key":key, "loop":self.loop_play, "loop_key":loop_key,
            "gain":self.gain, "pan":self.pan, "fade_ms":self.fade_ms.to_string(), "buffer_ms":self.buffer_ms,
            "skippable":self.skippable, "crossfade":self.crossfade,
        });
        Ok((
            record,
            Playback {
                id,
                snapshot: AudioChannelSnapshot {
                    id: script_id.unwrap_or("bgm").to_owned(),
                    file: self.file.to_owned(),
                    loop_play: self.loop_play,
                    gain: self.gain.unwrap_or(1000),
                    pan: self.pan.unwrap_or(0),
                    skippable: self.skippable,
                },
                start_ns: None,
                start_received: false,
                terminal_received: false,
            },
        ))
    }
}

fn emit(transport: &mut SessionTransport, metadata: Value) -> Result<(), ProtocolError> {
    transport.notify(
        MEDIA_AUDIO_COMMAND,
        MessageBody::new(metadata.to_string(), vec![])?,
    )
}

fn next_audio_id(serial: u64) -> Result<u64, ProtocolError> {
    serial
        .checked_add(1)
        .filter(|id| *id < video::VIDEO_ID_BIT)
        .ok_or(ProtocolError::Limit)
}
