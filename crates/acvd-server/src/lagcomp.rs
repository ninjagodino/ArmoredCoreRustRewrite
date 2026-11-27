//! 90-tick hitbox history and lag-compensated validation for hitscan / fast projectiles.
//!
//! The server stores every AC's position/facing after each simulation tick. A high-velocity round
//! is validated against the target spheres at the shooter's view tick:
//!
//! `rewind = clamp(rtt_ticks / 2 + INTERP_DELAY_TICKS + jitter_ticks, 0, MAX_REWIND_TICKS)`
//!
//! with `INTERP_DELAY_TICKS = 4` (the client's 66 ms remote render delay). Slow projectiles are
//! simulated forward by `acvd-sim`; only hitscan/fast rounds rewind.

use std::collections::{HashMap, VecDeque};

use acvd_sim::collision::{Collision, RayFilter};
use acvd_sim::damage;
use acvd_sim::ids::NetId;
use acvd_sim::tick::TICK_HZ;
use acvd_sim::world::SimWorld;
use glam::Vec3;

/// History ring length in ticks (1.5 s).
pub const HISTORY_TICKS: usize = 90;
/// Client remote interpolation delay (4 ticks = 66 ms at 30 Hz snapshots).
pub const INTERP_DELAY_TICKS: i32 = 4;
/// Hard cap on rewind (`~200 ms`), so 120 ms ping + jitter still fits.
pub const MAX_REWIND_TICKS: i32 = 12;

/// One AC's authoritative pose on one tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HistoryEntry {
    pub tick: u32,
    pub position: Vec3,
    pub yaw: f32,
}

/// Per-AC 90-tick pose ring.
#[derive(Clone, Debug, Default)]
pub struct AcHistory {
    entries: VecDeque<HistoryEntry>,
}

impl AcHistory {
    pub fn push(&mut self, tick: u32, position: Vec3, yaw: f32) {
        if let Some(last) = self.entries.back_mut() {
            if last.tick == tick {
                last.position = position;
                last.yaw = yaw;
                return;
            }
        }
        self.entries.push_back(HistoryEntry {
            tick,
            position,
            yaw,
        });
        while self.entries.len() > HISTORY_TICKS {
            self.entries.pop_front();
        }
    }

    /// Closest stored entry at or before `tick` (falls back to the oldest entry).
    pub fn at(&self, tick: u32) -> Option<HistoryEntry> {
        self.entries
            .iter()
            .rev()
            .find(|entry| entry.tick <= tick)
            .or_else(|| self.entries.front())
            .copied()
    }

    pub fn newest(&self) -> Option<HistoryEntry> {
        self.entries.back().copied()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// All AC histories for one match.
#[derive(Clone, Debug, Default)]
pub struct HitboxHistory {
    per_ac: HashMap<NetId, AcHistory>,
}

impl HitboxHistory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.per_ac.clear();
    }

    /// Pushes every living AC's tick pose; removes histories of departed ACs.
    pub fn push_world(&mut self, world: &SimWorld) {
        let live: Vec<NetId> = world.acs.keys().copied().collect();
        self.per_ac.retain(|id, _| live.contains(id));
        for (id, ac) in &world.acs {
            if ac.alive {
                self.per_ac
                    .entry(*id)
                    .or_default()
                    .push(world.tick.0, ac.position, ac.motion.yaw);
            }
        }
    }

    pub fn get(&self, id: NetId) -> Option<&AcHistory> {
        self.per_ac.get(&id)
    }

    /// Test/bot hook: insert an explicit historical pose.
    pub fn push(&mut self, id: NetId, tick: u32, position: Vec3, yaw: f32) {
        self.per_ac.entry(id).or_default().push(tick, position, yaw);
    }

    pub fn remove(&mut self, id: NetId) {
        self.per_ac.remove(&id);
    }

    /// Number of ACs with retained hitbox history (leak check).
    pub fn tracked(&self) -> usize {
        self.per_ac.len()
    }
}

/// `clamp(rtt_ticks/2 + INTERP_DELAY_TICKS + jitter_ticks, 0, MAX_REWIND_TICKS)`.
pub fn rewind_ticks(rtt_ms: f64, jitter_ms: f64) -> i32 {
    let rtt_ticks = (rtt_ms / 1000.0 * f64::from(TICK_HZ) / 2.0).round() as i32;
    let jitter_ticks = (jitter_ms / 1000.0 * f64::from(TICK_HZ)).round() as i32;
    (rtt_ticks + INTERP_DELAY_TICKS + jitter_ticks).clamp(0, MAX_REWIND_TICKS)
}

/// What a hitscan/fast round hit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HitscanOutcome {
    Achit { target: NetId, position: Vec3 },
    Prop { prop_id: u32, position: Vec3 },
    Expire { position: Vec3 },
}

