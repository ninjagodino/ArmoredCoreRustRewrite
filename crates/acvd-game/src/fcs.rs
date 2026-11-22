//! Fire Control System (FCS) lock-on and lead-point prediction (`AcFcs`, `FcsManager`).
//!
//! Matches the 360 lock-on pipeline:
//! - **Lock range**: `AcCtrlParam::first_lock_range` (Lua param 74 = `firstLockRangeRate * fcs.lock_range`,
//!   from head `camera_ability` `+0x140` `0x82479188` and FCS `lock_range` `+0x13c` `0x8247a078`),
//!   copied into the frustum spec `+0x10` by `0x828577b0`.
//! - **Elliptical lock-sight cone** (`0x82858178` → `0x828577b0` → `0x829d8d58` → `0x829d8850` →
//!   `0x829d8340` / `0x829dd5b8`): datum sits `fcs_datum_back` (`20.0 m`, `Assemble.bin +0x328`)
//!   behind the AC along the aim direction. A target within `first_lock_range` of the AC with
//!   datum-local `z > 0` lies inside the inner cone when
//!   `(x / (z * tan(sight_x)))^2 + (y / (z * tan(sight_y)))^2 <= 1.0`. An outer hysteresis cone
//!   adds `2.0°` (`DAT_820b005c` in `0x829d8758`) to keep an existing lock from flickering at the
//!   edge. Terrain line-of-sight occlusion (`LockCheckRay` `0x829c5f20` → `0x829c5e30`, filter
//!   `0x80100006` + `0x829c5c00` `AcObjectParam.camera_collidable`) is raycast against the map
//!   collision BVH (`Collision::has_los(..., RayFilter::FcsLos)`).
//! - **Secondary lock-on timer** (`0x82857688` → `0x829debe8` → `0x829de560`): per hand,
//!   `lock_time = weapon.missile_lock_time * AcCtrlParam::lock_effective_rate / 60.0` seconds.
//!   Once `elapsed >= lock_time`, secondary lock is complete.
//! - **Lead-point prediction** (`0x8287b070` → `0x829c2ee8` → `0x829c2808`):
//!   - Secondary lock complete: aims at `target_pos + target_vel * t_flight`, solving time-of-flight
//!     from `muzzle` at `gun.init_speed`.
//!   - Primary lock only: aims at the current `target_pos` without velocity lead (`0x8287b158`).
//!   - No lock: falls back to `Pilot::aim_direction()`.

use std::collections::HashMap;

use acvd_data::generated::ctrl::AcCtrlParam;
use bevy::prelude::*;

use crate::collision::{Collision, RayFilter};
use crate::control::{Pilot, Piloting};
use crate::targets::{Target, TargetHealth};
use crate::weapons::{Armament, Kind};

/// Outer keep-lock hysteresis margin added to `sight_x`/`sight_y` (`DAT_820b005c` = `2.0` deg in
/// `0x829d8758`, tested as the outer cone in `0x829d8850`).
const OUTER_MARGIN_DEG: f32 = 2.0;
/// Game tick rate for `missile_lock_time` (`DAT_82004408` = `1/60` in `0x82857688`).
const TICK_RATE: f32 = 60.0;
/// Maximum lead-prediction time of flight in seconds (matches projectile `MAX_LIFE`).
const MAX_LEAD_TIME: f32 = 5.0;
/// Approximate AC waist height above feet (used as the AC-side lock origin).
const AC_WAIST_Y: f32 = 4.5;

