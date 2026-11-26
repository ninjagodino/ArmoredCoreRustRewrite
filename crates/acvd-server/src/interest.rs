//! Interest management: a uniform XZ grid, three distance tiers, and per-snapshot cadences.
//!
//! Tier 0 (near) is sent every snapshot (30 Hz), tier 1 (mid) every third (10 Hz), tier 2 (far)
//! every fifteenth (2 Hz), and ACs beyond 800 m are culled. A client-locked target, a teammate
//! within 250 m, and an AC damaged in the last 3 s are promoted to tier 0.

use std::collections::HashMap;

use acvd_sim::ids::NetId;
use acvd_sim::tick::Tick;
use acvd_sim::world::SimWorld;
use glam::Vec2;

use crate::config::ServerConfig;

/// Full-record distance for tier 0.
pub const NEAR_RANGE: f32 = 150.0;
/// Full-record distance for tier 1.
pub const MID_RANGE: f32 = 400.0;
/// Culling distance (and reduced-record tier 2 range).
pub const FAR_RANGE: f32 = 800.0;
/// Teammates inside this range are always tier 0.
pub const TEAM_NEAR_RANGE: f32 = 250.0;
/// Ticks an AC stays tier 0 after taking damage (3 s at 60 Hz).
pub const DAMAGE_NEAR_TICKS: u32 = 180;

/// Uniform XZ bucket grid over AC positions.
#[derive(Clone, Debug)]
pub struct SpatialGrid {
    cell: f32,
    cells: HashMap<(i32, i32), Vec<NetId>>,
}

impl Default for SpatialGrid {
    fn default() -> Self {
        Self {
            cell: FAR_RANGE,
            cells: HashMap::new(),
        }
    }
}

impl SpatialGrid {
    pub fn rebuild(&mut self, world: &SimWorld) {
        self.cells.clear();
        for ac in world.acs.values() {
            let key = self.key(ac.position.x, ac.position.z);
            self.cells.entry(key).or_default().push(ac.id);
        }
    }

    fn key(&self, x: f32, z: f32) -> (i32, i32) {
        (
            (x / self.cell).floor() as i32,
            (z / self.cell).floor() as i32,
        )
    }

    /// Every AC in buckets that can contain a point within `radius` of the position.
    pub fn nearby(&self, position: Vec2, radius: f32) -> Vec<NetId> {
        let mut out = Vec::new();
        let r = (radius / self.cell).ceil() as i32;
        let (cx, cz) = self.key(position.x, position.y);
        for x in cx - r..=cx + r {
            for z in cz - r..=cz + r {
                if let Some(ids) = self.cells.get(&(x, z)) {
                    out.extend_from_slice(ids);
                }
            }
        }
        out
    }
}

/// Snapshot tier of one AC for one viewer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Near,
    Mid,
    Far,
}

impl Tier {
    /// Send every Nth snapshot (`Near` = every snapshot).
    pub fn cadence(self) -> u32 {
        match self {
            Tier::Near => 1,
            Tier::Mid => 3,
            Tier::Far => 15,
        }
    }

    pub fn should_send(self, snapshot_seq: u16) -> bool {
        u32::from(snapshot_seq) % self.cadence() == 0
    }
}

/// Per-snapshot interest decisions.
#[derive(Clone, Debug, Default)]
pub struct InterestManager {
    grid: SpatialGrid,
    last_rebuild: Option<Tick>,
}

impl InterestManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuilds the grid once per snapshot tick.
    pub fn rebuild(&mut self, world: &SimWorld) {
        if self.last_rebuild != Some(world.tick) {
            self.grid.rebuild(world);
            self.last_rebuild = Some(world.tick);
        }
    }

    /// Visible ACs for `viewer`, ordered near -> mid -> far, then by distance.
    ///
    /// `lock_target` is the client's input hint only (never trusted for damage). `self_record`
    /// controls whether the viewer's own AC is included (late-join full sync includes it).
    pub fn visible(
        &self,
        world: &SimWorld,
        viewer: NetId,
        lock_target: NetId,
        include_self: bool,
    ) -> Vec<(NetId, Tier)> {
        let Some(view) = world.acs.get(&viewer) else {
            return Vec::new();
        };
        let viewer_pos = Vec2::new(view.position.x, view.position.z);
        let mut out: Vec<(NetId, Tier, f32)> = Vec::new();
        let mut candidates = self.grid.nearby(viewer_pos, FAR_RANGE);
        if lock_target != NetId(0) && !candidates.contains(&lock_target) {
            // A client-locked target is always tier 0 and visible.
            candidates.push(lock_target);
        }
        for id in candidates {
            let Some(ac) = world.acs.get(&id) else {
                continue;
            };
            if ac.id == viewer && !include_self {
                continue;
            }
            let d = Vec2::new(ac.position.x, ac.position.z).distance(viewer_pos);
            let recently_hit = ac
                .last_hit_tick
                .is_some_and(|tick| world.tick.since(tick) <= DAMAGE_NEAR_TICKS);
            let tier = if d <= NEAR_RANGE
                || ac.id == lock_target
                || (ac.team == view.team && d <= TEAM_NEAR_RANGE)
                || recently_hit
            {
                Tier::Near
            } else if d <= MID_RANGE {
                Tier::Mid
            } else if d <= FAR_RANGE {
                Tier::Far
            } else {
                continue;
            };
            out.push((ac.id, tier, d));
        }
        out.sort_by(|a, b| {
            a.1.cadence()
                .cmp(&b.1.cadence())
                .then(a.2.total_cmp(&b.2))
                .then(a.0.cmp(&b.0))
        });
        out.into_iter().map(|(id, tier, _)| (id, tier)).collect()
    }

    /// Candidate count within `radius` of `position` (grid sanity checks/tests).
    pub fn candidates(&self, position: Vec2, radius: f32) -> usize {
        self.grid.nearby(position, radius).len()
    }
}

