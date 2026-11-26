//! 360 damage model: KE / CE / TE defense mitigation, impact / stagger, and hit-sphere tests.
//!
//! Traced on `ACV2.pe`:
//! - Weapon damage attribute (`acvparts.bin` category 10 `+0x188` = byte `+0` of the 5-word
//!   damage block copied by `0x8247d4d8` ahead of `damage_power` `+0x18c` and `damage_recoil`
//!   `+0x190`): `0x82874190` switches on `0` = KE, `1` = TE, `6` = CE (`4`/`7` = unmitigated
//!   `low=100, hi=100, thr=0` via `0x827d0928`).
//! - Three-zone defense formula (`0x82874190` → `0x827d0d08` → `0x827d0cc8` → `0x827d0b50`,
//!   reading `NewAcBehavior` `+0x605` = `30%` and `+0x606` = `50%` via `0x827d08b8`):
//!   - `low_dmg = power * low_def * 0.01`, zeroed if `< 1.0` (`0x827d0978`).
//!   - `hi_dmg = power * hi_def * 0.01` (`0x827d09c0`).
//!   - `upper = thr * (1 + mid_damage_width_pct * 0.01)` (`1.30 * thr`).
//!   - `power <= thr`: ineffective / ricochet → `low_dmg`, `effective = false`.
//!   - `power >= upper`: effective / penetration → `hi_dmg`, `effective = true`.
//!   - `thr < power < upper`: transition zone → `lerp(lerp(low_dmg, hi_dmg, mid_damage_min_rate_pct * 0.01), hi_dmg, (power - thr) / (upper - thr))`, `effective = true`.
//! - Impact recoil is scaled by `param/damage.bin` `+0x00` (`recoil_scale` = `1.0`, `0x82984438` at
//!   `0x82984518`). When impact meets or exceeds the target's stagger threshold, the target enters
//!   stagger for `stagger_penalty_time` seconds, lowering its defense thresholds (`0.8×` on ACs per
//!   `AcCtrlParamCalc.lua` lines 221–223; `stagger_penalty_def_thr_*` on enemies in `EnemyBasicSt`).
//! - AC hit shapes (`0x82863b60`, `NewAcBehavior +0x22c/+0x230/+0x234`): lower sphere `r = 3.0 m`
//!   at `y = 3.0 m`, upper sphere `r = 5.5 m` at `y = 6.0 m`. Enemy hit shapes come from
//!   `EnemyGraphicsSt` (`collision_radius`, `hit_upper_hit_sphere_radius`,
//!   `hit_upper_hit_sphere_height_offset`, `one_col_sphere_offset`).

use acvd_data::generated::ac_unit::AcAssemblyDesignSt;
use acvd_data::generated::ctrl::AcCtrlParam;
use acvd_data::generated::enemy::{EnemyBasicSt, EnemyGraphicsSt};
use acvd_data::generated::tuning::DAMAGE;
use glam::Vec3;

/// Damage attribute byte (`0x8247d4d8` `+0x54` / `acvparts.bin` category 10 `+0x188`,
/// switched in `0x82874190`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DamageType {
    /// Kinetic energy (`0` in `0x82874190`).
    Ke,
    /// Thermal energy (`1` in `0x82874190`).
    Te,
    /// Chemical energy (`6` in `0x82874190`).
    Ce,
    /// Unmitigated (`4` / `7` in `0x82874190` → `0x827d0928`: `low=100, hi=100, thr=0`).
    Unmitigated,
}

impl DamageType {
    pub fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::Ke,
            1 => Self::Te,
            6 => Self::Ce,
            _ => Self::Unmitigated,
        }
    }

    /// The `acvparts.bin` attribute byte (`0` KE, `1` TE, `6` CE, `4`/`7` unmitigated).
    pub fn as_raw(self) -> u8 {
        match self {
            Self::Ke => 0,
            Self::Te => 1,
            Self::Ce => 6,
            Self::Unmitigated => 4,
        }
    }
}

/// One attribute's `(low_def, hi_def, threshold)` triple (exact 360 layout in `EnemyBasicSt`
/// and AC `0x82873e70`) plus its staggered defense threshold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DefenseChannel {
    /// Percentage of attack power taken on ricochet (`power <= threshold`, `0x827d0978`).
    pub low_def: u8,
    /// Percentage of attack power taken on full penetration (`power >= 1.3 * threshold`, `0x827d09c0`).
    pub hi_def: u8,
    /// Normal defense threshold (`u16`).
    pub threshold: u16,
    /// Defense threshold while staggered (`0.8 * threshold` on ACs; `stagger_penalty_def_thr_*` on enemies).
    pub stagger_threshold: u16,
}

