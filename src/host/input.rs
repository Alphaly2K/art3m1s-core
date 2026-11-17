//! Device input normalized by the Host to Artemis keys and stage coordinates.

use super::protocol::ProtocolError;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostInput {
    Key {
        code: u32,
        down: bool,
    },
    Pointer {
        x: i32,
        y: i32,
    },
    Touch {
        id: u32,
        phase: TouchPhase,
        x: i32,
        y: i32,
    },
    FocusLost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchPhase {
    Down,
    Move,
    Up,
}

const MAX_PENDING_INPUTS: usize = 4096;
const MAX_TOUCHES: usize = 16;
const PUSH_REPEAT_NS: u64 = 500_000_000;

#[derive(Default)]
pub(in crate::host) struct InputFrame {
    pending: Vec<HostInput>,
    now_ns: u64,
    keys: BTreeMap<u32, KeyState>,
    pub pointer: (i32, i32),
    pub touches: Vec<(u32, i32, i32)>,
}

#[derive(Default)]
struct KeyState {
    down_since: Option<u64>,
    pressed: bool,
    released: bool,
}

impl InputFrame {
    pub fn submit(&mut self, events: &[HostInput]) -> Result<(), ProtocolError> {
        if events.len() > MAX_PENDING_INPUTS {
            return Err(ProtocolError::Limit);
        }
        if events
            .iter()
            .any(|event| matches!(event, HostInput::Key { code, .. } if !(1..=255).contains(code)))
        {
            return Err(ProtocolError::Argument("invalid Artemis key code"));
        }
        if self.pending.len() + events.len() > MAX_PENDING_INPUTS {
            return Err(ProtocolError::WouldBlock);
        }
        self.pending.extend_from_slice(events);
        Ok(())
    }

    pub fn advance(&mut self, now_ns: u64) {
        self.now_ns = now_ns;
        self.keys.retain(|_, key| {
            key.pressed = false;
            key.released = false;
            key.down_since.is_some()
        });
        for event in self.pending.drain(..) {
            match event {
                HostInput::Key { code, down: true } => {
                    let key = self.keys.entry(code).or_default();
                    if key.down_since.is_none() {
                        key.down_since = Some(now_ns);
                        key.pressed = true;
                    }
                }
                HostInput::Key { code, down: false } => {
                    if let Some(key) = self.keys.get_mut(&code)
                        && key.down_since.take().is_some()
                    {
                        key.released = true;
                    }
                }
                HostInput::Pointer { x, y } => self.pointer = (x, y),
                HostInput::Touch { id, phase, x, y } => {
                    let index = self.touches.iter().position(|point| point.0 == id);
                    match (phase, index) {
                        (TouchPhase::Up, Some(index)) => {
                            self.touches.remove(index);
                        }
                        (TouchPhase::Down | TouchPhase::Move, Some(index)) => {
                            self.touches[index] = (id, x, y);
                        }
                        (TouchPhase::Down, None) if self.touches.len() < MAX_TOUCHES => {
                            self.touches.push((id, x, y));
                        }
                        _ => {}
                    }
                }
                HostInput::FocusLost => {
                    self.keys.clear();
                    self.touches.clear();
                }
            }
        }
    }

    pub fn is_down(&self, code: u32) -> bool {
        self.keys
            .get(&code)
            .is_some_and(|key| key.down_since.is_some())
    }

    pub fn is_pressed(&self, code: u32) -> bool {
        self.keys.get(&code).is_some_and(|key| key.pressed)
    }

    pub fn is_released(&self, code: u32) -> bool {
        self.keys.get(&code).is_some_and(|key| key.released)
    }

    pub fn is_push(&self, code: u32) -> bool {
        self.keys.get(&code).is_some_and(|key| {
            key.pressed
                || key
                    .down_since
                    .is_some_and(|since| self.now_ns - since >= PUSH_REPEAT_NS)
        })
    }

    pub fn has_press(&self) -> bool {
        self.keys.values().any(|key| key.pressed)
    }

    pub fn wants_advance(&self) -> bool {
        [1, 13, 137].into_iter().any(|code| self.is_pressed(code))
    }
}
