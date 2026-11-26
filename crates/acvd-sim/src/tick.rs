//! Fixed simulation clock.

use std::fmt;
use std::ops::{Add, AddAssign, Sub};

/// Simulation ticks per second.
pub const TICK_HZ: u32 = 60;
/// Seconds per simulation tick.
pub const TICK_DT: f32 = 1.0 / TICK_HZ as f32;

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
