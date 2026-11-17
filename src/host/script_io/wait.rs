use super::*;
use crate::host::input::HostInput;
use asb_interpreter::WaitReason;

impl ScriptIoSession {
    pub(in crate::host) fn submit_input(
        &mut self,
        epoch: u64,
        events: &[HostInput],
    ) -> Result<(), ProtocolError> {
        if epoch != self.transport.state().epoch {
            return Err(ProtocolError::Stale);
        }
        if self.faulted || !matches!(self.shutdown, Shutdown::Open) {
            return Err(ProtocolError::State);
        }
        self.scene
            .as_ref()
            .ok_or(ProtocolError::State)?
            .with_input(|input| input.submit(events))
    }

    pub(super) fn has_frame_wait(&self) -> bool {
        matches!(
            self.scenario_event,
            Some(Event::Wait {
                reason: WaitReason::Timed { .. }
                    | WaitReason::Generic
                    | WaitReason::Generic0
                    | WaitReason::KeyWait { .. }
                    | WaitReason::Se { .. }
                    | WaitReason::VideoLayer { .. }
                    | WaitReason::FullscreenVideo
            })
        )
    }

    pub(super) fn advance_frame_wait(
        &mut self,
        frame: HostFrame,
    ) -> Result<Option<ScriptIoProgress>, ProtocolError> {
        let Some(Event::Wait { reason }) = self.scenario_event.as_ref() else {
            return Err(ProtocolError::State);
        };
        let &(start_id, start_ns) = &*self
            .wait_start
            .get_or_insert((frame.id, frame.monotonic_ns));
        if frame.id == start_id {
            return Ok(Some(ScriptIoProgress::Event));
        }
        let input_satisfied = self.scene.as_ref().is_some_and(|scene| {
            scene.with_input(|input| match reason {
                WaitReason::Timed { input: 1, .. } => input.has_press(),
                WaitReason::Generic | WaitReason::Generic0 => input.wants_advance(),
                WaitReason::KeyWait { buttons } if buttons.is_empty() => input.wants_advance(),
                WaitReason::KeyWait { buttons } => buttons.iter().any(|button| {
                    button
                        .trim()
                        .parse::<u32>()
                        .is_ok_and(|code| input.is_pressed(code))
                }),
                _ => false,
            })
        });
        let elapsed = matches!(reason, WaitReason::Timed { milliseconds, .. }
            if (frame.monotonic_ns - start_ns) / 1_000_000 >= *milliseconds);
        let audio_finished = matches!(reason, WaitReason::Se { id, time }
            if self.audio.wait_finished(id, *time, frame));
        let video_finished = match reason {
            WaitReason::VideoLayer { id } => self.video.finished(Some(id)),
            WaitReason::FullscreenVideo => self.video.finished(None),
            _ => false,
        };
        if !input_satisfied && !elapsed && !audio_finished && !video_finished {
            return Ok(Some(ScriptIoProgress::Event));
        }
        self.resume_scenario()?;
        Ok(None)
    }
}
