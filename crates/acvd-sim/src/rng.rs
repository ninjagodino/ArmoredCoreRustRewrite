//! Deterministic PCG32 random numbers (no `rand` dependency). Every random decision in the
//! simulation is seeded from `(match_seed, tick, net_id)` so a replay or late joiner derives the
//! same result.

use crate::ids::NetId;
use crate::tick::Tick;

const MULTIPLIER: u64 = 6_364_136_223_846_793_005;

/// Minimal PCG32 generator.
#[derive(Clone, Debug)]
pub struct Pcg32 {
    state: u64,
    inc: u64,
}

impl Pcg32 {
    pub fn new(seed: u64, stream: u64) -> Self {
        let mut rng = Self {
            state: 0,
            inc: (stream << 1) | 1,
        };
        rng.next_u32();
        rng.state = rng.state.wrapping_add(seed);
        rng.next_u32();
        rng
    }

    /// Seeds a generator for one AC's decision on one tick.
    pub fn for_tick(match_seed: u64, tick: Tick, net_id: NetId) -> Self {
        let seed = match_seed
            ^ (u64::from(tick.0) << 32)
            ^ (u64::from(net_id.0) << 16)
            ^ 0x9E37_79B9_7F4A_7C15;
        Self::new(seed, u64::from(net_id.0) | 1)
    }

    /// Seeds the deterministic spread roll for one shot: `(match_seed, tick, net_id, shot_seq)`.
    pub fn for_shot(match_seed: u64, tick: u32, net_id: NetId, shot_seq: u32) -> Self {
        let seed = match_seed
            ^ (u64::from(tick) << 40)
            ^ (u64::from(net_id.0) << 24)
            ^ u64::from(shot_seq)
            ^ 0xD1B5_4A32_D192_ED03;
        Self::new(seed, u64::from(net_id.0) | 1)
    }

    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old.wrapping_mul(MULTIPLIER).wrapping_add(self.inc);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// Uniform in `[0, 1)`.
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 * (1.0 / 16_777_216.0)
    }

    /// Uniform in `[lo, hi)`.
    pub fn range_f32(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next_f32()
    }
}
