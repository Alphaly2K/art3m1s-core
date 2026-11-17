use super::*;
use crate::host::input::{InputFrame, OVERRIDE_IS_DOWN, OVERRIDE_IS_DOWN_EDGE};
use asb_interpreter::WaitReason;

const ADVANCE: i32 = 0;
const HIDE_IN: i32 = 3;
const HIDE_OUT: i32 = 4;
const BACKLOG_IN: i32 = 5;
const BACKLOG_OUT: i32 = 6;
const AUTO_IN: i32 = 9;
const AUTO_OUT: i32 = 10;
const AUTO_OUT_NOCLICK: i32 = 11;
const SKIP_IN: i32 = 12;
const SKIP_OUT: i32 = 13;
const CONTROL_SKIP: i32 = 14;
const AVOID_IN: i32 = 15;
const AVOID_OUT: i32 = 16;

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
    hide_allowed: bool,
    hide_window: Vec<String>,
    hidden_layers: Option<Vec<(String, Option<bool>)>>,
    right_click_allowed: bool,
    right_click_file: Option<String>,
    avoid_asset: Option<String>,
    avoid_windowbutton: i32,
    avoid: bool,
    avoid_dirty: bool,
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
                (HIDE_IN, vec![32]),
                (HIDE_OUT, vec![32]),
                (BACKLOG_IN, vec![38, 136]),
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
            hide_allowed: true,
            hide_window: Vec::new(),
            hidden_layers: None,
            right_click_allowed: false,
            right_click_file: None,
            avoid_asset: None,
            avoid_windowbutton: 0,
            avoid: false,
            avoid_dirty: false,
            check_read: true,
            ready_since: None,
            input_frame: None,
            blocked_click: None,
        }
    }
}

impl PlaybackControl {
    pub(super) fn avoid_pending(&self) -> bool {
        self.avoid_dirty
    }

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
    fn set_avoid(&mut self, active: bool) {
        let active = active && self.control.avoid_asset.is_some();
        if self.control.avoid != active {
            self.control.avoid = active;
            self.control.avoid_dirty = true;
        }
    }

    pub(super) fn deliver_avoid_state(&mut self) -> Result<bool, ProtocolError> {
        if !self.control.avoid_dirty {
            return Ok(true);
        }
        let data = if self.control.avoid {
            json!({"visible":true, "mute":true, "asset":self.control.avoid_asset, "windowbutton":self.control.avoid_windowbutton})
        } else {
            json!({"visible":false, "mute":false})
        };
        match self
            .transport
            .notify(UI_AVOID, MessageBody::new(data.to_string(), vec![])?)
        {
            Err(ProtocolError::WouldBlock) => return Ok(false),
            result => result?,
        }
        self.control.avoid_dirty = false;
        Ok(true)
    }

    fn dispatch_window_buttons(&mut self, buttons: Vec<u32>) {
        for button in buttons {
            if self.control.avoid {
                if self.control.avoid_windowbutton != 2 {
                    continue;
                }
                self.set_avoid(false);
            }
            self.interpreter.set_variable(
                "s.status.windowbutton",
                asb_interpreter::Value::Int(i64::from(button)),
            );
            if let Some(scene) = self.scene.as_mut() {
                let key = button.to_string();
                let params = [("button", key.as_str()), ("type", "windowbutton")];
                if !scene.queue_global_handler("windowbutton", &key, &params) {
                    scene.queue_global_handler("windowbutton", "", &params);
                }
            }
        }
    }

    fn set_hide(&mut self, hide: bool) {
        let Some(scene) = &self.scene else {
            return;
        };
        if hide {
            if !self.control.hide_allowed || self.control.hidden_layers.is_some() {
                return;
            }
            self.control.hidden_layers = Some(scene.hide_message_layers(&self.control.hide_window));
            self.queue_mode_handler("hidein");
        } else if let Some(layers) = self.control.hidden_layers.take() {
            scene.restore_hidden_layers(layers);
            self.queue_mode_handler("hideout");
        }
    }

