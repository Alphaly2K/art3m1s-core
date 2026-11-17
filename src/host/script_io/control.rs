use super::*;
use crate::host::input::{InputFrame, OVERRIDE_IS_DOWN};
use asb_interpreter::WaitReason;

const ADVANCE: i32 = 0;
const AUTO_IN: i32 = 9;
const AUTO_OUT: i32 = 10;
const AUTO_OUT_NOCLICK: i32 = 11;
const SKIP_IN: i32 = 12;
const SKIP_OUT: i32 = 13;
const CONTROL_SKIP: i32 = 14;

pub(super) struct PlaybackControl {
    keys: HashMap<i32, Vec<u32>>,
    skip_allowed: bool,
    skip_unread: bool,
    command_skip: bool,
    held_skip: bool,
    auto_allowed: bool,
    auto: bool,
    stop_on_click: bool,
    stop_on_stop: bool,
    auto_layer: Option<String>,
    auto_sync: Vec<String>,
    check_read: bool,
    ready_since: Option<u64>,
    input_frame: Option<u64>,
    blocked_click: Option<u64>,
}

impl Default for PlaybackControl {
    fn default() -> Self {
        Self {
            keys: HashMap::from([
                (ADVANCE, vec![1, 13, 137]),
                (AUTO_IN, vec![65]),
                (SKIP_IN, vec![16]),
                (SKIP_OUT, vec![16]),
                (CONTROL_SKIP, vec![17]),
            ]),
            skip_allowed: true,
            skip_unread: false,
            command_skip: false,
            held_skip: false,
            auto_allowed: true,
            auto: false,
            stop_on_click: true,
            stop_on_stop: true,
            auto_layer: None,
            auto_sync: Vec::new(),
            check_read: true,
            ready_since: None,
            input_frame: None,
            blocked_click: None,
        }
    }
}

impl PlaybackControl {
    fn keys(&self, role: i32) -> &[u32] {
        self.keys.get(&role).map_or(&[], Vec::as_slice)
    }

    pub(super) fn wants_advance(&self, input: &InputFrame, frame: HostFrame) -> bool {
        self.blocked_click != Some(frame.id)
            && self
                .keys(ADVANCE)
                .iter()
                .any(|&code| input.unclaimed_decide(code))
    }

    pub(super) fn input_blocked(&self, frame: HostFrame) -> bool {
        self.blocked_click == Some(frame.id)
    }

    pub(super) fn skipping(&self) -> bool {
        self.skip_allowed && (self.command_skip || self.held_skip)
    }

    pub(super) fn reset_wait(&mut self) {
        self.ready_since = None;
    }
}

impl ScriptIoSession {
    fn queue_mode_handler(&mut self, event: &str) {
        if let Some(scene) = self.scene.as_mut() {
            scene.queue_mode_handler(event);
        }
    }

    fn set_auto_layer(&self, id: Option<&str>, visible: bool) {
        if let (Some(scene), Some(id)) = (&self.scene, id) {
            scene.set_control_layer_visible(id, visible);
        }
    }

    fn publish_control_state(&mut self) {
        for (key, active) in [
            (
                "s.status.commandskip",
                self.control.skip_allowed && self.control.command_skip,
            ),
            (
                "s.status.controlskip",
                self.control.skip_allowed && self.control.held_skip,
            ),
            ("s.status.automode", self.control.auto),
        ] {
            self.interpreter
                .set_variable(key, asb_interpreter::Value::Int(i64::from(active)));
        }
        self.set_auto_layer(self.control.auto_layer.as_deref(), self.control.auto);
    }

    fn set_auto(&mut self, enabled: bool) {
        if enabled && !self.control.auto_allowed {
            return;
        }
        if enabled {
            self.set_command_skip(false);
        }
        if self.control.auto == enabled {
            return;
        }
        self.control.auto = enabled;
        self.control.reset_wait();
        self.publish_control_state();
        self.queue_mode_handler(if enabled { "automodein" } else { "automodeout" });
    }

    fn set_command_skip(&mut self, enabled: bool) {
        if enabled && !self.control.skip_allowed {
            return;
        }
        if enabled {
            self.set_auto(false);
        }
        if self.control.command_skip == enabled {
            return;
        }
        self.control.command_skip = enabled;
        self.control.reset_wait();
        self.publish_control_state();
        if !enabled && let Some(scene) = &self.scene {
            scene.reveal_text();
        }
        self.queue_mode_handler(if enabled {
            "commandskipin"
        } else {
            "commandskipout"
        });
    }

    pub(super) fn clear_control_modes(&mut self) {
        self.control.command_skip = false;
        self.control.held_skip = false;
        self.control.auto = false;
        self.control.reset_wait();
        self.publish_control_state();
    }

    pub(super) fn stop_skip_at_unread_text(&mut self) {
        if self.control.command_skip && self.control.check_read && !self.control.skip_unread {
            self.set_command_skip(false);
        }
    }