/// Static FCS parameters computed from `AcCtrlParam` (`sheets/ac_ctrl_calc.csv`).
#[derive(Clone, Copy, Debug)]
pub struct FcsSpec {
    /// Maximum primary/secondary lock range in metres (`AcCtrlParam::first_lock_range`, param 74).
    pub first_lock_range: f32,
    /// Secondary lock-time multiplier (`AcCtrlParam::lock_effective_rate`, param 80).
    pub lock_effective_rate: f32,
    /// Missile lock-time multiplier (`AcCtrlParam::missile_lock_effective_rate`, param 81).
    pub missile_lock_effective_rate: f32,
    /// FCS maximum simultaneous missile locks (`fcs.missile_lock_max`, `0x8247a1a0`, 1..=6).
    pub missile_lock_max: u8,
    /// Horizontal lock-sight half-angle in radians (`fcs.lock_sight_x`).
    pub sight_x_rad: f32,
    /// Vertical lock-sight half-angle in radians (`fcs.lock_sight_y`).
    pub sight_y_rad: f32,
    /// Backward datum offset behind the AC in metres (`Assemble.bin +0x328` = `20.0`).
    pub datum_back: f32,
}

impl FcsSpec {
    pub fn from_ctrl(ctrl: &AcCtrlParam) -> Self {
        Self {
            first_lock_range: ctrl.first_lock_range.max(1.0),
            lock_effective_rate: ctrl.lock_effective_rate.max(0.05),
            missile_lock_effective_rate: ctrl.missile_lock_effective_rate.max(0.05),
            missile_lock_max: ctrl.missile_lock_max.round().clamp(1.0, 6.0) as u8,
            sight_x_rad: ctrl.lock_sight_x_rad.clamp(0.02, std::f32::consts::FRAC_PI_2 - 0.01),
            sight_y_rad: ctrl.lock_sight_y_rad.clamp(0.02, std::f32::consts::FRAC_PI_2 - 0.01),
            datum_back: ctrl.fcs_datum_back.max(0.0),
        }
    }
}

/// Active primary/secondary lock state on a single target.
#[derive(Clone, Debug)]
pub struct LockedTarget {
    pub entity: Entity,
    #[allow(dead_code)]
    pub name: String,
    pub model: String,
    /// World-space center of the locked target's primary hit sphere.
    pub world_pos: Vec3,
    /// Estimated target velocity in m/s.
    pub velocity: Vec3,
    /// World distance from the AC to `world_pos` in metres.
    #[allow(dead_code)]
    pub distance: f32,
    pub ap: f32,
    #[allow(dead_code)]
    pub ap_max: f32,
    /// Elapsed secondary lock time in seconds for `[right, left]` hand weapons (`0x829debe8`).
    pub elapsed: [f32; 2],
    /// Required secondary lock time in seconds for `[right, left]` hand weapons (`0x82857688`).
    pub lock_time: [f32; 2],
    /// True once `elapsed[hand] >= lock_time[hand]` (`0x829de560`).
    pub secondary: [bool; 2],
}

impl LockedTarget {
    /// True if either hand weapon has completed secondary lock-on.
    pub fn any_secondary(&self) -> bool {
        self.secondary[0] || self.secondary[1]
    }

    /// Lock progress in `0.0..=1.0` for `hand_idx` (`0` right, `1` left).
    pub fn progress(&self, hand_idx: usize) -> f32 {
        let t = self.lock_time[hand_idx];
        if t <= 0.0 {
            1.0
        } else {
            (self.elapsed[hand_idx] / t).clamp(0.0, 1.0)
        }
    }
}

/// Per-AC FCS runtime state (`AcFcs` component on the piloted AC).
#[derive(Component, Clone, Debug)]
pub struct Fcs {
    pub spec: FcsSpec,
    pub locked: Option<LockedTarget>,
    /// Accumulated multi-target missile lock slots (`0x82858638`, up to 6).
    pub missile_locks: Vec<Entity>,
    /// Elapsed seconds toward the next missile lock slot (`0x82857688`).
    pub missile_lock_timer: f32,
    /// Effective max simultaneous missile locks (`min(fcs.missile_lock_max, weapon_lock_max)`).
    pub missile_lock_max: u8,
    /// Latest world-space center for each missile-locked target (for HUD projection).
    pub missile_target_pos: HashMap<Entity, Vec3>,
}