    fn right_click(&mut self) -> bool {
        if self.control.hidden_layers.is_some() {
            self.set_hide(false);
            return true;
        }
        if !self.control.right_click_allowed {
            return false;
        }
        if let Some(scene) = self.scene.as_mut() {
            let file = self
                .control
                .right_click_file
                .as_deref()
                .unwrap_or("rclick.iet");
            scene.queue_menu(file, self.interpreter.current_script() == Some(file));
        }
        true
    }

    pub(super) fn capture_scene_snapshot(
        &self,
        scene: &scene::SceneIo,
    ) -> crate::compositor::Scene {
        let mut snapshot = scene.save_snapshot();
        for (id, visible) in self.control.hidden_layers.iter().flatten() {
            if let Some(layer) = snapshot.get_mut(id) {
                layer.props.visible = *visible;
            }
        }
        snapshot
    }

    pub(super) fn handle_early_input(&mut self) {
        let Some(scene) = &self.scene else {
            return;
        };
        let keys = [1, 2, 27];
        let buttons = scene.with_input(|input| std::mem::take(&mut input.window_buttons));
        let pressed = scene
            .with_input(|input| keys.map(|key| input.frame_bits(key) & OVERRIDE_IS_DOWN_EDGE != 0));
        let restore = self.control.hidden_layers.is_some() && pressed.into_iter().any(|down| down);
        let right = (pressed[1] || pressed[2]) && !restore && self.right_click();
        if restore {
            self.set_hide(false);
        }
        if let Some(scene) = &self.scene {
            scene.with_input(|input| {
                for (key, down) in keys.into_iter().zip(pressed) {
                    if down && (restore || (key != 1 && right)) {
                        input.claim(key);
                    }
                }
            });
        }
        self.dispatch_window_buttons(buttons);
    }

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
        self.control.hidden_layers = None;
        self.set_avoid(false);
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
            let hide_out = self.control.keys(HIDE_OUT).contains(&key);
            if self.control.keys(HIDE_IN).contains(&key) {
                self.set_hide(!hide_out || self.control.hidden_layers.is_none());
            } else if hide_out {
                self.set_hide(false);
            }
            if self.control.keys(BACKLOG_IN).contains(&key) {
                self.queue_mode_handler("backlogin");
            }
            if self.control.keys(BACKLOG_OUT).contains(&key) {
                self.queue_mode_handler("backlogout");
            }
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
            let avoid_out = self.control.keys(AVOID_OUT).contains(&key);
            if self.control.keys(AVOID_IN).contains(&key) {
                self.set_avoid(!avoid_out || !self.control.avoid);
            } else if avoid_out {
                self.set_avoid(false);
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
            Some(Event::HideConfig { allow, window }) => {
                self.control.hide_allowed = allow;
                if let Some(window) = window {
                    self.control.hide_window = window;
                }
                if !allow {
                    self.set_hide(false);
                }
            }
            Some(Event::RightClickConfig { allow, file }) => {
                self.control.right_click_allowed = allow;
                if file.as_ref().is_some_and(|file| !file.is_empty()) {
                    self.control.right_click_file = file;
                }
            }
            Some(Event::AvoidConfig { file, windowbutton }) => {
                self.control.avoid_asset =
                    file.and_then(|file| self.scene.as_ref()?.resolve_media_asset(&file).ok());
                self.control.avoid_windowbutton = windowbutton.clamp(0, 2);
                if self.control.avoid {
                    self.set_avoid(self.control.avoid_asset.is_some());
                    self.control.avoid_dirty = true;
                }
            }
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
            Some(Event::Exec { command, .. }) if command == "hide" => {
                self.set_hide(self.control.hidden_layers.is_none());
            }
            Some(Event::Exec { command, .. }) if command == "backlog" => {
                self.queue_mode_handler("backlogin");
            }
            Some(Event::Exec { command, .. }) if command == "rclick" => {
                if !self.right_click()
                    && let Some(scene) = self.scene.as_mut()
                {
                    scene.queue_global_handler("push", "2", &[("key", "2"), ("type", "click")]);
                }
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
