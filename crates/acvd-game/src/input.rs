//! Last-used input device tracking: switching between mouse+keyboard and controller is automatic
//! and seamless (no manual toggle key).
//!
//! Any keyboard key, mouse button, mouse motion or wheel event selects [`InputDevice::KeyboardMouse`];
//! any gamepad button or analog-stick deflection selects [`InputDevice::Gamepad`]. The current mode
//! drives the gameplay mouselook/cursor behaviour and the on-screen button hints.

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;

/// Analog stick must deflect past this radius before it counts as controller use, so idle
/// stick drift cannot flip the mode.
const STICK_DEADZONE: f32 = 0.35;
/// Mouse motion in pixels that counts as keyboard/mouse use (filters 1-pixel jitter).
const MOUSE_MOTION_EPSILON: f32 = 1.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputDevice {
    #[default]
    KeyboardMouse,
    Gamepad,
}

/// The input device the player used most recently.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct ActiveInput(pub InputDevice);

impl ActiveInput {
    pub fn device(self) -> InputDevice {
        self.0
    }

    pub fn is_gamepad(self) -> bool {
        self.0 == InputDevice::Gamepad
    }
}

/// Chooses the device from one frame of activity. Pure so the precedence rule is testable.
pub fn next_device(
    current: InputDevice,
    keyboard_active: bool,
    gamepad_active: bool,
) -> InputDevice {
    if gamepad_active {
        InputDevice::Gamepad
    } else if keyboard_active {
        InputDevice::KeyboardMouse
    } else {
        current
    }
}

pub fn track_device(
    mut active: ResMut<ActiveInput>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    pads: Query<&Gamepad>,
    mut garage: Option<ResMut<crate::garage::GarageState>>,
    mut pause: Option<ResMut<crate::pause::PauseState>>,
) {
    let keyboard_active = keys.get_just_pressed().next().is_some()
        || mouse_buttons.get_just_pressed().next().is_some()
        || motion.delta.length() > MOUSE_MOTION_EPSILON
        || scroll.delta.length_squared() > 0.0;
    let gamepad_active = pads.iter().any(|pad| {
        pad.get_just_pressed().next().is_some()
            || pad.left_stick().length() > STICK_DEADZONE
            || pad.right_stick().length() > STICK_DEADZONE
    });

    let next = next_device(active.0, keyboard_active, gamepad_active);
    if next == active.0 {
        return;
    }
    active.0 = next;
    // Rebuild device-aware button hints on the next UI sync.
    if let Some(state) = garage.as_mut() {
        state.ui_dirty = true;
    }
    if let Some(state) = pause.as_mut() {
        state.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_activity_selects_the_last_used_input() {
        assert_eq!(
            next_device(InputDevice::KeyboardMouse, false, true),
            InputDevice::Gamepad
        );
        assert_eq!(
            next_device(InputDevice::Gamepad, true, false),
            InputDevice::KeyboardMouse
        );
        // Simultaneous activity keeps the controller selected, matching the gameplay
        // mouselook rule (the mouse only takes over on a mouse-only frame).
        assert_eq!(
            next_device(InputDevice::KeyboardMouse, true, true),
            InputDevice::Gamepad
        );
        // Idle frames never change the mode.
        assert_eq!(
            next_device(InputDevice::Gamepad, false, false),
            InputDevice::Gamepad
        );
    }
}