impl DefenseChannel {
    pub const UNMITIGATED: Self = Self {
        low_def: 100,
        hi_def: 100,
        threshold: 0,
        stagger_threshold: 0,
    };
}

/// Target KE / CE / TE defense profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DefenseProfile {
    pub ke: DefenseChannel,
    pub ce: DefenseChannel,
    pub te: DefenseChannel,
}

impl DefenseProfile {
    /// Builds a defense profile from an enemy's `EnemyBasicSt` row.
    pub fn from_enemy(basic: &EnemyBasicSt) -> Self {
        Self {
            ke: DefenseChannel {
                low_def: basic.low_defense_ke,
                hi_def: basic.hi_defense_ke,
                threshold: basic.defense_threshold_ke,
                stagger_threshold: basic.stagger_penalty_def_thr_ke,
            },
            ce: DefenseChannel {
                low_def: basic.low_defense_ce,
                hi_def: basic.hi_defense_ce,
                threshold: basic.defense_threshold_ce,
                stagger_threshold: basic.stagger_penalty_def_thr_ce,
            },
            te: DefenseChannel {
                low_def: basic.low_defense_te,
                hi_def: basic.hi_defense_te,
                threshold: basic.defense_threshold_te,
                stagger_threshold: basic.stagger_penalty_def_thr_te,
            },
        }
    }

    /// Builds an AC's defense profile via `AcCtrlParam` (`AcCtrlParamCalc.lua` lines 199–232,
    /// packed by `0x82873e70`).
    pub fn from_ac(design: &AcAssemblyDesignSt) -> Self {
        let ctrl = AcCtrlParam::calculate(design);
        let ch = |low: f32, hi: f32, thr: f32| DefenseChannel {
            low_def: low.clamp(0.0, 255.0) as u8,
            hi_def: hi.clamp(0.0, 255.0) as u8,
            threshold: thr.clamp(0.0, u16::MAX as f32) as u16,
            // `AcCtrlParamCalc.lua` lines 221-223: `staggerPenaltyDefThreshold* = *_thr * 0.8`.
            stagger_threshold: (thr * 0.8).clamp(0.0, u16::MAX as f32) as u16,
        };
        Self {
            ke: ch(ctrl.ke_low_def, ctrl.ke_hi_def, ctrl.ke_def_thr),
            ce: ch(ctrl.ce_low_def, ctrl.ce_hi_def, ctrl.ce_def_thr),
            te: ch(ctrl.te_low_def, ctrl.te_hi_def, ctrl.te_def_thr),
        }
    }

    pub fn channel(&self, attr: DamageType) -> DefenseChannel {
        match attr {
            DamageType::Ke => self.ke,
            DamageType::Ce => self.ce,
            DamageType::Te => self.te,
            DamageType::Unmitigated => DefenseChannel::UNMITIGATED,
        }
    }
}

/// Output of the 360 damage formula (`0x827d0b50` + `0x82984438`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HitResult {
    /// Final AP damage to subtract.
    pub damage: f32,
    /// Scaled impact recoil (`recoil * damage.bin[0]`).
    pub recoil: f32,
    /// `true` when `power > threshold` (penetration / effective hit), `false` on ricochet
    /// (bit 31 of result word `+8` in `0x827d0b50`).
    pub effective: bool,
}

