//! Device input normalized by the Host to Artemis keys and stage coordinates.

use super::protocol::ProtocolError;
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub(crate) fn parse_keyconfig(params: &HashMap<String, String>) -> Option<(i32, Vec<u32>)> {
    let role = params.get("role")?.trim().parse::<i32>().ok()?;
    let keys = params
        .get("keys")
        .map(|raw| {
            raw.split(|c: char| c == ',' || c.is_whitespace())
                .filter_map(|key| key.parse::<u32>().ok())
                .collect()
        })
        .unwrap_or_default();
    Some((role, keys))
}

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

pub(crate) const OVERRIDE_IS_PUSH: u32 = 2;
pub(crate) const OVERRIDE_IS_DOWN: u32 = 4;
pub(crate) const OVERRIDE_IS_DOWN_EDGE: u32 = 8;
pub(crate) const OVERRIDE_IS_UP_EDGE: u32 = 16;
pub(crate) const OVERRIDE_IS_DECIDE: u32 = 32;

#[derive(Default)]
pub(in crate::host) struct InputFrame {
    pending: Vec<HostInput>,
    now_ns: u64,
    keys: BTreeMap<u32, KeyState>,
    overrides: BTreeMap<u32, u32>,
    override_all: Option<u32>,
    effective: BTreeMap<u32, u32>,
    claimed: BTreeSet<u32>,
    pub pointer: (i32, i32),
    pub touches: Vec<(u32, i32, i32)>,
    pub pointer_present: bool,
    pub focus_lost: bool,
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
        self.overrides.clear();
        self.override_all = None;
        self.effective.clear();
        self.claimed.clear();
        self.focus_lost = false;
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
                HostInput::Pointer { x, y } => {
                    self.pointer = (x, y);
                    self.pointer_present = true;
                }
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
                    self.pointer_present = false;
                    self.focus_lost = true;
                }
            }
        }
    }

    pub fn is_down(&self, code: u32) -> bool {
        self.bits(code) & OVERRIDE_IS_DOWN != 0
    }

    pub fn is_pressed(&self, code: u32) -> bool {
        self.bits(code) & OVERRIDE_IS_DOWN_EDGE != 0
    }

    pub fn is_released(&self, code: u32) -> bool {
        self.bits(code) & OVERRIDE_IS_UP_EDGE != 0
    }

    pub fn is_push(&self, code: u32) -> bool {
        self.bits(code) & OVERRIDE_IS_PUSH != 0
    }

    pub fn is_decide(&self, code: u32) -> bool {
        self.bits(code) & OVERRIDE_IS_DECIDE != 0
    }

    pub fn override_key(&mut self, code: Option<u32>, status: Option<u32>) {
        match (code, status) {
            (Some(code), Some(bits)) => {
                self.overrides.insert(code, bits);
            }
            (Some(code), None) => {
                self.overrides.remove(&code);
            }
            (None, Some(bits)) => self.override_all = Some(bits),
            (None, None) => {
                self.overrides.clear();
                self.override_all = None;
            }
        }
    }

    fn bits(&self, code: u32) -> u32 {
        if let Some(bits) = self.overrides.get(&code).copied().or(self.override_all) {
            return bits;
        }
        let Some(key) = self.keys.get(&code) else {
            return 0;
        };
        let mut bits = 0;
        if key.pressed {
            bits |= OVERRIDE_IS_DOWN_EDGE | OVERRIDE_IS_DECIDE | OVERRIDE_IS_PUSH;
        }
        if key.released {
            bits |= OVERRIDE_IS_UP_EDGE;
        }
        if let Some(since) = key.down_since {
            bits |= OVERRIDE_IS_DOWN;
            if self.now_ns - since >= PUSH_REPEAT_NS {
                bits |= OVERRIDE_IS_PUSH;
            }
        }
        bits
    }

    pub fn freeze(&mut self) {
        self.effective = self
            .keys
            .keys()
            .chain(self.overrides.keys())
            .filter_map(|&code| {
                let bits = self.bits(code);
                (bits != 0).then_some((code, bits))
            })
            .collect();
    }

    pub fn frame_bits(&self, code: u32) -> u32 {
        self.effective.get(&code).copied().unwrap_or(0)
    }

    pub fn effective_keys(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.effective.iter().map(|(&code, &bits)| (code, bits))
    }

    pub fn claim(&mut self, code: u32) {
        self.claimed.insert(code);
    }

    pub fn is_physical_press(&self, code: u32) -> bool {
        self.keys.get(&code).is_some_and(|key| key.pressed)
    }

    pub fn is_physical_down(&self, code: u32) -> bool {
        self.keys
            .get(&code)
            .is_some_and(|key| key.down_since.is_some())
    }

    pub fn is_physical_release(&self, code: u32) -> bool {
        self.keys.get(&code).is_some_and(|key| key.released)
    }

    pub fn has_press(&self) -> bool {
        self.pressed_keys().any(|code| self.is_physical_press(code))
    }

    pub fn pressed_keys(&self) -> impl Iterator<Item = u32> + '_ {
        self.effective
            .keys()
            .copied()
            .filter(|&code| self.unclaimed_press(code))
    }

    pub fn unclaimed_press(&self, code: u32) -> bool {
        !self.claimed.contains(&code)
            && self.frame_bits(code) & (OVERRIDE_IS_DECIDE | OVERRIDE_IS_DOWN_EDGE) != 0
    }

    pub fn unclaimed_decide(&self, code: u32) -> bool {
        self.unclaimed_press(code) && self.frame_bits(code) & OVERRIDE_IS_DECIDE != 0
    }
}
