//! Bevy-side player health on top of the pure `acvd-sim` damage model.
//!
//! KE/CE/TE mitigation, hit spheres, and swept-sphere tests live in `acvd-sim::damage`; this
//! module re-exports them so existing `crate::damage::*` call sites keep compiling, and keeps
//! the piloted AC's live AP/stagger/respawn component.

use acvd_data::generated::ac_unit::AcAssemblyDesignSt;
use acvd_data::generated::ctrl::AcCtrlParam;
use acvd_data::part_field;
use bevy::prelude::*;

pub use acvd_sim::damage::*;

/// Live AP, KE/CE/TE defense profile, stagger state, and hit spheres on the piloted player AC.
#[derive(Component, Debug, Clone)]
pub struct PlayerHealth {
    pub ap: f32,
    pub ap_max: f32,
    pub defense: DefenseProfile,
    /// Recoil threshold triggering stagger (`AcCtrlParam::stagger_penalty_threshold`).
    pub stagger_threshold: f32,
    /// Stagger penalty duration in seconds (`2.0` s per `AcCtrlParamCalc.lua` line 338).
    pub stagger_penalty_time: f32,
    /// Remaining stagger seconds (`> 0.0` applies the 0.8× defense threshold penalty).
    pub stagger_timer: f32,
    /// Local-space hit spheres (`0x82863b60`, `ac_hit_spheres()`).
    pub spheres: Vec<HitSphere>,
    /// True once player AP has reached 0.
    pub destroyed: bool,
    /// Consumed once on the lethal hit to play AC destruction explosion + cue.
    pub just_destroyed: bool,
    /// Seconds remaining until automatic AC TEST respawn after AP reaches 0.
    pub respawn_timer: f32,
    /// Number of enemy hits absorbed since spawn/reset.
    pub hits_taken: u32,
    /// Pending camera shake from the latest hit: `(recoil, staggered)`.
    pub hit_shake: Option<(f32, bool)>,
}

/// Automatic AC TEST respawn delay after player AP reaches 0.
pub const PLAYER_RESPAWN_SECS: f32 = 3.0;

impl PlayerHealth {
    /// Builds `PlayerHealth` for the piloted AC from its `AcAssemblyDesignSt`.
    pub fn from_design(design: &AcAssemblyDesignSt) -> Self {
        let ap_max = [
            (design.head, 0u8),
            (design.core, 1),
            (design.arms, 2),
            (design.legs, 3),
        ]
        .into_iter()
        .map(|(id, cat)| part_field(id as i64, cat, "ap").max(0.0))
        .sum::<f32>()
        .max(1.0);
        let ctrl = AcCtrlParam::calculate(design);
        Self {
            ap: ap_max,
            ap_max,
            defense: DefenseProfile::from_ac(design),
            stagger_threshold: ctrl.stagger_penalty_threshold,
            stagger_penalty_time: 2.0,
            stagger_timer: 0.0,
            spheres: ac_hit_spheres(),
            destroyed: false,
            just_destroyed: false,
            respawn_timer: 0.0,
            hits_taken: 0,
            hit_shake: None,
        }
    }

    pub fn is_staggered(&self) -> bool {
        self.stagger_timer > 0.0
    }

    /// Applies one incoming hit using the 360 three-zone formula and stagger check.
    pub fn apply_hit(&mut self, attr: DamageType, power: f32, recoil: f32) -> (HitResult, bool) {
        let res = calculate_damage(attr, power, recoil, &self.defense, self.is_staggered());
        if self.destroyed {
            return (res, false);
        }
        self.hits_taken = self.hits_taken.saturating_add(1);
        self.ap = (self.ap - res.damage).max(0.0);
        let mut triggered_stagger = false;
        if self.stagger_threshold > 0.0 && res.recoil >= self.stagger_threshold {
            self.stagger_timer = self.stagger_penalty_time;
            triggered_stagger = true;
        }
        let prev = self.hit_shake.unwrap_or((0.0, false));
        self.hit_shake = Some((prev.0.max(res.recoil), prev.1 || triggered_stagger));
        let lethal = self.ap <= 0.0;
        if lethal {
            self.destroyed = true;
            self.just_destroyed = true;
            self.respawn_timer = PLAYER_RESPAWN_SECS;
        }
        (res, lethal)
    }

    /// Current integer AP for `hud::Status::ap`.
    pub fn ap_u32(&self) -> u32 {
        if self.ap <= 0.0 {
            0
        } else {
            self.ap.ceil() as u32
        }
    }

    /// Advances stagger and respawn timers. Returns `true` when respawn should execute.
    pub fn tick(&mut self, dt: f32) -> bool {
        if self.stagger_timer > 0.0 {
            self.stagger_timer = (self.stagger_timer - dt).max(0.0);
        }
        if self.destroyed && self.respawn_timer > 0.0 {
            self.respawn_timer = (self.respawn_timer - dt).max(0.0);
            if self.respawn_timer <= 0.0 {
                return true;
            }
        }
        false
    }

    /// Restores full AP and clears stagger / destroyed state on AC TEST respawn.
    pub fn reset(&mut self) {
        self.ap = self.ap_max;
        self.stagger_timer = 0.0;
        self.hit_shake = None;
        self.destroyed = false;
        self.just_destroyed = false;
        self.respawn_timer = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;

    #[test]
    fn player_health_takes_damage_staggers_and_respawns() {
        let design = AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 5001)
            .map(|r| &r.data)
            .expect("design 5001");
        let mut ph = PlayerHealth::from_design(design);
        assert_eq!(ph.ap_u32(), 34695);
        assert!(!ph.is_staggered());
        assert_eq!(ph.spheres.len(), 2);

        let (res1, lethal1) = ph.apply_hit(DamageType::Ke, 3000.0, 1200.0);
        assert!(res1.effective && !lethal1);
        assert!(ph.ap < ph.ap_max);
        assert!(ph.is_staggered());

        let normal_ke_thr = f32::from(ph.defense.ke.threshold);
        let probe_power = normal_ke_thr * 0.9;
        let (res_stag, _) = ph.apply_hit(DamageType::Ke, probe_power, 0.0);
        assert!(res_stag.effective);

        assert!(!ph.tick(2.1));
        assert!(!ph.is_staggered());
        let (res_clear, _) = ph.apply_hit(DamageType::Ke, probe_power, 0.0);
        assert!(!res_clear.effective);

        let (_, lethal) = ph.apply_hit(DamageType::Unmitigated, 100_000.0, 0.0);
        assert!(lethal && ph.destroyed && ph.just_destroyed && ph.ap_u32() == 0);
        assert!(!ph.tick(1.5));
        assert!(ph.tick(2.0));
        ph.reset();
        assert!(!ph.destroyed && ph.ap_u32() == 34695 && !ph.is_staggered());
    }
}
