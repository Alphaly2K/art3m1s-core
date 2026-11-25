use crate::{RfvpHostInputEvent, RfvpPointerButton, RfvpTouchPhase};

const ART3M1S_RFVP_STATUS_INVALID_ARGUMENT: i32 = -1;

pub const ART3M1S_RFVP_INPUT_KEY: u32 = 1;
pub const ART3M1S_RFVP_INPUT_TEXT: u32 = 2;
pub const ART3M1S_RFVP_INPUT_POINTER_MOVE: u32 = 3;
pub const ART3M1S_RFVP_INPUT_POINTER_BUTTON: u32 = 4;
pub const ART3M1S_RFVP_INPUT_WHEEL: u32 = 5;
pub const ART3M1S_RFVP_INPUT_TOUCH: u32 = 6;
pub const ART3M1S_RFVP_INPUT_FOCUS: u32 = 7;
pub const ART3M1S_RFVP_INPUT_QUIT: u32 = 8;

pub const ART3M1S_RFVP_INPUT_PHASE_DOWN: u32 = 0;
pub const ART3M1S_RFVP_INPUT_PHASE_UP: u32 = 1;
pub const ART3M1S_RFVP_INPUT_PHASE_REPEAT: u32 = 2;
pub const ART3M1S_RFVP_INPUT_PHASE_MOVE: u32 = 3;

pub const ART3M1S_RFVP_POINTER_LEFT: u32 = 1 << 0;
pub const ART3M1S_RFVP_POINTER_RIGHT: u32 = 1 << 1;
pub const ART3M1S_RFVP_POINTER_MIDDLE: u32 = 1 << 2;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Art3m1sRfvpInputEventV1 {
    pub struct_size: u32,
    pub kind: u32,
    pub code: u32,
    pub phase: u32,
    pub x: i32,
    pub y: i32,
    pub value: i32,
    pub modifiers: u32,
    pub id: u64,
}

pub fn convert_input_event(event: &Art3m1sRfvpInputEventV1) -> Result<RfvpHostInputEvent, i32> {
    if (event.struct_size as usize) < std::mem::size_of::<Art3m1sRfvpInputEventV1>() {
        return Err(ART3M1S_RFVP_STATUS_INVALID_ARGUMENT);
    }
    let event = match event.kind {
        ART3M1S_RFVP_INPUT_KEY => RfvpHostInputEvent::Key {
            code: event.code,
            pressed: event.phase != ART3M1S_RFVP_INPUT_PHASE_UP,
            repeat: event.phase == ART3M1S_RFVP_INPUT_PHASE_REPEAT,
            modifiers: event.modifiers,
        },
        ART3M1S_RFVP_INPUT_TEXT => {
            let Some(character) = char::from_u32(event.code) else {
                return Err(ART3M1S_RFVP_STATUS_INVALID_ARGUMENT);
            };
            RfvpHostInputEvent::Text { character }
        }
        ART3M1S_RFVP_INPUT_POINTER_MOVE => RfvpHostInputEvent::PointerMove {
            x: event.x,
            y: event.y,
        },
        ART3M1S_RFVP_INPUT_POINTER_BUTTON => RfvpHostInputEvent::PointerButton {
            button: match event.code {
                ART3M1S_RFVP_POINTER_LEFT => RfvpPointerButton::Left,
                ART3M1S_RFVP_POINTER_RIGHT => RfvpPointerButton::Right,
                ART3M1S_RFVP_POINTER_MIDDLE => RfvpPointerButton::Middle,
                _ => return Err(ART3M1S_RFVP_STATUS_INVALID_ARGUMENT),
            },
            pressed: event.phase == ART3M1S_RFVP_INPUT_PHASE_DOWN,
            x: event.x,
            y: event.y,
        },
        ART3M1S_RFVP_INPUT_WHEEL => RfvpHostInputEvent::Wheel {
            delta_x: event.x,
            delta_y: event.y,
        },
        ART3M1S_RFVP_INPUT_TOUCH => RfvpHostInputEvent::Touch {
            id: event.id,
            phase: match event.phase {
                ART3M1S_RFVP_INPUT_PHASE_DOWN => RfvpTouchPhase::Down,
                ART3M1S_RFVP_INPUT_PHASE_MOVE => RfvpTouchPhase::Move,
                ART3M1S_RFVP_INPUT_PHASE_UP => RfvpTouchPhase::Up,
                _ => return Err(ART3M1S_RFVP_STATUS_INVALID_ARGUMENT),
            },
            x: event.x,
            y: event.y,
        },
        ART3M1S_RFVP_INPUT_FOCUS => RfvpHostInputEvent::Focus {
            focused: event.phase != 0,
        },
        ART3M1S_RFVP_INPUT_QUIT => RfvpHostInputEvent::Quit,
        _ => return Err(ART3M1S_RFVP_STATUS_INVALID_ARGUMENT),
    };
    Ok(event)
}
