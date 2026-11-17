use super::*;
use crate::host::frame::{FrameSnapshot, FrameTexture};
use asb_interpreter::WaitReason;
use std::sync::Arc;

pub(super) struct Transition {
    pub source: Arc<FrameSnapshot>,
    pub rule: Option<Arc<FrameTexture>>,
    pub vague: f32,
    started: HostFrame,
    duration_ns: u64,
    input: i32,
}

impl Transition {
    pub(super) fn progress(&self, frame: HostFrame) -> f32 {
        let elapsed = frame.monotonic_ns.saturating_sub(self.started.monotonic_ns);
        (elapsed.min(self.duration_ns) as f64 / self.duration_ns as f64) as f32
    }
}

impl ScriptIoSession {
    pub(super) fn start_transition(
        &mut self,
        frame: HostFrame,
    ) -> Result<ScriptIoProgress, ProtocolError> {
        let Some(Event::Trans {
            trans_type,
            time,
            input,
            rule,
            vague,
        }) = self.scenario_event.as_ref()
        else {
            return Err(ProtocolError::State);
        };
        let duration_ns = time.unwrap_or(1000).saturating_mul(1_000_000);
        self.transition = if self.transitions_enabled
            && self.scene.is_some()
            && matches!(trans_type, 1 | 2)
            && duration_ns != 0
        {
            let rule = if *trans_type == 2 {
                rule.as_deref()
                    .and_then(|rule| self.scene.as_ref()?.transition_rule(rule))
            } else {
                None
            };
            self.frames.latest_snapshot().map(|source| Transition {
                source,
                rule,
                vague: vague.unwrap_or(32).max(1) as f32 / 255.0,
                started: frame,
                duration_ns,
                input: *input,
            })
        } else {
            None
        };
        if self.transition.is_some() {
            self.scenario_event = Some(Event::Wait {
                reason: WaitReason::SceneTransition,
            });
        } else {
            self.resume_scenario()?;
        }
        Ok(ScriptIoProgress::Event)
    }

    pub(super) fn waiting_for_transition(&self) -> bool {
        matches!(
            self.scenario_event,
            Some(Event::Wait {
                reason: WaitReason::SceneTransition
            })
        )
    }

    pub(super) fn advance_transition_wait(
        &mut self,
        frame: HostFrame,
    ) -> Result<Option<ScriptIoProgress>, ProtocolError> {
        if let Some(transition) = &self.transition {
            let skip = frame.id != transition.started.id
                && !matches!(transition.input, 0 | 2)
                && self
                    .scene
                    .as_ref()
                    .is_some_and(|scene| scene.with_input(|input| input.wants_advance()));
            if !skip
                && frame.monotonic_ns - transition.started.monotonic_ns < transition.duration_ns
            {
                return Ok(Some(ScriptIoProgress::Event));
            }
        }
        self.transition = None;
        self.resume_scenario()?;
        Ok(None)
    }
}