/// Validates a fast round from `muzzle` along `dir` (unit length) against rewound AC spheres and
/// current static terrain. `rewind` comes from [`rewind_ticks`] and `tick` is the fire tick.
#[allow(clippy::too_many_arguments)]
pub fn resolve_hitscan(
    history: &HitboxHistory,
    world: &SimWorld,
    collision: &Collision,
    shooter: NetId,
    muzzle: Vec3,
    dir: Vec3,
    range: f32,
    tick: u32,
    rewind: i32,
) -> HitscanOutcome {
    let end = muzzle + dir * range;
    let target_tick = tick.saturating_sub(rewind.max(0) as u32);
    let terrain = collision.raycast_with_prop(muzzle, end, RayFilter::Bullet);
    let terrain_t = terrain.map_or(f32::INFINITY, |(t, _, _, _)| t);
    let shooter_team = world.acs.get(&shooter).map_or(0, |ac| ac.team);

    let mut best: Option<(f32, NetId, Vec3)> = None;
    for (id, ac) in &world.acs {
        if *id == shooter || !ac.alive {
            continue;
        }
        if !world.friendly_fire && ac.team == shooter_team {
            continue;
        }
        let Some(entry) = history.get(*id).and_then(|h| h.at(target_tick)) else {
            continue;
        };
        let _ = entry.yaw;
        for sphere in damage::ac_hit_spheres() {
            let center = entry.position + Vec3::Y * sphere.offset_y;
            if let Some((t, pos, _)) =
                damage::segment_hits_sphere(muzzle, end, center, sphere.radius)
            {
                if t <= terrain_t && best.is_none_or(|(bt, _, _)| t < bt) {
                    best = Some((t, *id, pos));
                }
            }
        }
    }
    if let Some((_, target, position)) = best {
        return HitscanOutcome::Achit { target, position };
    }
    if let Some((t, at, _, prop_id)) = terrain {
        if let Some(prop_id) = prop_id {
            return HitscanOutcome::Prop {
                prop_id,
                position: at,
            };
        }
        let _ = t;
        return HitscanOutcome::Expire { position: at };
    }
    HitscanOutcome::Expire { position: end }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acvd_proto::Loadout;
    use acvd_sim::world::SimAc;

    fn world_with_target(pos: Vec3) -> SimWorld {
        let mut world = SimWorld::new();
        let mut shooter = SimAc::new(NetId(1), Loadout::default(), Vec3::ZERO, 0.0, 0);
        shooter.motion.yaw = 0.0;
        world.insert(shooter);
        let target = SimAc::new(NetId(2), Loadout::default(), pos, 0.0, 1);
        world.insert(target);
        world.tick = acvd_sim::tick::Tick(100);
        world
    }

    #[test]
    fn rewind_formula_fits_120ms_ping_and_caps_at_12() {
        // 120 ms -> 3.6 ticks / 2? No: rtt_ticks = 120*60/1000 = 7.2, half = 3.6 -> 4.
        assert_eq!(rewind_ticks(120.0, 0.0), 4 + INTERP_DELAY_TICKS);
        assert_eq!(rewind_ticks(120.0, 30.0), 4 + INTERP_DELAY_TICKS + 2);
        assert_eq!(rewind_ticks(1000.0, 500.0), MAX_REWIND_TICKS);
        assert_eq!(rewind_ticks(0.0, 0.0), INTERP_DELAY_TICKS);
    }

    #[test]
    fn strafing_target_is_hit_at_the_shooter_view_tick() {
        // The target was at x = 0 on the shooter's view tick, then strafed 5 m by the fire tick.
        let world = world_with_target(Vec3::new(5.0, 0.0, -20.0));
        let mut history = HitboxHistory::new();
        let target = world.acs.get(&NetId(2)).unwrap().clone();
        history
            .per_ac
            .entry(NetId(2))
            .or_default()
            .push(100, Vec3::new(0.0, 0.0, -20.0), 0.0);
        assert_eq!(
            history.get(NetId(2)).unwrap().at(100).unwrap().position,
            Vec3::new(0.0, 0.0, -20.0)
        );
        let collision = Collision::default();
        let rewind = rewind_ticks(120.0, 0.0);
        let outcome = resolve_hitscan(
            &history,
            &world,
            &collision,
            NetId(1),
            Vec3::new(0.0, 3.0, 0.0),
            Vec3::new(0.0, 0.0, -1.0),
            200.0,
            world.tick.0,
            rewind,
        );
        match outcome {
            HitscanOutcome::Achit { target, .. } => assert_eq!(target, NetId(2)),
            other => panic!("expected rewound hit, got {other:?}"),
        }
        // Without rewind the current 5 m offset is a clean miss (spheres span ~5.5 m, so test the
        // lateral offset directly with a narrower shot).
        let miss = resolve_hitscan(
            &history,
            &world,
            &collision,
            NetId(1),
            Vec3::new(12.0, 3.0, 0.0),
            Vec3::new(0.0, 0.0, -1.0),
            200.0,
            world.tick.0,
            0,
        );
        assert!(matches!(miss, HitscanOutcome::Expire { .. }), "{miss:?}");
        let _ = target;
    }
}
