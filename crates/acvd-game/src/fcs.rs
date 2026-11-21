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
//!   edge. Terrain line-of-sight occlusion (`LockCheckRay` `0x829c5f20`) is not raycast yet.
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
    #[allow(dead_code)]
    pub missile_lock_effective_rate: f32,
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
}

impl Fcs {
    pub fn from_ctrl(ctrl: &AcCtrlParam) -> Self {
        Self {
            spec: FcsSpec::from_ctrl(ctrl),
            locked: None,
        }
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

/// Per-frame FCS target selection and secondary lock-on timer update.
pub fn update(
    time: Res<Time>,
    piloting: Res<Piloting>,
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

    let ac_pos = ac_tf.translation + Vec3::Y * AC_WAIST_Y;
    let aim_dir = pilot.aim_direction();
    let spec = fcs.spec;
    let outer_x = (spec.sight_x_rad + OUTER_MARGIN_DEG.to_radians())
        .min(std::f32::consts::FRAC_PI_2 - 0.01);
    let outer_y = (spec.sight_y_rad + OUTER_MARGIN_DEG.to_radians())
        .min(std::f32::consts::FRAC_PI_2 - 0.01);
    let cur_entity = fcs.locked.as_ref().map(|l| l.entity);

    // Track target velocities and find best candidate in the lock-sight cone.
    let mut best: Option<(f32, Entity, String, String, Vec3, Vec3, f32, f32, f32)> = None;
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
        let score = if let Some(ell) = inner {
            // Slight bias toward keeping the current locked target to prevent edge flicker.
            if Some(entity) == cur_entity {
                ell * 0.85
            } else {
                ell
            }
        } else if Some(entity) == cur_entity {
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
                Some(ell) => 1.0 + ell,
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
}