impl Fcs {
    pub fn from_ctrl(ctrl: &AcCtrlParam) -> Self {
        Self {
            spec: FcsSpec::from_ctrl(ctrl),
            locked: None,
            missile_locks: Vec::new(),
            missile_lock_timer: 0.0,
            missile_lock_max: 0,
            missile_target_pos: HashMap::new(),
        }
    }

    /// Number of active missile locks on `entity` (`0..=6`, `0x829d1a10`).
    #[allow(dead_code)]
    pub fn missile_lock_count(&self, entity: Entity) -> u8 {
        self.missile_locks
            .iter()
            .filter(|&&e| e == entity)
            .count()
            .min(6) as u8
    }

    /// Unique missile-locked targets with their latest world center and lock count (`1..=6`),
    /// preserving first-locked order.
    pub fn missile_targets(&self) -> Vec<(Entity, Vec3, u8)> {
        let mut out: Vec<(Entity, Vec3, u8)> = Vec::new();
        for &entity in &self.missile_locks {
            if let Some(entry) = out.iter_mut().find(|(e, _, _)| *e == entity) {
                entry.2 = entry.2.saturating_add(1).min(6);
            } else if let Some(&pos) = self.missile_target_pos.get(&entity) {
                out.push((entity, pos, 1));
            }
        }
        out
    }

    /// Drains all accumulated missile locks when firing a shoulder salvo and resets the timer.
    pub fn take_missile_locks(&mut self) -> Vec<Entity> {
        self.missile_lock_timer = 0.0;
        std::mem::take(&mut self.missile_locks)
    }

    /// Computes the aim unit vector for hand `hand_idx` (`0` right, `1` left) from `muzzle`
    /// (`0x8287b070`):
    /// - Secondary locked: lead-predicted target position (`0x829c2ee8` → `0x829c2808`).
    /// - Primary locked only: current target position (`0x8287b158`).
    /// - Unlocked: `fallback` (`Pilot::aim_direction()`).
    pub fn aim_for_hand(
        &self,
        hand_idx: usize,
        muzzle: Vec3,
        bullet_speed: f32,
        fallback: Vec3,
    ) -> Vec3 {
        let Some(lock) = self.locked.as_ref() else {
            return fallback;
        };
        let point = if lock.secondary[hand_idx] {
            predict_lead_point(muzzle, lock.world_pos, lock.velocity, bullet_speed)
        } else {
            lock.world_pos
        };
        let dir = point - muzzle;
        if dir.length_squared() > 1.0e-6 {
            dir.normalize()
        } else {
            fallback
        }
    }
}

/// Computes secondary lock-on duration in seconds (`0x82857688`):
/// `weapon.missile_lock_time * AcCtrlParam::lock_effective_rate / 60.0`.
pub fn secondary_lock_time(lock_time_ticks: f32, lock_effective_rate: f32) -> f32 {
    if lock_time_ticks <= 0.0 {
        return 0.0;
    }
    (lock_time_ticks * lock_effective_rate / TICK_RATE).max(0.0)
}

/// Computes per-missile lock-on duration in seconds (`0x82857688` + `AcCtrlParam::missile_lock_effective_rate`,
/// Lua param 81): `weapon.missile_lock_time * missile_lock_effective_rate / 60.0`.
pub fn missile_lock_time(lock_time_ticks: f32, missile_lock_effective_rate: f32) -> f32 {
    if lock_time_ticks <= 0.0 {
        return 0.05;
    }
    (lock_time_ticks * missile_lock_effective_rate / TICK_RATE).max(0.05)
}

