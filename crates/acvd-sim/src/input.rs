//! One player's per-tick input.
//!
//! Phase 0 keeps the analog fields the existing movement integrator consumes (derived from
//! keyboard / gamepad in `acvd-game`). Later phases pack these into the wire `buttons` bitfield
//! and absolute `aim_yaw`/`aim_pitch`; the absolute aim fields are already carried here so the
//! server can derive turn from them.

use glam::{Vec2, Vec3};

use crate::ids::NetId;

/// Input for one 60 Hz tick.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InputFrame {
    /// Monotonic input sequence number (wrapping).
    pub seq: u16,
    /// Move stick: x right, y forward, length at most 1.
    pub stick: Vec2,
    /// Turn command in `[-1, 1]`, positive turns left.
    pub turn: f32,
    /// Look (pitch) command in `[-1, 1]`, positive looks up.
    pub look: f32,
    pub jump: bool,
    pub boost_toggle: bool,
    pub glide: bool,
    pub quick_boost: bool,
    pub qb_held: bool,
    pub kick: bool,
    /// Combat triggers. The game's bay-shift modifier turns the hand triggers into the rack-swap
    /// bits before sampling the network frame.
    pub fire_r: bool,
    pub fire_l: bool,
    pub fire_shoulder: bool,
    pub bay_r: bool,
    pub bay_l: bool,
    /// Overed Weapon trigger (arming/fire resolved server-side in a later task).
    pub fire_ow: bool,
    /// Absolute aim yaw in radians (`Pilot::yaw + FollowCam::yaw_off`).
    pub aim_yaw: f32,
    /// Absolute aim pitch in radians.
    pub aim_pitch: f32,
    /// Player-supplied interest hint for interest management (never trusted for damage).
    pub lock_target: NetId,
}

impl InputFrame {
    /// Aim direction from the absolute aim angles, AC facing `FORWARD` (`-Z`).
    pub fn aim_direction(&self) -> Vec3 {
        let (sy, cy) = self.aim_yaw.sin_cos();
        let (sp, cp) = self.aim_pitch.sin_cos();
        Vec3::new(-sy * cp, sp, -cy * cp)
    }

    /// Copy of this frame safe for the server's starvation repeat: movement axes and absolute aim
    /// stay, every one-shot edge is cleared so a repeated frame cannot toggle boost, jump, start a
    /// Quick Boost, a kick, or a Glide Boost again. Holds that are intentionally continuous
    /// (`qb_held`) stay set.
    pub fn repeat_safe(&self) -> Self {
        Self {
            jump: false,
            boost_toggle: false,
            glide: false,
            quick_boost: false,
            kick: false,
            bay_r: false,
            bay_l: false,
            ..*self
        }
    }

    /// Neutral frame that keeps the last absolute aim (server idle fallback).
    pub fn idle(aim_yaw: f32, aim_pitch: f32, lock_target: NetId) -> Self {
        Self {
            aim_yaw,
            aim_pitch,
            lock_target,
            ..Self::default()
        }
    }
}

/// Wraps an angle difference into `(-pi, pi]`.
pub fn wrap_angle(a: f32) -> f32 {
    (a + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
}