/// Evaluates the 360 three-zone defense mitigation formula (`0x82874190` → `0x827d0d08` →
/// `0x827d0cc8` → `0x827d0b50`).
pub fn calculate_damage(
    attr: DamageType,
    power: f32,
    recoil: f32,
    defense: &DefenseProfile,
    staggered: bool,
) -> HitResult {
    let ch = defense.channel(attr);
    let thr = if staggered {
        f32::from(ch.stagger_threshold)
    } else {
        f32::from(ch.threshold)
    };
    // `NewAcBehavior` #431 (`+0x605` = 30%) and #432 (`+0x606` = 50%), read by `0x827d08b8`;
    // `Damage` #0 (`+0x00` = 1.0), read by `0x82984438` at `0x82984518`.
    let mid_width_pct = acvd_data::tuning("NewAcBehavior", 431).unwrap_or(30.0) as f32;
    let mid_min_rate_pct = acvd_data::tuning("NewAcBehavior", 432).unwrap_or(50.0) as f32;
    let recoil_scale = acvd_data::tuning("Damage", 0).unwrap_or(1.0) as f32;
    let scaled_recoil = recoil * recoil_scale;

    // `0x827d0978`: `low = power * low_def * 0.01`, floored to 0.0 if `< 1.0`.
    let raw_low = power * f32::from(ch.low_def) * 0.01;
    let low_dmg = if raw_low < 1.0 { 0.0 } else { raw_low };
    // `0x827d09c0`: `hi = power * hi_def * 0.01`.
    let hi_dmg = power * f32::from(ch.hi_def) * 0.01;
    // `0x827d0ba0`: `upper = thr * (1.0 + mid_width_pct * 0.01)`.
    let upper = thr * (1.0 + mid_width_pct * 0.01);

    if power <= thr {
        HitResult {
            damage: low_dmg,
            recoil: scaled_recoil,
            effective: false,
        }
    } else if power >= upper {
        HitResult {
            damage: hi_dmg,
            recoil: scaled_recoil,
            effective: true,
        }
    } else {
        // `0x827d0c40..0x827d0c88`:
        // `mid_start = low + (hi - low) * (mid_min_rate_pct * 0.01)`
        // `damage = mid_start + (hi - mid_start) * ((power - thr) / (upper - thr))`
        let mid_start = low_dmg + (hi_dmg - low_dmg) * (mid_min_rate_pct * 0.01);
        let t = (power - thr) / (upper - thr);
        HitResult {
            damage: mid_start + (hi_dmg - mid_start) * t,
            recoil: scaled_recoil,
            effective: true,
        }
    }
}

/// One local-space vertical hit sphere on a target entity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HitSphere {
    pub offset_y: f32,
    pub radius: f32,
}

/// AC body hit spheres (`0x82863b60`, `NewAcBehavior +0x22c/+0x230/+0x234`, same as `collision.rs`).
pub fn ac_hit_spheres() -> Vec<HitSphere> {
    vec![
        HitSphere {
            offset_y: 3.0,
            radius: 3.0,
        },
        HitSphere {
            offset_y: 6.0,
            radius: 5.5,
        },
    ]
}

/// Enemy hit spheres from `EnemyGraphicsSt` (`collision_radius`, `hit_upper_hit_sphere_radius`,
/// `hit_upper_hit_sphere_height_offset`, `one_col_sphere_offset`).
pub fn enemy_hit_spheres(gfx: &EnemyGraphicsSt) -> Vec<HitSphere> {
    let r = gfx.collision_radius.max(1.0);
    if gfx.hit_upper_hit_sphere_radius > 0.0 {
        vec![
            HitSphere {
                offset_y: r,
                radius: r,
            },
            HitSphere {
                offset_y: gfx.hit_upper_hit_sphere_height_offset,
                radius: gfx.hit_upper_hit_sphere_radius,
            },
        ]
    } else {
        let offset_y = if gfx.one_col_sphere_offset < 0.0 {
            r
        } else {
            gfx.one_col_sphere_offset
        };
        vec![HitSphere {
            offset_y,
            radius: r,
        }]
    }
}

/// Speed × weight melee damage scaling parameters (`0x828a3470` → `0x828a33a8` →
/// `0x828a2e50` / `0x828a20d8` / `0x828a2078` / `0x828a20a8`).
///
/// - Boost Charge Kick (`0x8286ac40`): `param/damage.bin` fields 11–15 (`+0x2c..+0x3c`):
///   `ref_speed = 100 km/h`, `ref_weight = 5000`, `max_speed = 10`, `max_weight = 10`, `min_rate = 0.4`.
/// - Physical blade & Heat Pile (`0x828a43e8`, gated by `0x8247f170` = `damage_type != TE`):
///   `param/damage.bin` fields 16–20 (`+0x40..+0x50`):
///   `ref_speed = 130 km/h`, `ref_weight = 3500`, `max_speed = 10`, `max_weight = 1.5`, `min_rate = 0.6`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpeedWeightScale {
    pub ref_speed_kmh: f32,
    pub ref_weight: f32,
    pub max_speed_factor: f32,
    pub max_weight_factor: f32,
    pub min_power_rate: f32,
}