/// Pure multi-target missile lock accumulator (`0x82858638` + `0x82857688`):
/// - Prunes any existing locks whose target left the outer keep-lock cone (`valid_targets`).
/// - While `locks.len() < max_locks`, advances `timer` by `dt` and assigns each new lock
///   to the inner-cone candidate with the fewest existing locks (tie-breaking by smallest `ell`).
pub fn step_missile_locks(
    locks: &mut Vec<Entity>,
    timer: &mut f32,
    inner_candidates: &[(Entity, f32)],
    valid_targets: &HashMap<Entity, Vec3>,
    max_locks: u8,
    step_time: f32,
    can_acquire: bool,
    dt: f32,
) {
    locks.retain(|e| valid_targets.contains_key(e));
    let cap = (max_locks.min(6)) as usize;
    if locks.len() > cap {
        locks.truncate(cap);
    }
    if !can_acquire || cap == 0 || inner_candidates.is_empty() || locks.len() >= cap {
        *timer = 0.0;
        return;
    }
    let step = step_time.max(0.05);
    *timer += dt;
    while *timer >= step && locks.len() < cap {
        *timer -= step;
        // Spread locks evenly across targets in the cone, breaking ties by closest to sight center.
        let pick = inner_candidates
            .iter()
            .min_by(|&&(ea, ella), &&(eb, ellb)| {
                let ca = locks.iter().filter(|&&x| x == ea).count();
                let cb = locks.iter().filter(|&&x| x == eb).count();
                ca.cmp(&cb).then_with(|| ella.total_cmp(&ellb))
            })
            .map(|&(e, _)| e);
        if let Some(target) = pick {
            locks.push(target);
        } else {
            break;
        }
    }
    if locks.len() >= cap {
        *timer = 0.0;
    }
}

/// Tests whether `target_pos` is within `range` metres of `ac_pos` and inside the elliptical
/// lock-sight cone (`0x829d8d58` → `0x829d8850` → `0x829d8340` / `0x829dd5b8`).
///
/// Returns `Some(ell)` where `ell = (x / max_x)^2 + (y / max_y)^2` in `0.0..=1.0` (0 = dead
/// center of sight, 1 = on the ellipse boundary), or `None` if outside range or cone.
pub fn in_lock_cone(
    ac_pos: Vec3,
    aim_dir: Vec3,
    target_pos: Vec3,
    range: f32,
    half_x_rad: f32,
    half_y_rad: f32,
    datum_back: f32,
) -> Option<f32> {
    let to_target = target_pos - ac_pos;
    if to_target.length_squared() > range * range {
        return None;
    }
    let forward = aim_dir.normalize_or_zero();
    if forward == Vec3::ZERO {
        return None;
    }
    // Build orthonormal basis around `forward` (up = world +Y, fallback +Z near poles).
    let up_hint = if forward.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let right = forward.cross(up_hint).normalize();
    let up = right.cross(forward);

    let datum = ac_pos - forward * datum_back;
    let rel = target_pos - datum;
    let z = rel.dot(forward);
    if z <= 0.0 {
        return None;
    }
    let x = rel.dot(right);
    let y = rel.dot(up);
    let max_x = z * half_x_rad.tan();
    let max_y = z * half_y_rad.tan();
    if max_x <= 0.0 || max_y <= 0.0 {
        return None;
    }
    if x.abs() > max_x || y.abs() > max_y {
        return None;
    }
    let nx = x / max_x;
    let ny = y / max_y;
    let ell = nx * nx + ny * ny;
    (ell <= 1.0).then_some(ell)
}

/// Linear lead-point prediction (`0x829c2ee8` → `0x829c2808`):
/// solves `|target_pos + target_vel * t - muzzle| = bullet_speed * t` for `t >= 0` (clamped to
/// `MAX_LEAD_TIME`) and returns `target_pos + target_vel * t`.
pub fn predict_lead_point(
    muzzle: Vec3,
    target_pos: Vec3,
    target_vel: Vec3,
    bullet_speed: f32,
) -> Vec3 {
    if bullet_speed <= 1.0e-3 || target_vel.length_squared() <= 1.0e-6 {
        return target_pos;
    }
    let d = target_pos - muzzle;
    let a = target_vel.length_squared() - bullet_speed * bullet_speed;
    let b = 2.0 * d.dot(target_vel);
    let c = d.length_squared();
    let t = if a.abs() < 1.0e-5 {
        if b.abs() > 1.0e-5 {
            -c / b
        } else {
            d.length() / bullet_speed
        }
    } else {
        let disc = b * b - 4.0 * a * c;
        if disc < 0.0 {
            d.length() / bullet_speed
        } else {
            let s = disc.sqrt();
            let t1 = (-b - s) / (2.0 * a);
            let t2 = (-b + s) / (2.0 * a);
            match (t1 >= 0.0, t2 >= 0.0) {
                (true, true) => t1.min(t2),
                (true, false) => t1,
                (false, true) => t2,
                (false, false) => d.length() / bullet_speed,
            }
        }
    };
    target_pos + target_vel * t.clamp(0.0, MAX_LEAD_TIME)
}

