use super::*;
use asb_interpreter::WaitReason;

impl ScriptIoSession {
    pub(super) fn has_timed_wait(&self) -> bool {
        matches!(
            self.scenario_event,
            Some(Event::Wait {
                reason: WaitReason::Timed { .. }
            })
        )
    }

    pub(super) fn advance_timed_wait(
        &mut self,
        frame: HostFrame,
    ) -> Result<Option<ScriptIoProgress>, ProtocolError> {
        let Some(Event::Wait {
            reason: WaitReason::Timed { milliseconds, .. },
        }) = self.scenario_event
        else {
            return Err(ProtocolError::State);
        };
        let &(start_id, start_ns) = &*self
            .timed_wait_start
            .get_or_insert((frame.id, frame.monotonic_ns));
        // A new wait starts in the frame that reached it, even if resuming IO.
        let elapsed_ns = frame.monotonic_ns - start_ns;
        if frame.id == start_id || elapsed_ns / 1_000_000 < milliseconds {
            return Ok(Some(ScriptIoProgress::Event));
        }
        self.resume_scenario()?;
        Ok(None)
    }
}