impl SpeedWeightScale {
    /// Boost Charge Kick scaling (`param/damage.bin` fields 11–15, `0x8286ac40`).
    pub fn kick() -> Self {
        Self {
            ref_speed_kmh: DAMAGE.kick_ref_speed_kmh,
            ref_weight: DAMAGE.kick_ref_weight,
            max_speed_factor: DAMAGE.kick_max_speed_factor,
            max_weight_factor: DAMAGE.kick_max_weight_factor,
            min_power_rate: DAMAGE.kick_min_power_rate,
        }
    }

    /// Physical blade & Heat Pile scaling (`param/damage.bin` fields 16–20, `0x828a43e8`).
    pub fn physical_blade() -> Self {
        Self {
            ref_speed_kmh: DAMAGE.blade_ref_speed_kmh,
            ref_weight: DAMAGE.blade_ref_weight,
            max_speed_factor: DAMAGE.blade_max_speed_factor,
            max_weight_factor: DAMAGE.blade_max_weight_factor,
            min_power_rate: DAMAGE.blade_min_power_rate,
        }
    }

    /// Evaluates `(scaled_power, scaled_recoil)` from horizontal AC speed (km/h) and total build
    /// weight (`0x828a33a8` → `0x828a2e50`, `0x828a20d8`, `0x828a2078`, `0x828a20a8`).
    pub fn scale(
        self,
        base_power: f32,
        base_recoil: f32,
        horiz_speed_kmh: f32,
        total_weight: f32,
    ) -> (f32, f32) {
        let speed_f = if self.ref_speed_kmh > 0.0 {
            (horiz_speed_kmh / self.ref_speed_kmh).clamp(0.0, self.max_speed_factor)
        } else {
            1.0
        };
        let weight_f = if self.ref_weight > 0.0 {
            (total_weight / self.ref_weight).clamp(0.0, self.max_weight_factor)
        } else {
            1.0
        };
        let mult = (speed_f * weight_f).max(self.min_power_rate);
        (base_power * mult, base_recoil * mult)
    }
}

/// Tests a swept sphere (capsule from `a` to `b` with radius `sweep_radius`) against a target
/// sphere `(center, target_radius)`. Returns `(t, hit_point, outward_normal)`.
pub fn swept_sphere_hits_sphere(
    a: Vec3,
    b: Vec3,
    sweep_radius: f32,
    center: Vec3,
    target_radius: f32,
) -> Option<(f32, Vec3, Vec3)> {
    let (t, _center_at_hit, normal) =
        segment_hits_sphere(a, b, center, sweep_radius + target_radius)?;
    let hit_point = center + normal * target_radius;
    Some((t, hit_point, normal))
}