/// The distance tier for one viewer/AC pair, ignoring the grid. Used by tests and the snapshot
/// builder to keep the policy in one place.
pub fn tier_for(
    world: &SimWorld,
    viewer: NetId,
    target: NetId,
    lock_target: NetId,
) -> Option<Tier> {
    let view = world.acs.get(&viewer)?;
    let ac = world.acs.get(&target)?;
    let d = Vec2::new(
        ac.position.x - view.position.x,
        ac.position.z - view.position.z,
    )
    .length();
    let recently_hit = ac
        .last_hit_tick
        .is_some_and(|tick| world.tick.since(tick) <= DAMAGE_NEAR_TICKS);
    if d <= NEAR_RANGE
        || target == lock_target
        || (ac.team == view.team && d <= TEAM_NEAR_RANGE)
        || recently_hit
    {
        Some(Tier::Near)
    } else if d <= MID_RANGE {
        Some(Tier::Mid)
    } else if d <= FAR_RANGE {
        Some(Tier::Far)
    } else {
        None
    }
}

/// Default spawn offset for player `net_id` in the synthetic test arena.
pub fn arena_spawn(team: u8, net_id: NetId) -> glam::Vec3 {
    let lane = f32::from(net_id.0) - 1.0;
    let side = if team == 0 { -1.0 } else { 1.0 };
    glam::Vec3::new(side * (2.0 + lane * 1.5), 0.0, 8.0 + lane * 2.0)
}

/// Expands a map AABB so the quantizer never clips an AC at the edge.
pub fn padded_aabb(min: glam::Vec3, max: glam::Vec3) -> (glam::Vec3, glam::Vec3) {
    let span = (max - min).max(glam::Vec3::splat(100.0));
    let pad = span * 0.1 + glam::Vec3::splat(20.0);
    (min - pad, max + pad)
}

/// Quantizer configuration from `ServerConfig` (tests keep the default).
pub fn quantizer_for(config: &ServerConfig) -> acvd_proto::PosQuantizer {
    config.quantizer
}

#[cfg(test)]
mod tests {
    use super::*;
    use acvd_proto::Loadout;
    use acvd_sim::world::SimAc;
    use glam::Vec3;

    fn world_with(positions: &[(u8, Vec3, u8)]) -> SimWorld {
        let mut world = SimWorld::new();
        for (i, (id, pos, team)) in positions.iter().enumerate() {
            let mut ac = SimAc::new(NetId(*id), Loadout::default(), *pos, 0.0, *team);
            ac.pose = 0;
            world.insert(ac);
            world.tick = Tick(i as u32);
        }
        world
    }

    #[test]
    fn distance_tiers_and_cull() {
        let world = world_with(&[
            (1, Vec3::ZERO, 0),
            (2, Vec3::new(100.0, 0.0, 0.0), 1),
            (3, Vec3::new(300.0, 0.0, 0.0), 1),
            (4, Vec3::new(600.0, 0.0, 0.0), 1),
            (5, Vec3::new(900.0, 0.0, 0.0), 1),
        ]);
        assert_eq!(
            tier_for(&world, NetId(1), NetId(2), NetId(0)),
            Some(Tier::Near)
        );
        assert_eq!(
            tier_for(&world, NetId(1), NetId(3), NetId(0)),
            Some(Tier::Mid)
        );
        assert_eq!(
            tier_for(&world, NetId(1), NetId(4), NetId(0)),
            Some(Tier::Far)
        );
        assert_eq!(tier_for(&world, NetId(1), NetId(5), NetId(0)), None);
    }

    #[test]
    fn lock_and_team_promote_to_near() {
        let world = world_with(&[
            (1, Vec3::ZERO, 0),
            (2, Vec3::new(200.0, 0.0, 0.0), 1),
            (3, Vec3::new(200.0, 0.0, 0.0), 0),
        ]);
        assert_eq!(
            tier_for(&world, NetId(1), NetId(2), NetId(2)),
            Some(Tier::Near)
        );
        assert_eq!(
            tier_for(&world, NetId(1), NetId(3), NetId(0)),
            Some(Tier::Near)
        );
    }

    #[test]
    fn grid_returns_nearby_candidates() {
        let mut world = world_with(&[
            (1, Vec3::ZERO, 0),
            (2, Vec3::new(100.0, 0.0, 0.0), 1),
            (3, Vec3::new(700.0, 0.0, 0.0), 1),
        ]);
        world.tick = Tick(5);
        let mut interest = InterestManager::new();
        interest.rebuild(&world);
        assert!(interest.candidates(Vec2::ZERO, 100.0) >= 1);
        let visible = interest.visible(&world, NetId(1), NetId(0), false);
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].1, Tier::Near);
    }

    #[test]
    fn cadence_is_30_10_2_hz() {
        assert!(Tier::Near.should_send(1));
        assert!(!Tier::Mid.should_send(1));
        assert!(Tier::Mid.should_send(3));
        assert!(!Tier::Far.should_send(3));
        assert!(Tier::Far.should_send(30));
    }
}
