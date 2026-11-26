//! Network-stable simulation identifiers. These replace Bevy `Entity` inside the simulation so
//! the same state can be replayed, snapshotted, and sent over the wire.

use std::fmt;

/// Per-match player/AC identifier. `0` means "none"; `255` is reserved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NetId(pub u8);

impl NetId {
    pub const NONE: Self = Self(0);
    pub const RESERVED: Self = Self(255);
    pub const MAX: Self = Self(254);

    pub fn is_some(self) -> bool {
        self.0 != 0
    }

    pub fn is_none(self) -> bool {
        self.0 == 0
    }
}

impl fmt::Display for NetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Stable player slot (one or more `NetId`s may belong to a player across respawns).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlayerId(pub u32);

/// Simulation projectile identifier (also used to dedupe `FireEvent`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectileId(pub u32);

/// Destructible prop identifier; matches `collision::Hit::prop_id` (`part_idx + 1`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PropId(pub u32);