/// Tests directed segment `a -> b` against a sphere `(center, radius)`.
/// Returns `(t, hit_point, outward_normal)` for the earliest entry `t in [0.0, 1.0]`.
pub fn segment_hits_sphere(
    a: Vec3,
    b: Vec3,
    center: Vec3,
    radius: f32,
) -> Option<(f32, Vec3, Vec3)> {
    let d = b - a;
    let m = a - center;
    let c = m.dot(m) - radius * radius;
    if c <= 0.0 {
        let normal = m.normalize_or_zero();
        return Some((
            0.0,
            a,
            if normal == Vec3::ZERO {
                Vec3::Y
            } else {
                normal
            },
        ));
    }
    let len_sq = d.dot(d);
    if len_sq <= 1.0e-12 {
        return None;
    }
    let b_coef = m.dot(d);
    if b_coef > 0.0 {
        return None;
    }
    let disc = b_coef * b_coef - len_sq * c;
    if disc < 0.0 {
        return None;
    }
    let t = (-b_coef - disc.sqrt()) / len_sq;
    if !(0.0..=1.0).contains(&t) {
        return None;
    }
    let hit = a + d * t;
    let normal = (hit - center).normalize_or_zero();
    Some((
        t,
        hit,
        if normal == Vec3::ZERO {
            Vec3::Y
        } else {
            normal
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use acvd_data::find;
    use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;
    use acvd_data::generated::enemy::{ENEMY_ENEMYBASICPARAM_BIN, ENEMY_ENEMYGRAPHICSPARAM_BIN};

    #[test]
    fn three_zone_damage_formula_matches_360() {
        let def = DefenseProfile {
            ke: DefenseChannel {
                low_def: 20,
                hi_def: 80,
                threshold: 1000,
                stagger_threshold: 800,
            },
            ce: DefenseChannel::UNMITIGATED,
            te: DefenseChannel::UNMITIGATED,
        };
        // Ricochet: power <= 1000 -> 20% = 200.
        let r_low = calculate_damage(DamageType::Ke, 1000.0, 500.0, &def, false);
        assert!(!r_low.effective);
        assert!((r_low.damage - 200.0).abs() < 1e-4);
        assert!((r_low.recoil - 500.0).abs() < 1e-4);

        // Full penetration: power >= 1300 (1.3 * 1000) -> 80% = 1040.
        let r_hi = calculate_damage(DamageType::Ke, 1300.0, 500.0, &def, false);
        assert!(r_hi.effective);
        assert!((r_hi.damage - 1040.0).abs() < 1e-4);

        // Midpoint of transition zone: power = 1150 (t = 0.5).
        // low = 230, hi = 920, mid_start = 230 + (920 - 230) * 0.5 = 575,
        // damage = 575 + (920 - 575) * 0.5 = 747.5.
        let r_mid = calculate_damage(DamageType::Ke, 1150.0, 500.0, &def, false);
        assert!(r_mid.effective);
        assert!((r_mid.damage - 747.5).abs() < 1e-3, "got {}", r_mid.damage);

        // Stagger lowers threshold from 1000 to 800: power 1040 (= 1.3 * 800) now penetrates!
        let r_stag = calculate_damage(DamageType::Ke, 1040.0, 500.0, &def, true);
        assert!(r_stag.effective);
        assert!((r_stag.damage - 832.0).abs() < 1e-3);

        // Sub-1.0 ricochet damage floors to 0.0 (`0x827d0978`).
        let r_tiny = calculate_damage(DamageType::Ke, 4.0, 0.0, &def, false);
        assert!(!r_tiny.effective);
        assert_eq!(r_tiny.damage, 0.0);
    }

    #[test]
    fn rifle_vs_tank_and_def_mt_matches_disc_params() {
        // Starter rifle 1720: KE, power 1158.
        // Tank e1020 (row 1020): AP 1200, KE thr 100, hi_def 98 -> 1158 * 0.98 = 1134.84 (2 shots).
        let tank = &find(ENEMY_ENEMYBASICPARAM_BIN, 1020).unwrap().data;
        let tank_def = DefenseProfile::from_enemy(tank);
        let hit_tank = calculate_damage(DamageType::Ke, 1158.0, 628.0, &tank_def, false);
        assert!(hit_tank.effective);
        assert!((hit_tank.damage - 1158.0 * 0.98).abs() < 1e-2);
        assert!(tank.ap > hit_tank.damage && tank.ap <= hit_tank.damage * 2.0);

        // DEF-MT e0210 (row 210): KE thr 800, hi_def 84, upper = 1040 -> 1158 penetrates.
        let def_mt = &find(ENEMY_ENEMYBASICPARAM_BIN, 210).unwrap().data;
        let def_mt_def = DefenseProfile::from_enemy(def_mt);
        let hit_mt = calculate_damage(DamageType::Ke, 1158.0, 628.0, &def_mt_def, false);
        assert!(hit_mt.effective);
        assert!((hit_mt.damage - 1158.0 * 0.84).abs() < 1e-2);
    }

    #[test]
    fn ac_defense_profile_from_test_ac_designs() {
        // Test AC designs 5011..=5014 must produce positive KE/CE/TE defenses.
        for id in [5011u32, 5012, 5013, 5014] {
            let design = AC_ASSEMBLY_DESIGN_ST_FILES
                .iter()
                .flat_map(|(_, rows)| rows.iter())
                .find(|r| r.id == id)
                .map(|r| &r.data)
                .expect("test AC design");
            let def = DefenseProfile::from_ac(design);
            assert!(def.ke.threshold > 0 && def.ce.threshold > 0 && def.te.threshold > 0);
            assert!(def.ke.low_def > 0 && def.ke.hi_def > def.ke.low_def);
            assert!(def.ke.stagger_threshold < def.ke.threshold);
        }
    }

    #[test]
    fn enemy_hit_spheres_match_graphics_rows() {
        let tank = &find(ENEMY_ENEMYGRAPHICSPARAM_BIN, 1020).unwrap().data;
        assert_eq!(
            enemy_hit_spheres(tank),
            vec![HitSphere {
                offset_y: 3.0,
                radius: 3.0
            }]
        );
        let heli = &find(ENEMY_ENEMYGRAPHICSPARAM_BIN, 2030).unwrap().data;
        assert_eq!(
            enemy_hit_spheres(heli),
            vec![HitSphere {
                offset_y: 0.0,
                radius: 8.0
            }]
        );
        let mt = &find(ENEMY_ENEMYGRAPHICSPARAM_BIN, 10).unwrap().data;
        assert_eq!(
            enemy_hit_spheres(mt),
            vec![
                HitSphere {
                    offset_y: 5.0,
                    radius: 5.0
                },
                HitSphere {
                    offset_y: 8.0,
                    radius: 5.0
                },
            ]
        );
    }

    #[test]
    fn segment_sphere_hit_detects_entry_and_miss() {
        let hit = segment_hits_sphere(
            Vec3::new(0.0, 0.0, 10.0),
            Vec3::new(0.0, 0.0, -10.0),
            Vec3::ZERO,
            2.0,
        )
        .expect("segment passes through sphere");
        assert!((hit.0 - 0.4).abs() < 1e-5);
        assert!((hit.1 - Vec3::new(0.0, 0.0, 2.0)).length() < 1e-4);
        assert!((hit.2 - Vec3::Z).length() < 1e-4);

        assert!(segment_hits_sphere(
            Vec3::new(5.0, 0.0, 10.0),
            Vec3::new(5.0, 0.0, -10.0),
            Vec3::ZERO,
            2.0,
        )
        .is_none());
    }

    #[test]
    fn speed_weight_scaling_matches_360_kick_and_blade_params() {
        let kick = SpeedWeightScale::kick();
        assert_eq!(kick.ref_speed_kmh, 100.0);
        assert_eq!(kick.ref_weight, 5000.0);
        assert!((kick.min_power_rate - 0.4).abs() < 1e-6);

        // At 0 km/h, floor at min_power_rate = 0.4.
        let (p0, r0) = kick.scale(7000.0, 2500.0, 0.0, 6000.0);
        assert!((p0 - 2800.0).abs() < 1e-3);
        assert!((r0 - 1000.0).abs() < 1e-3);

        // At 150 km/h and 6000 weight: 1.5 * 1.2 = 1.8x.
        let (p1, r1) = kick.scale(7000.0, 2500.0, 150.0, 6000.0);
        assert!((p1 - 7000.0 * 1.8).abs() < 1e-2);
        assert!((r1 - 2500.0 * 1.8).abs() < 1e-2);

        // Physical blade / Heat Pile (`0x828a43e8`): ref 130 km/h, weight 3500, max_weight 1.5, floor 0.6.
        let blade = SpeedWeightScale::physical_blade();
        assert_eq!(blade.ref_speed_kmh, 130.0);
        assert_eq!(blade.ref_weight, 3500.0);
        assert!((blade.max_weight_factor - 1.5).abs() < 1e-6);
        assert!((blade.min_power_rate - 0.6).abs() < 1e-6);

        // Heavy AC (weight 9000 -> weight_factor clamped at 1.5) at 130 km/h -> 1.0 * 1.5 = 1.5x.
        let (bp, br) = blade.scale(6248.0, 1589.0, 130.0, 9000.0);
        assert!((bp - 6248.0 * 1.5).abs() < 1e-2);
        assert!((br - 1589.0 * 1.5).abs() < 1e-2);

        // Standstill physical blade floors at 0.6x.
        let (bp0, _) = blade.scale(6248.0, 1589.0, 0.0, 6000.0);
        assert!((bp0 - 6248.0 * 0.6).abs() < 1e-2);

        // Swept sphere hit detection: radius 7 m kick sphere passing within 9 m of a radius 3 m target hits.
        let hit = swept_sphere_hits_sphere(
            Vec3::new(0.0, 4.0, 15.0),
            Vec3::new(0.0, 4.0, -5.0),
            7.0,
            Vec3::new(8.0, 4.0, 0.0),
            3.0,
        );
        assert!(hit.is_some());
        let miss = swept_sphere_hits_sphere(
            Vec3::new(0.0, 4.0, 15.0),
            Vec3::new(0.0, 4.0, -5.0),
            7.0,
            Vec3::new(12.0, 4.0, 0.0),
            3.0,
        );
        assert!(miss.is_none());
    }
}