    pub(super) fn update_control_input(&mut self, frame: HostFrame) {
        if self.control.input_frame == Some(frame.id) || !matches!(self.shutdown, Shutdown::Open) {
            return;
        }
        let Some(scene) = &self.scene else {
            return;
        };
        self.control.input_frame = Some(frame.id);
        let (held, pressed, stop_click) = scene.with_input(|input| {
            (
                self.control
                    .keys(CONTROL_SKIP)
                    .iter()
                    .any(|&key| input.frame_bits(key) & OVERRIDE_IS_DOWN != 0),
                input.pressed_keys().collect::<Vec<_>>(),
                self.control
                    .keys(ADVANCE)
                    .iter()
                    .any(|&key| input.unclaimed_decide(key) && input.is_physical_press(key)),
            )
        });
        if self.control.held_skip != held {
            self.control.held_skip = held;
            self.control.reset_wait();
            self.publish_control_state();
            if self.control.skip_allowed {
                self.queue_mode_handler(if held {
                    "controlskipin"
                } else {
                    "controlskipout"
                });
            }
        }
        for &key in &pressed {
            if self.control.keys(AUTO_IN).contains(&key) {
                self.set_auto(true);
            } else if self.control.keys(AUTO_OUT).contains(&key)
                || self.control.keys(AUTO_OUT_NOCLICK).contains(&key)
            {
                self.set_auto(false);
            }
            let skip_out = self.control.keys(SKIP_OUT).contains(&key);
            if self.control.keys(SKIP_IN).contains(&key) {
                self.set_command_skip(!skip_out || !self.control.command_skip);
            } else if skip_out {
                self.set_command_skip(false);
            }
        }
        if self.control.auto && self.control.stop_on_click && stop_click {
            self.control.blocked_click = Some(frame.id);
            self.set_auto(false);
        }
    }

    pub(super) fn dispatch_control_event(&mut self) -> Result<bool, ProtocolError> {
        if self.scene.is_none() {
            return Ok(false);
        }
        match self.scenario_event.clone() {
            Some(Event::KeyConfig(params)) => {
                if let Some((role, keys)) = crate::host::input::parse_keyconfig(&params) {
                    self.control.keys.insert(role, keys);
                }
            }
            Some(Event::AlreadyReadConfig { mode }) => self.control.check_read = mode != 0,
            Some(Event::SkipConfig { allow, skip_unread }) => {
                if let Some(allow) = allow {
                    let control_changed =
                        allow != self.control.skip_allowed && self.control.held_skip;
                    self.control.skip_allowed = allow;
                    if !allow {
                        self.set_command_skip(false);
                    }
                    self.publish_control_state();
                    if control_changed {
                        self.queue_mode_handler(if allow {
                            "controlskipin"
                        } else {
                            "controlskipout"
                        });
                    }
                }
                if let Some(unread) = skip_unread {
                    self.control.skip_unread = unread;
                }
            }
            Some(Event::AutoModeConfig {
                allow,
                layer,
                stopbyclick,
                stopbystop,
                syncse,
            }) => {
                if layer != self.control.auto_layer {
                    self.set_auto_layer(self.control.auto_layer.as_deref(), false);
                    self.control.auto_layer = layer;
                }
                self.control.auto_allowed = allow;
                if let Some(value) = stopbyclick {
                    self.control.stop_on_click = value;
                }
                if let Some(value) = stopbystop {
                    self.control.stop_on_stop = value;
                }
                if let Some(value) = syncse {
                    self.control.auto_sync = value;
                }
                if !allow {
                    self.set_auto(false);
                } else {
                    self.publish_control_state();
                }
            }
            Some(Event::AutoSkipDisable) => {
                self.set_auto(false);
                self.set_command_skip(false);
            }
            Some(Event::Exec { command, mode }) if command == "skip" => {
                self.set_command_skip(mode.map_or(!self.control.command_skip, |mode| mode != 0));
            }
            Some(Event::Exec { command, mode }) if command == "automode" => {
                self.set_auto(mode.unwrap_or(1) != 0);
            }
            _ => return Ok(false),
        }
        self.resume_scenario()?;
        Ok(true)
    }

    pub(super) fn stop_auto_at_stop(&mut self) -> bool {
        if self.control.auto
            && self.control.stop_on_stop
            && matches!(self.scenario_event,
            Some(Event::Wait { reason: WaitReason::Stop { ref reason } }) if !reason.as_deref().is_some_and(|r| r == "video" || r == "trans" || r.starts_with("tween:")))
        {
            self.set_auto(false);
            return true;
        }
        false
    }

    pub(super) fn auto_ready(&mut self, frame: HostFrame) -> bool {
        if !self.control.auto
            || !self
                .scene
                .as_ref()
                .is_some_and(scene::SceneIo::text_revealed)
            || !self.audio.auto_ready(&self.control.auto_sync, frame)
        {
            self.control.reset_wait();
            return false;
        }
        let delay = self
            .interpreter
            .get_variable("s.automodewait")
            .and_then(|v| v.as_int())
            .and_then(|delay| u64::try_from(delay).ok())
            .unwrap_or(900);
        let ready_since = *self.control.ready_since.get_or_insert(frame.monotonic_ns);
        (frame.monotonic_ns - ready_since) / 1_000_000 >= delay
    }
}
