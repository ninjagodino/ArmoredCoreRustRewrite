//! Fixed simulation clock.

use std::fmt;
use std::ops::{Add, AddAssign, Sub};

/// Simulation ticks per second.
pub const TICK_HZ: u32 = 60;
/// Seconds per simulation tick.
pub const TICK_DT: f32 = 1.0 / TICK_HZ as f32;
/// Action-boundary rate: body action states (`BladeState`, `KickState`, `WallJumpState`) advance
/// their phase timers every second 60 Hz tick.
pub const ACTION_HZ: u32 = 30;
/// 60 Hz ticks between action-boundary updates.
pub const ACTION_TICKS: u32 = TICK_HZ / ACTION_HZ;
/// Seconds per action-boundary update.
pub const ACTION_DT: f32 = 1.0 / ACTION_HZ as f32;

/// 30 Hz body-action clock. The 360 runs the body action phase function and then, only when the
/// phase did not change, adds the owner's per-action dt (`0x828ad124..0x828ad14c`). The action
/// clock therefore ticks every 60 Hz simulation tick but only every second tick is an actionable
/// boundary (`ACTION_TICKS` = 2). Clip frames advance two per boundary because the animation
/// player still advances at the 60 Hz frame step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActionClock {
    ticks: u32,
}

impl ActionClock {
    pub const fn new() -> Self {
        Self { ticks: 0 }
    }

    /// Restart the action at the next tick; that tick is boundary 0 (an action step).
    pub fn reset(&mut self) {
        self.ticks = 0;
    }

    /// Advance one 60 Hz tick. Returns `true` when this tick is a 30 Hz action boundary.
    pub fn tick(&mut self) -> bool {
        let boundary = self.ticks % ACTION_TICKS == 0;
        self.ticks = self.ticks.wrapping_add(1);
        boundary
    }

    /// 60 Hz ticks since the action started.
    pub fn ticks(self) -> u32 {
        self.ticks
    }

    /// Number of completed action boundaries.
    pub fn action_steps(self) -> u32 {
        self.ticks / ACTION_TICKS
    }

    /// Animation clip frame: the 60 Hz clip player advances one frame per tick, so two frames per
    /// action boundary.
    pub fn clip_frame(self) -> u32 {
        self.action_steps() * ACTION_TICKS
    }
}

/// Monotonic simulation tick counter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tick(pub u32);

impl Tick {
    pub const ZERO: Self = Self(0);

    pub fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }

    pub fn saturating_add(self, ticks: u32) -> Self {
        Self(self.0.saturating_add(ticks))
    }

    pub fn since(self, earlier: Self) -> u32 {
        self.0.saturating_sub(earlier.0)
    }

    pub fn as_secs(self) -> f32 {
        self.0 as f32 * TICK_DT
    }
}

impl fmt::Display for Tick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Add<u32> for Tick {
    type Output = Self;
    fn add(self, rhs: u32) -> Self {
        Self(self.0.wrapping_add(rhs))
    }
}

impl AddAssign<u32> for Tick {
    fn add_assign(&mut self, rhs: u32) {
        self.0 = self.0.wrapping_add(rhs);
    }
}

impl Sub for Tick {
    type Output = u32;
    fn sub(self, rhs: Self) -> u32 {
        self.0.saturating_sub(rhs.0)
    }
}

/// Converts wall-clock seconds into whole simulation ticks.
pub fn ticks_for_seconds(secs: f32) -> u32 {
    (secs.max(0.0) / TICK_DT).round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_clock_is_30_hz_and_clip_is_two_frames_per_step() {
        let mut clock = ActionClock::new();
        assert!(clock.tick());
        assert!(!clock.tick());
        assert!(clock.tick());
        assert_eq!(clock.ticks(), 3);
        assert_eq!(clock.action_steps(), 1);
        assert_eq!(clock.clip_frame(), 2);
        for _ in 3..14 {
            clock.tick();
        }
        assert_eq!(clock.ticks(), 14);
        assert_eq!(clock.action_steps(), 7);
        assert_eq!(clock.clip_frame(), 14);
        clock.reset();
        assert_eq!(clock.ticks(), 0);
        assert!(clock.tick());
    }
}
