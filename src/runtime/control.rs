use super::CoreRuntime;
use super::input::enqueue_handler_tags;
use asb_interpreter::Value;

const AUTO_ADVANCE_DELAY_MS: u64 = 900;

#[derive(Debug, Clone)]
pub(super) struct RuntimeControlState {
    skip_allowed: bool,
    skip_unread: bool,
    skip_active: bool,
    automode_allowed: bool,
    automode_active: bool,
    automode_layer: Option<String>,
    auto_wait_elapsed_ms: u64,
    skip_wait_revealed: bool,
    skip_hold_frames: u8,
    was_skipping: bool,
}

impl Default for RuntimeControlState {
    fn default() -> Self {
        Self {
            skip_allowed: true,
            skip_unread: true,
            skip_active: false,
            automode_allowed: true,
            automode_active: false,
            automode_layer: None,
            auto_wait_elapsed_ms: 0,
            skip_wait_revealed: false,
            skip_hold_frames: 0,
            was_skipping: false,
        }
    }
}

impl RuntimeControlState {
    pub(super) fn skip_active(&self) -> bool {
        self.skip_active && self.skip_allowed
    }

    pub(super) fn automode_active(&self) -> bool {
        self.automode_active && self.automode_allowed
    }

    pub(super) fn reset_auto_wait(&mut self) {
        self.auto_wait_elapsed_ms = 0;
    }

    pub(super) fn reset_wait_flags(&mut self) {
        self.auto_wait_elapsed_ms = 0;
        self.skip_wait_revealed = false;
        self.skip_hold_frames = 0;
    }

    pub(super) fn mark_skip_wait_revealed(&mut self) {
        self.skip_wait_revealed = true;
    }

    pub(super) fn skip_wait_revealed(&self) -> bool {
        self.skip_wait_revealed
    }

    pub(super) fn should_auto_advance(
        &mut self,
        delta_ms: u64,
        text_ready: bool,
        wait_ms: u64,
    ) -> bool {
        if !self.automode_active() {
            self.auto_wait_elapsed_ms = 0;
            return false;
        }
        if !text_ready {
            self.auto_wait_elapsed_ms = 0;
            return false;
        }
        self.auto_wait_elapsed_ms = self.auto_wait_elapsed_ms.saturating_add(delta_ms);
        self.auto_wait_elapsed_ms >= wait_ms
    }
}

impl CoreRuntime {
    pub(super) fn set_skip_mode(&mut self, enabled: bool) {
        if enabled && !self.control.skip_allowed {
            return;
        }
        let next = enabled;
        if next && self.control.automode_active {
            self.set_automode_mode(false);
        }
        if self.control.skip_active == next {
            self.audio.set_skipping(next);
            return;
        }
        self.control.skip_active = next;
        self.control.reset_auto_wait();
        self.audio.set_skipping(next);
        self.sync_control_status_variables();
        if !next {
            self.control.was_skipping = true;
        }
        if !next {
            // 退出 skip 时立即把当前文字全部揭示，否则
            // 新推入的文本会卡在 advance_reveal 的初始进度（仅 1-2 字）。
            self.reveal_text_now();
        }
        self.enqueue_control_handler(if next {
            "commandskipin"
        } else {
            "commandskipout"
        });
    }

    pub(super) fn set_automode_mode(&mut self, enabled: bool) {
        if enabled && !self.control.automode_allowed {
            return;
        }
        let next = enabled;
        if next && self.control.skip_active {
            self.set_skip_mode(false);
        }
        if self.control.automode_active == next {
            return;
        }
        self.control.automode_active = next;
        self.control.reset_auto_wait();
        self.sync_control_status_variables();
        self.enqueue_control_handler(if next { "automodein" } else { "automodeout" });
    }

    pub(super) fn apply_skip_config(&mut self, allow: bool, skip_unread: bool) {
        self.control.skip_allowed = allow;
        self.control.skip_unread = skip_unread;
        if !allow {
            self.set_skip_mode(false);
        }
    }

    pub(super) fn apply_automode_config(&mut self, allow: bool, layer: Option<String>) {
        self.control.automode_allowed = allow;
        self.control.automode_layer = layer;
        if !allow {
            self.set_automode_mode(false);
        }
    }

    pub(super) fn disable_auto_skip(&mut self) {
        self.set_automode_mode(false);
        self.set_skip_mode(false);
    }

    pub(super) fn apply_exec_command(&mut self, command: &str, mode: Option<i32>) {
        match command {
            "automode" => {
                let enabled = mode.unwrap_or(1) != 0;
                self.set_automode_mode(enabled);
            }
            "skip" => {
                let enabled = mode
                    .map(|value| value != 0)
                    .unwrap_or(!self.control.skip_active);
                self.set_skip_mode(enabled);
            }
            _ => {}
        }
    }

    pub(super) fn reset_control_wait_flags(&mut self) {
        self.control.reset_wait_flags();
    }

    pub(super) fn skip_active(&self) -> bool {
        self.control.skip_active()
    }

    pub(super) fn was_skipping(&self) -> bool {
        self.control.was_skipping
    }

    pub(super) fn clear_was_skipping(&mut self) {
        self.control.was_skipping = false;
    }

    pub(super) fn should_auto_advance(&mut self, delta_ms: u64) -> bool {
        let text_ready = self.is_text_reveal_complete();
        let voice_ready = !self.is_voice_playing();
        let wait_ms = self
            .interpreter
            .get_variable("s.automodewait")
            .and_then(|value| value.as_int())
            .and_then(|value| u64::try_from(value).ok())
            .unwrap_or(AUTO_ADVANCE_DELAY_MS);
        self.control
            .should_auto_advance(delta_ms, text_ready && voice_ready, wait_ms)
    }

    pub(super) fn should_hold_for_skip_reveal(&mut self) -> bool {
        if self.control.skip_wait_revealed() {
            if self.control.skip_hold_frames < 3 {
                self.control.skip_hold_frames += 1;
                return true;
            }
            return false;
        }
        self.reveal_text_now();
        self.control.mark_skip_wait_revealed();
        self.control.skip_hold_frames = 1;
        true
    }

    fn enqueue_control_handler(&mut self, event_name: &str) {
        let Some(handler) = self.compositor.get_input_handler(event_name, "") else {
            return;
        };
        enqueue_handler_tags(
            &self.interpreter,
            handler.handler.as_deref(),
            handler.file.as_deref(),
            handler.label.as_deref(),
            handler.call,
            &handler.params,
            &[("type", event_name)],
        );
    }

    pub(super) fn sync_control_status_variables(&mut self) {
        self.interpreter.set_variable(
            "s.status.commandskip",
            Value::Int(if self.control.skip_active() { 1 } else { 0 }),
        );
        self.interpreter.set_variable(
            "s.status.automode",
            Value::Int(if self.control.automode_active() { 1 } else { 0 }),
        );
        self.interpreter
            .set_variable("s.status.controlskip", Value::Int(0));
    }
}