/// Terrain line-of-sight test for FCS lock-on (`LockCheckRay` `0x829c5f20` mode 1, filter
/// `0x80100006` + `0x829c5c00` `camera_collidable`).
pub fn target_has_los(collision: &Collision, ac_pos: Vec3, target_pos: Vec3) -> bool {
    collision.has_los(ac_pos, target_pos, RayFilter::FcsLos)
}

/// Per-frame FCS target selection and secondary lock-on timer update.
pub fn update(
    time: Res<Time>,
    piloting: Res<Piloting>,
    collision: Res<Collision>,
    mut prev_pos: Local<HashMap<Entity, Vec3>>,
    mut acs: Query<(&Pilot, &Armament, &Transform, &mut Fcs)>,
    targets: Query<(Entity, &Target, &TargetHealth, &Transform)>,
) {
    if !piloting.0 {
        return;
    }
    let dt = time.delta_secs();
    let Ok((pilot, arms, ac_tf, mut fcs)) = acs.single_mut() else {
        return;
    };
    if pilot.scan_mode {
        fcs.locked = None;
        fcs.missile_locks.clear();
        fcs.missile_lock_timer = 0.0;
        fcs.missile_target_pos.clear();
        return;
    }

    let ac_pos = ac_tf.translation + Vec3::Y * AC_WAIST_Y;
    let aim_dir = pilot.aim_direction();
    let spec = fcs.spec;
    let outer_x = (spec.sight_x_rad + OUTER_MARGIN_DEG.to_radians())
        .min(std::f32::consts::FRAC_PI_2 - 0.01);
    let outer_y = (spec.sight_y_rad + OUTER_MARGIN_DEG.to_radians())
        .min(std::f32::consts::FRAC_PI_2 - 0.01);
    let cur_entity = fcs.locked.as_ref().map(|l| l.entity);

    // Track target velocities and collect all candidates in the inner/outer lock-sight cones.
    let mut best: Option<(f32, Entity, String, String, Vec3, Vec3, f32, f32, f32)> = None;
    let mut inner_candidates: Vec<(Entity, f32)> = Vec::new();
    let mut valid_targets: HashMap<Entity, Vec3> = HashMap::new();
    for (entity, target, health, tf) in &targets {
        let center = tf.translation + Vec3::Y * health.center_offset_y();
        let vel = prev_pos
            .get(&entity)
            .filter(|_| dt > 1.0e-5)
            .map_or(Vec3::ZERO, |&p| (center - p) / dt);
        prev_pos.insert(entity, center);

        if health.destroyed {
            continue;
        }
        let inner = in_lock_cone(
            ac_pos,
            aim_dir,
            center,
            spec.first_lock_range,
            spec.sight_x_rad,
            spec.sight_y_rad,
            spec.datum_back,
        );
        let in_missile_locks = fcs.missile_locks.contains(&entity);
        let score = if let Some(ell) = inner {
            if !target_has_los(&collision, ac_pos, center) {
                continue;
            }
            inner_candidates.push((entity, ell));
            valid_targets.insert(entity, center);
            // Slight bias toward keeping the current locked target to prevent edge flicker.
            if Some(entity) == cur_entity {
                ell * 0.85
            } else {
                ell
            }
        } else if Some(entity) == cur_entity || in_missile_locks {
            // Outer +2 deg hysteresis cone (`0x829d8758` / `0x829d8850`).
            match in_lock_cone(
                ac_pos,
                aim_dir,
                center,
                spec.first_lock_range,
                outer_x,
                outer_y,
                spec.datum_back,
            ) {
                Some(ell) => {
                    if !target_has_los(&collision, ac_pos, center) {
                        continue;
                    }
                    valid_targets.insert(entity, center);
                    if Some(entity) == cur_entity {
                        1.0 + ell
                    } else {
                        continue;
                    }
                }
                None => continue,
            }
        } else {
            continue;
        };

        let dist = (center - ac_pos).length();
        if best.as_ref().is_none_or(|b| score < b.0) {
            best = Some((
                score,
                entity,
                target.name.clone(),
                target.model.clone(),
                center,
                vel,
                dist,
                health.ap,
                health.ap_max,
            ));
        }
    }

    // Multi-target missile lock accumulation (`0x82858638` + `0x82857688`).
    let sh = &arms.shoulder;
    let effective_max = if sh.is_missile && sh.remaining > 0 {
        spec.missile_lock_max
            .min(sh.weapon_lock_max)
            .min(sh.remaining.min(6) as u8)
            .min(6)
    } else {
        0
    };
    fcs.missile_lock_max = effective_max;
    let step_time = missile_lock_time(sh.lock_time_ticks, spec.missile_lock_effective_rate);
    let can_acquire = sh.is_missile && sh.remaining > 0 && sh.cooldown <= 0.0 && arms.salvo.is_empty();
    let mut locks = std::mem::take(&mut fcs.missile_locks);
    let mut timer = fcs.missile_lock_timer;
    step_missile_locks(
        &mut locks,
        &mut timer,
        &inner_candidates,
        &valid_targets,
        effective_max,
        step_time,
        can_acquire,
        dt,
    );
    fcs.missile_target_pos.clear();
    for &e in &locks {
        if let Some(&pos) = valid_targets.get(&e) {
            fcs.missile_target_pos.insert(e, pos);
        }
    }
    fcs.missile_locks = locks;
    fcs.missile_lock_timer = timer;

    let Some((_, entity, name, model, world_pos, velocity, distance, ap, ap_max)) = best else {
        fcs.locked = None;
        return;
    };

    let lock_time = [0, 1].map(|i| {
        let g = &arms.hands[i];
        if g.kind == Kind::Skip {
            0.0
        } else {
            secondary_lock_time(g.lock_time_ticks, spec.lock_effective_rate)
        }
    });

    let mut elapsed = match fcs.locked.as_ref() {
        Some(prev) if prev.entity == entity => prev.elapsed,
        _ => [0.0, 0.0],
    };
    let mut secondary = [false; 2];
    for i in 0..2 {
        if arms.hands[i].kind == Kind::Skip {
            elapsed[i] = 0.0;
            secondary[i] = false;
            continue;
        }
        elapsed[i] = (elapsed[i] + dt).min(lock_time[i]);
        secondary[i] = elapsed[i] >= lock_time[i];
    }

    fcs.locked = Some(LockedTarget {
        entity,
        name,
        model,
        world_pos,
        velocity,
        distance,
        ap,
        ap_max,
        elapsed,
        lock_time,
        secondary,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;

    #[test]
    fn ac_ctrl_calc_fcs_params_match_lua() {
        // Design 5001: verify first_lock_range, lock_effective_rate, lock_sight_x/y, datum_back.
        let d = AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 5001)
            .unwrap();
        let ctrl = AcCtrlParam::calculate(&d.data);
        let spec = FcsSpec::from_ctrl(&ctrl);
        assert!(
            spec.first_lock_range > 100.0 && spec.first_lock_range < 600.0,
            "first_lock_range = {}",
            spec.first_lock_range
        );
        assert!(
            spec.lock_effective_rate > 0.3 && spec.lock_effective_rate < 1.5,
            "lock_effective_rate = {}",
            spec.lock_effective_rate
        );
        assert!((spec.datum_back - 20.0).abs() < 1e-4, "Assemble.bin +0x328 = 20 m");
        assert!(spec.sight_x_rad > 0.1 && spec.sight_y_rad > 0.1);

        // Verify exact Lua formula for FCS 111 + Head 111:
        // Head 111 camera_ability = 554 -> firstLockRangeRate = 1.0 - (500 - 554) / 1000 = 1.054
        // FCS 111 lock_range = 160 -> firstLockRange = 168.64 m
        // FCS 111 lock_calc_speed = 130 -> lockEffectiveRate = 1.0 + (100 - 130) / 200 = 0.85
        let cam = acvd_data::part_field(111, 0, "camera_ability");
        let range = acvd_data::part_field(111, 4, "lock_range");
        let calc = acvd_data::part_field(111, 4, "lock_calc_speed");
        let sx = acvd_data::part_field(111, 4, "lock_sight_x");
        assert_eq!(cam, 554.0);
        assert_eq!(range, 160.0);
        assert_eq!(calc, 130.0);
        assert_eq!(sx, 14.0);
    }

    #[test]
    fn elliptical_cone_respects_range_datum_and_angles() {
        let ac = Vec3::ZERO;
        let aim = Vec3::NEG_Z;
        let half = 14.0_f32.to_radians();
        let datum_back = 20.0;

        // Straight ahead at 100 m (z from datum = 120 m): dead center -> ell == 0.
        let c = in_lock_cone(ac, aim, Vec3::new(0.0, 0.0, -100.0), 160.0, half, half, datum_back);
        assert!(c.is_some_and(|e| e < 1e-5));

        // Out of range (170 m > 160 m): rejected.
        assert!(in_lock_cone(ac, aim, Vec3::new(0.0, 0.0, -170.0), 160.0, half, half, datum_back).is_none());

        // Behind AC (z from datum <= 0): rejected.
        assert!(in_lock_cone(ac, aim, Vec3::new(0.0, 0.0, 25.0), 160.0, half, half, datum_back).is_none());

        // At 100 m ahead, z_datum = 120 m, max_x = 120 * tan(14°) ≈ 29.92 m.
        let max_x = 120.0 * half.tan();
        assert!(in_lock_cone(ac, aim, Vec3::new(max_x * 0.9, 0.0, -100.0), 160.0, half, half, datum_back).is_some());
        assert!(in_lock_cone(ac, aim, Vec3::new(max_x * 1.05, 0.0, -100.0), 160.0, half, half, datum_back).is_none());

        // Diagonal corner (0.8, 0.8) has 0.64 + 0.64 = 1.28 > 1.0 -> outside ellipse even though inside box!
        assert!(
            in_lock_cone(
                ac,
                aim,
                Vec3::new(max_x * 0.8, max_x * 0.8, -100.0),
                160.0,
                half,
                half,
                datum_back
            )
            .is_none(),
            "ellipse rejects box corner (0.8, 0.8)"
        );
    }

    #[test]
    fn lead_prediction_intercepts_moving_target() {
        let muzzle = Vec3::ZERO;
        let target_pos = Vec3::new(0.0, 0.0, -200.0);
        let target_vel = Vec3::new(40.0, 0.0, 0.0);
        let bullet_speed = 400.0;

        let lead = predict_lead_point(muzzle, target_pos, target_vel, bullet_speed);
        // Verify exact intercept: time for bullet to reach `lead` equals time for target to reach `lead`.
        let t_bullet = (lead - muzzle).length() / bullet_speed;
        let expected_target = target_pos + target_vel * t_bullet;
        assert!(
            (lead - expected_target).length() < 1e-3,
            "lead {lead} vs expected {expected_target} (t={t_bullet})"
        );
        assert!(lead.x > 19.0 && lead.x < 21.0, "leads ~20 m ahead in +X");
    }

    #[test]
    fn secondary_lock_time_matches_0x82857688() {
        // Starter rifle 1720 missile_lock_time = 24 ticks (or whatever disc has) * 0.85 / 60.
        let ticks = acvd_data::part_field(1720, 10, "missile_lock_time");
        let t = secondary_lock_time(ticks, 0.85);
        assert!((t - ticks * 0.85 / 60.0).abs() < 1e-6);
    }

    #[test]
    fn multi_lock_missile_accumulation_spreads_and_caps() {
        let e1 = Entity::from_bits(1);
        let e2 = Entity::from_bits(2);
        let e3 = Entity::from_bits(3);
        let inner = vec![(e1, 0.1), (e2, 0.3), (e3, 0.6)];
        let mut valid = HashMap::new();
        valid.insert(e1, Vec3::new(0.0, 0.0, -50.0));
        valid.insert(e2, Vec3::new(10.0, 0.0, -60.0));
        valid.insert(e3, Vec3::new(-10.0, 0.0, -70.0));

        let mut locks = Vec::new();
        let mut timer = 0.0;
        let step = missile_lock_time(18.0, 1.0); // 0.3 s per lock

        // After 0.95 s -> 3 locks, one on each target in center-ness order.
        step_missile_locks(&mut locks, &mut timer, &inner, &valid, 6, step, true, 0.95);
        assert_eq!(locks, vec![e1, e2, e3]);

        // After another 1.0 s -> capped at max_locks = 5 (e1: 2, e2: 2, e3: 1).
        step_missile_locks(&mut locks, &mut timer, &inner, &valid, 5, step, true, 1.0);
        assert_eq!(locks.len(), 5);
        assert_eq!(locks.iter().filter(|&&e| e == e1).count(), 2);
        assert_eq!(locks.iter().filter(|&&e| e == e2).count(), 2);
        assert_eq!(locks.iter().filter(|&&e| e == e3).count(), 1);

        // Removing e2 from valid_targets prunes its locks immediately.
        valid.remove(&e2);
        let inner2 = vec![(e1, 0.1), (e3, 0.6)];
        step_missile_locks(&mut locks, &mut timer, &inner2, &valid, 5, step, true, 0.01);
        assert_eq!(locks.iter().filter(|&&e| e == e2).count(), 0);
        assert_eq!(locks.len(), 3);
    }

    #[test]
    fn lock_check_ray_blocks_fcs_lock_behind_wall() {
        use crate::collision::Hit;
        let wall = Collision::with_hits(
            Vec::new(),
            vec![
                Hit::solid(
                    Vec3::new(-20.0, 0.0, -50.0),
                    Vec3::new(20.0, 0.0, -50.0),
                    Vec3::new(-20.0, 30.0, -50.0),
                ),
                Hit::solid(
                    Vec3::new(20.0, 0.0, -50.0),
                    Vec3::new(20.0, 30.0, -50.0),
                    Vec3::new(-20.0, 30.0, -50.0),
                ),
            ],
        );
        let ac_pos = Vec3::new(0.0, 4.5, 0.0);
        // Target in front of wall (-30 m): clear LOS.
        assert!(target_has_los(&wall, ac_pos, Vec3::new(0.0, 4.5, -30.0)));
        // Target behind wall (-80 m): occluded by `LockCheckRay`.
        assert!(!target_has_los(&wall, ac_pos, Vec3::new(0.0, 4.5, -80.0)));

        // Also test against actual `m4000` geometry if disc is present:
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        if let Ok(disc) = acvd_formats::vfs::Disc::open(&acvd_formats::vfs::default_disc(&root)) {
            let m4000 = crate::collision::load_map(&disc, "m4000").expect("m4000 collision");
            let origin = Vec3::new(324.65, 33.5, 649.5);
            // Straight down through the floor (to y = -10) is occluded by m4000 ground.
            assert!(!target_has_los(&m4000, origin, Vec3::new(324.65, -10.0, 649.5)));
            // Short open-air sightline 5 m above start is clear.
            assert!(target_has_los(&m4000, origin, origin + Vec3::Y * 5.0));
        }
    }
}
