//! Deterministic fixed-step weapon, projectile, missile, blade, and bay-swap simulation.
//!
//! This module is the multiplayer-authoritative combat subset moved out of `acvd-game`:
//! - every step is exactly one 60 Hz tick (no wall clock, no `dt`),
//! - guns are resolved from the same `acvparts.bin` / `bullet/*.bin` rows the single-player code
//!   uses (`sheets/ac_part_fields.csv`, `sheets/bullet.csv`),
//! - spread is a PCG32 roll seeded by `(match_seed, tick, net_id, shot_seq)`,
//! - state references `NetId`/`ProjectileId`/`PropId`, never a Bevy `Entity`.
//!
//! `acvd-game` keeps its richer single-player presentation path; the server, the client
//! predictor, and the network visual projectiles all step the pieces here.

use acvd_data::generated::ac_unit::AcAssemblyDesignSt;
use acvd_data::generated::bullet::{
    BULLET_BULLETARISE_BIN, BULLET_BULLETBLADE_BIN, BULLET_BULLETENERGY_BIN,
    BULLET_BULLETEXPLOSION_BIN, BULLET_BULLETMISSILE_BIN, BULLET_BULLETRIGID_BIN,
    BULLET_BULLETSTATUSERROR_BIN, PARAM_BULLETHITSEPARAM_BIN, PARAM_BULLETHITSFXPARAM_BIN,
};
use acvd_data::generated::ctrl::AcCtrlParam;
use acvd_data::generated::sfx::PARAM_ACWEAPONSFXPARAM_BIN;
use acvd_data::generated::sound::PARAM_ACWEAPONSOUNDPARAM_BIN;
use acvd_data::{find, part_field};
use glam::{Quat, Vec3};

use crate::damage::DamageType;
use crate::ids::{NetId, ProjectileId};
use crate::input::InputFrame;
use crate::motion::{BladeLaunch, BladePhase};
use crate::rng::Pcg32;
use crate::tick::{TICK_DT, TICK_HZ};
use crate::world::{pose_state, SimAc};

/// Ticks from bay-shift start (input) to the rack grab that flips the active weapon
/// (`own000100.tae` `kind=101` at frame 150; `bay.rs` `GRAB_FRAME`).
pub const BAY_SWAP_TICKS: u16 = 150;
/// Fixed simulation frames per second as `f32` (360 `DAT_82004408`).
pub const TICK_RATE: f32 = TICK_HZ as f32;
/// Speed at or above which a round is validated by hitbox-history rewind instead of forward
/// simulation (sniper rifles, rail/laser cannons; design config, not game data).
pub const FAST_PROJECTILE_MPS: f32 = 180.0;
/// Maximum hitscan reach (design config).
pub const HITSCAN_MAX_RANGE: f32 = 1000.0;
/// Range beyond `damp_range` where damage decay reaches `min_power_rate` (`0x820466cc`).
pub const MAX_DECAY_RANGE: f32 = 2000.0;
/// Tracer lifetime in seconds (design config; matches the single-player tracer).
pub const MAX_LIFE: f32 = 5.0;
/// Shoot id written when a weapon has no `acweaponsoundparam` row (`0x82891e18`).
pub const DEFAULT_SHOOT: i16 = 0x12B;
const KMH_PER_MS: f32 = 3.6;

/// Hand order used by the wire `weapon_slot` byte and the rack arrays.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Hand {
    #[default]
    Right = 0,
    Left = 1,
}

impl Hand {
    pub fn index(self) -> usize {
        self as usize
    }
}

/// Broadcast weapon slot; matches the `weapons::Hand` order plus shoulder/overed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WeaponSlot {
    #[default]
    RightHand = 0,
    LeftHand = 1,
    Shoulder = 2,
    Overed = 3,
}

/// FFX / sound ids used by the client presentation layer.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Fx {
    pub bullet: i32,
    pub muzzle: i32,
    pub cartridge: i32,
    pub hit: i32,
    pub hit_low: i32,
    pub hit_hi: i32,
    pub after_accel: i32,
    pub delete_fx: i32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HitSe {
    pub low: i16,
    pub hi: i16,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    Skip,
    Rigid,
    Energy,
    Missile,
    Blade,
}

/// Range-decay curve packed by `0x8247d350` and evaluated by `0x8298be98`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RangeDecay {
    pub no_decay_range: f32,
    pub damp_range: f32,
    pub guarantee_rate: f32,
    pub min_power_rate: f32,
}

impl RangeDecay {
    pub fn multiplier(self, dist: f32) -> f32 {
        let g = self.guarantee_rate * 0.01;
        let m = self.min_power_rate * 0.01;
        if dist <= self.no_decay_range {
            1.0
        } else if dist < self.damp_range {
            let span = self.damp_range - self.no_decay_range;
            let t = if span <= 0.0 {
                0.0
            } else {
                ((dist - self.no_decay_range) / span).clamp(0.0, 1.0)
            };
            1.0 + (g - 1.0) * t
        } else if dist < MAX_DECAY_RANGE {
            let span = MAX_DECAY_RANGE - self.damp_range;
            let t = if span <= 0.0 {
                0.0
            } else {
                ((dist - self.damp_range) / span).clamp(0.0, 1.0)
            };
            g + (m - g) * t
        } else {
            m
        }
    }
}

/// Ballistic parameters from `bullet/bulletrigid.bin` / `bullet/bulletenergy.bin`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Flight {
    pub min_speed: f32,
    pub max_speed: f32,
    pub air_cp: f32,
    pub stalled_air_cp_h: f32,
    pub stalled_air_cp_v: f32,
    pub gravity: f32,
    pub brake: f32,
    pub stall_to_vanish_frame: Option<f32>,
}

/// Sub-munition split specification from `BulletMissileSt`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MissileSplit {
    pub force_trace_frame: f32,
    pub start_frame: f32,
    pub start_dist: f32,
    pub arise_id: u32,
    pub count: u8,
    pub shoot_speed: f32,
}

/// Missile flight parameters from `bullet/bulletmissile.bin`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MissileFlight {
    pub accel: f32,
    pub air_cp: f32,
    pub before_accel_gravity: f32,
    pub after_accel_gravity: f32,
    pub brake: f32,
    pub accel_start_frame: f32,
    pub accel_end_frame: f32,
    pub act_start_frame: f32,
    pub trace_turn_deg_s: f32,
    pub min_speed: f32,
    pub max_speed: f32,
    pub hit_radius: f32,
    pub proximity_fuse_range: f32,
    pub split: Option<MissileSplit>,
}

/// Melee blade parameters from `bullet/bulletblade.bin` and `acvparts.bin` category 10.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BladeSpec {
    pub hit_radius: f32,
    pub blade_radius: f32,
    pub max_hits: u8,
    pub decay_power: f32,
    pub decay_recoil: f32,
    pub physical: bool,
    pub infinite_ammo: bool,
}

/// Secondary AoE blast (`BULLET_ARISE_ST` -> `BULLET_EXPLOSION_ST` / `BULLET_STATUS_ERROR_ST`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImpactBlast {
    pub radius: f32,
    pub damage_type: DamageType,
    pub damage_power: f32,
    pub damage_recoil: f32,
    pub hit_fx: i32,
    pub hit_se: i16,
}

/// Hand-weapon firing mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FireMode {
    #[default]
    Auto,
    PelletSpread,
    Burst,
    Charge,
}

/// Result of stepping one weapon's trigger for a tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TriggerAction {
    None,
    StartCharge,
    Fire { power_scale: f32, pellets: usize },
}

/// One equipped weapon's deterministic state.
#[derive(Clone, Copy, Debug)]
pub struct Gun {
    pub remaining: u16,
    pub magazine: u16,
    pub reload_time: f32,
    pub reload_stab_factor: f32,
    pub cooldown: f32,
    pub init_speed: f32,
    pub kind: Kind,
    pub flight: Flight,
    pub missile_flight: Option<MissileFlight>,
    pub blade: Option<BladeSpec>,
    pub is_missile: bool,
    pub weapon_lock_max: u8,
    pub burst_count: u8,
    pub burst_interval: f32,
    pub damage_type: DamageType,
    pub damage_power: f32,
    pub damage_recoil: f32,
    pub en_drain: f32,
    pub decay: RangeDecay,
    pub fx: Fx,
    pub shoot: i16,
    pub hit_se: HitSe,
    pub stance: bool,
    pub sniper: bool,
    pub fire_mode: FireMode,
    pub charge_time: f32,
    pub charge_ticks: f32,
    pub burst_wait: f32,
    pub burst_remaining: u8,
    pub burst_timer: f32,
    pub trigger_held: bool,
}

impl Gun {
    pub fn is_silent(&self) -> bool {
        self.kind == Kind::Skip
    }

    /// High-velocity rounds are validated against rewound hit spheres instead of forward sim.
    pub fn is_hitscan(&self) -> bool {
        self.kind != Kind::Skip && self.init_speed >= FAST_PROJECTILE_MPS
    }

    pub fn charge_fraction(&self) -> f32 {
        if self.charge_time > 0.0 {
            (self.charge_ticks / self.charge_time).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// One 60 Hz trigger tick. `can` folds in ammo, cooldown, EN, and action restrictions.
    pub fn step_trigger(&mut self, want: bool, can: bool, stance_ready: bool) -> TriggerAction {
        let was_held = self.trigger_held;
        self.trigger_held = want;
        match self.fire_mode {
            FireMode::Auto => {
                if want && can && stance_ready {
                    TriggerAction::Fire {
                        power_scale: 1.0,
                        pellets: 1,
                    }
                } else {
                    TriggerAction::None
                }
            }
            FireMode::PelletSpread => {
                if want && can && stance_ready {
                    TriggerAction::Fire {
                        power_scale: 1.0,
                        pellets: self.burst_count.max(1) as usize,
                    }
                } else {
                    TriggerAction::None
                }
            }
            FireMode::Burst => {
                if self.burst_remaining > 0 {
                    self.burst_timer = (self.burst_timer - 1.0).max(0.0);
                    if self.burst_timer <= 0.0 && self.remaining > 0 {
                        self.burst_remaining -= 1;
                        let delay = self.burst_wait.max(self.burst_interval).max(2.0);
                        self.burst_timer = delay;
                        return TriggerAction::Fire {
                            power_scale: 1.0,
                            pellets: 1,
                        };
                    }
                    return TriggerAction::None;
                }
                if want && can && stance_ready {
                    let count = self.burst_count.max(1).min(self.remaining as u8);
                    if count > 1 {
                        self.burst_remaining = count - 1;
                        self.burst_timer = self.burst_wait.max(self.burst_interval).max(2.0);
                    }
                    TriggerAction::Fire {
                        power_scale: 1.0,
                        pellets: 1,
                    }
                } else {
                    TriggerAction::None
                }
            }
            FireMode::Charge => {
                if want && can && stance_ready {
                    let started = self.charge_ticks <= 0.0;
                    self.charge_ticks = (self.charge_ticks + 1.0).min(self.charge_time.max(1.0));
                    if started {
                        TriggerAction::StartCharge
                    } else {
                        TriggerAction::None
                    }
                } else if !want && was_held && self.charge_ticks > 0.0 && can {
                    let frac = self.charge_fraction();
                    self.charge_ticks = 0.0;
                    TriggerAction::Fire {
                        power_scale: 0.5 + 0.5 * frac,
                        pellets: 1,
                    }
                } else {
                    if !want || !can {
                        self.charge_ticks = 0.0;
                    }
                    TriggerAction::None
                }
            }
        }
    }

    pub fn apply_reload_stability(&mut self, ctrl: &AcCtrlParam) {
        if self.kind != Kind::Skip && self.reload_time > 0.0 {
            let balance = if self.stance {
                ctrl.snipe_attack_balance
            } else {
                ctrl.attack_balance
            };
            let denom = ctrl.attack_stab_max_rate - 1.0;
            let mut factor = if denom.abs() > 1.0e-6 {
                let t = (self.reload_stab_factor - 1.0) / denom;
                (1.0 - t) + t * balance
            } else {
                1.0
            };
            if !self.stance {
                factor *= ctrl.arm_shoot_stability;
            }
            self.reload_time *= factor.max(0.05);
        }
    }

    /// Bay-swap and reload lockout status for the compact `AcRecord::reload` byte.
    pub fn reload_ticks(&self) -> u8 {
        self.cooldown.ceil().clamp(0.0, 255.0) as u8
    }
}

/// A shot emitted by the armament simulator.
#[derive(Clone, Copy, Debug)]
pub struct Shot {
    pub id: ProjectileId,
    pub slot: WeaponSlot,
    pub position: Vec3,
    pub dir: Vec3,
    pub target: Option<NetId>,
    pub gun: Gun,
    pub seed: u8,
}

/// Output of one armament tick.
#[derive(Clone, Debug, Default)]
pub struct WeaponOutput {
    pub shots: Vec<Shot>,
    pub blades: Vec<BladeLaunch>,
    pub swap_started: [bool; 2],
    pub swap_finished: [bool; 2],
}

/// One per-AC armament: hand weapons, hanger/bay weapons, shoulder unit, and rack-swap state.
#[derive(Clone, Debug)]
pub struct Armament {
    pub hands: [Gun; 2],
    pub bays: [Gun; 2],
    pub shoulder: Gun,
    /// `true` when the hanger/bay weapon is in the hand (the shoulder rack is empty).
    pub active_bay: [bool; 2],
    /// Remaining ticks of an active bay swap (`0` = idle).
    pub swap_ticks: [u16; 2],
    /// Queued missile/rocket salvo rounds (`Some(target)` for homing missiles).
    pub salvo: Vec<Option<NetId>>,
    pub salvo_timer: f32,
    pub salvo_index: u32,
    /// Rising-edge latch for the one-shot bay buttons.
    pub prev_bay: [bool; 2],
    pub weapon_arm: bool,
}

impl Default for Armament {
    fn default() -> Self {
        let silent = silent();
        Self {
            hands: [silent, silent],
            bays: [silent, silent],
            shoulder: silent,
            active_bay: [false; 2],
            swap_ticks: [0; 2],
            salvo: Vec::new(),
            salvo_timer: 0.0,
            salvo_index: 0,
            prev_bay: [false; 2],
            weapon_arm: false,
        }
    }
}

impl Armament {
    /// Builds the armament exactly as the single-player `Armament::from_design` does.
    pub fn from_design(design: &AcAssemblyDesignSt) -> Self {
        let ctrl = AcCtrlParam::calculate(design);
        let weapon_arm = part_field(design.arms as i64, 2, "is_weapon_arm") != 0.0;
        if weapon_arm {
            let wid = part_field(design.arms as i64, 2, "weapon_arm_id") as i64;
            let mut wa = gun_from_category(wid, 10, 1);
            wa.apply_reload_stability(&ctrl);
            return Self {
                hands: [wa, silent()],
                weapon_arm: true,
                ..Self::default()
            };
        }
        let arms_mount =
            (part_field(design.arms as i64, 2, "shoulder_mount_count").round() as u8).max(1);
        let mut hands = [gun(design.armwep_r as i64), gun(design.armwep_l as i64)];
        let mut bays = [gun(design.hanger_r as i64), gun(design.hanger_l as i64)];
        let mut shoulder = shoulder_gun(design.shoulder as i64, arms_mount);
        for g in hands.iter_mut().chain(bays.iter_mut()) {
            g.apply_reload_stability(&ctrl);
        }
        shoulder.apply_reload_stability(&ctrl);
        Self {
            hands,
            bays,
            shoulder,
            ..Self::default()
        }
    }

    /// The gun currently in `hand` (hand slot or hanger slot when the rack is active).
    pub fn active_gun(&self, hand: usize) -> &Gun {
        if self.active_bay[hand.min(1)] {
            &self.bays[hand.min(1)]
        } else {
            &self.hands[hand.min(1)]
        }
    }

    fn active_gun_mut(&mut self, hand: usize) -> &mut Gun {
        if self.active_bay[hand.min(1)] {
            &mut self.bays[hand.min(1)]
        } else {
            &mut self.hands[hand.min(1)]
        }
    }

    pub fn swapping(&self, hand: usize) -> bool {
        self.swap_ticks[hand.min(1)] > 0
    }

    /// Compact replicated combat flags (`acvd_proto::combat_flags` layout).
    pub fn combat_flags(&self) -> u8 {
        let mut flags = 0;
        if self.active_bay[0] {
            flags |= acvd_proto::combat_flags::RIGHT_BAY_ACTIVE;
        }
        if self.active_bay[1] {
            flags |= acvd_proto::combat_flags::LEFT_BAY_ACTIVE;
        }
        if self.swapping(0) {
            flags |= acvd_proto::combat_flags::RIGHT_SWAPPING;
        }
        if self.swapping(1) {
            flags |= acvd_proto::combat_flags::LEFT_SWAPPING;
        }
        flags
    }

    /// Restores the compact combat flags/swap timers from an authoritative state.
    pub fn apply_combat_state(&mut self, flags: u8, swap_ticks: [u16; 2]) {
        self.active_bay[0] = flags & acvd_proto::combat_flags::RIGHT_BAY_ACTIVE != 0;
        self.active_bay[1] = flags & acvd_proto::combat_flags::LEFT_BAY_ACTIVE != 0;
        self.swap_ticks = swap_ticks;
    }

    /// Full per-slot state used by `LocalPlayerState` (prediction replays fire validation).
    pub fn write_local_state(&self, out: &mut acvd_proto::LocalPlayerState) {
        out.combat_flags = self.combat_flags();
        out.swap_ticks = self.swap_ticks;
        out.hand_ammo = [self.hands[0].remaining, self.hands[1].remaining];
        out.bay_ammo = [self.bays[0].remaining, self.bays[1].remaining];
        out.shoulder_ammo = self.shoulder.remaining;
        out.hand_cooldown = [self.hands[0].cooldown, self.hands[1].cooldown];
        out.bay_cooldown = [self.bays[0].cooldown, self.bays[1].cooldown];
        out.shoulder_cooldown = self.shoulder.cooldown;
        out.hand_charge = [self.hands[0].charge_ticks, self.hands[1].charge_ticks];
        out.bay_charge = [self.bays[0].charge_ticks, self.bays[1].charge_ticks];
        out.shoulder_charge = self.shoulder.charge_ticks;
        out.burst_remaining = [self.hands[0].burst_remaining, self.hands[1].burst_remaining];
        out.burst_timer = [self.hands[0].burst_timer, self.hands[1].burst_timer];
    }

    pub fn read_local_state(&mut self, state: &acvd_proto::LocalPlayerState) {
        self.apply_combat_state(state.combat_flags, state.swap_ticks);
        self.hands[0].remaining = state.hand_ammo[0];
        self.hands[1].remaining = state.hand_ammo[1];
        self.bays[0].remaining = state.bay_ammo[0];
        self.bays[1].remaining = state.bay_ammo[1];
        self.shoulder.remaining = state.shoulder_ammo;
        self.hands[0].cooldown = state.hand_cooldown[0];
        self.hands[1].cooldown = state.hand_cooldown[1];
        self.bays[0].cooldown = state.bay_cooldown[0];
        self.bays[1].cooldown = state.bay_cooldown[1];
        self.shoulder.cooldown = state.shoulder_cooldown;
        self.hands[0].charge_ticks = state.hand_charge[0];
        self.hands[1].charge_ticks = state.hand_charge[1];
        self.bays[0].charge_ticks = state.bay_charge[0];
        self.bays[1].charge_ticks = state.bay_charge[1];
        self.shoulder.charge_ticks = state.shoulder_charge;
        self.hands[0].burst_remaining = state.burst_remaining[0];
        self.hands[1].burst_remaining = state.burst_remaining[1];
        self.hands[0].burst_timer = state.burst_timer[0];
        self.hands[1].burst_timer = state.burst_timer[1];
    }

    /// Replaces the armament with a fresh one (respawn reload).
    pub fn replenish(&mut self, design: &AcAssemblyDesignSt) {
        *self = Self::from_design(design);
    }

    /// Steps one fixed tick of cooldowns, swaps, triggers, and salvo release.
    ///
    /// `blocked` is the action restriction (`AcActRestrictionParamSt.b_shoot == 0`): scan mode,
    /// kick, or blade. `lock_pos` is the server's interest hint only; damage is never trusted from
    /// it.
    #[allow(clippy::too_many_arguments)]
    pub fn step_tick(
        &mut self,
        input: &InputFrame,
        tick: u32,
        net_id: NetId,
        match_seed: u64,
        position: Vec3,
        aim_dir: Vec3,
        lock_target: NetId,
        lock_pos: Option<Vec3>,
        blocked: bool,
        en: &mut f32,
        next_projectile: &mut u32,
        shot_seq: &mut u32,
    ) -> WeaponOutput {
        let mut out = WeaponOutput::default();
        for gun in self
            .hands
            .iter_mut()
            .chain(self.bays.iter_mut())
            .chain(std::iter::once(&mut self.shoulder))
        {
            gun.cooldown = (gun.cooldown - 1.0).max(0.0);
        }

        // Bay/rack swap: one 150-tick carousel, flipped at the grab.
        let requests = [input.bay_r, input.bay_l];
        for side in 0..2 {
            if requests[side] && !self.prev_bay[side] && !self.swapping(side) {
                // A rack swap needs an incoming hanger weapon; an empty bay is a no-op.
                if self.bays[side].kind != Kind::Skip {
                    self.swap_ticks[side] = BAY_SWAP_TICKS;
                    out.swap_started[side] = true;
                }
            }
            // The swap starts on this tick; its 150-tick carousel begins next tick.
            if self.swap_ticks[side] > 0 && !out.swap_started[side] {
                self.swap_ticks[side] -= 1;
                if self.swap_ticks[side] == 0 {
                    self.active_bay[side] = !self.active_bay[side];
                    out.swap_finished[side] = true;
                }
            }
        }
        self.prev_bay = requests;

        let right = aim_dir.cross(Vec3::Y).normalize_or(Vec3::X);
        for side in 0..2 {
            let want = if side == 0 {
                input.fire_r
            } else {
                input.fire_l
            };
            if self.swapping(side) || blocked {
                // Still advance held triggers/charge decay with `want = false` so a charge cannot
                // finish while the arm is busy.
                let gun = self.active_gun_mut(side);
                let _ = gun.step_trigger(false, false, true);
                continue;
            }
            let gun = self.active_gun_mut(side);
            let has_ammo = gun
                .blade
                .map_or(gun.remaining > 0, |b| b.infinite_ammo || gun.remaining > 0);
            let can = !gun.is_silent() && has_ammo && gun.cooldown <= 0.0 && *en >= gun.en_drain;
            if gun.kind == Kind::Blade {
                if want && can {
                    let blade = gun.blade.unwrap_or_default();
                    let launch = BladeLaunch {
                        hand_idx: side,
                        physical: blade.physical,
                        target: lock_pos.map(|p| (0, p)),
                        hit_radius: blade.hit_radius,
                        blade_radius: blade.blade_radius,
                        max_hits: blade.max_hits,
                        decay_power: blade.decay_power,
                        decay_recoil: blade.decay_recoil,
                        damage_type: gun.damage_type,
                        damage_power: gun.damage_power,
                        damage_recoil: gun.damage_recoil,
                        trail_fx: gun.fx.bullet,
                        hit_low_fx: gun.fx.hit_low,
                        hit_hi_fx: gun.fx.hit_hi,
                        hit_low_se: gun.hit_se.low,
                        hit_hi_se: gun.hit_se.hi,
                    };
                    out.blades.push(launch);
                    *en = (*en - gun.en_drain).max(0.0);
                    if !blade.infinite_ammo {
                        gun.remaining = gun.remaining.saturating_sub(1);
                    }
                    gun.cooldown = gun.reload_time.max(1.0);
                }
                continue;
            }
            match gun.step_trigger(want, can, true) {
                TriggerAction::None | TriggerAction::StartCharge => {}
                TriggerAction::Fire {
                    power_scale,
                    pellets,
                } => {
                    *en = (*en - gun.en_drain).max(0.0);
                    gun.remaining = gun.remaining.saturating_sub(1);
                    if gun.burst_remaining == 0 {
                        gun.cooldown = gun.reload_time.max(1.0);
                    }
                    let slot = if side == 0 {
                        WeaponSlot::RightHand
                    } else {
                        WeaponSlot::LeftHand
                    };
                    let origin = position + Vec3::Y * 4.5 + aim_dir * 1.0;
                    Self::emit_gun_shots(
                        gun,
                        slot,
                        origin,
                        aim_dir,
                        lock_target,
                        tick,
                        net_id,
                        match_seed,
                        next_projectile,
                        shot_seq,
                        power_scale,
                        pellets,
                        right,
                        &mut out,
                    );
                }
            }
        }

        // Shoulder unit: missiles queue a salvo, everything else fires directly.
        let want_shoulder = input.fire_shoulder;
        if !blocked && !self.shoulder.is_silent() {
            let gun = &mut self.shoulder;
            let can = gun.remaining > 0 && gun.cooldown <= 0.0 && *en >= gun.en_drain;
            if self.salvo.is_empty() && want_shoulder && can {
                let burst = gun.burst_count.max(1) as usize;
                let count = if gun.is_missile {
                    burst
                } else {
                    burst.min(gun.remaining as usize)
                };
                for _ in 0..count {
                    self.salvo
                        .push(lock_target.is_some().then_some(lock_target));
                }
                if !self.salvo.is_empty() {
                    self.salvo_timer = 0.0;
                    self.salvo_index = 0;
                    gun.cooldown = gun.reload_time.max(1.0);
                }
            }
            if !self.salvo.is_empty() {
                self.salvo_timer = (self.salvo_timer - 1.0).max(0.0);
                if self.salvo_timer <= 0.0 {
                    let target = self.salvo.remove(0);
                    let gun = &mut self.shoulder;
                    let can = gun.remaining > 0 && *en >= gun.en_drain;
                    if can {
                        *en = (*en - gun.en_drain).max(0.0);
                        gun.remaining = gun.remaining.saturating_sub(1);
                        let side = if self.salvo_index % 2 == 0 { 1.0 } else { -1.0 };
                        let origin = position + Vec3::Y * 5.5 + aim_dir * 0.8;
                        let launch_dir = if gun.is_missile {
                            (aim_dir + Vec3::Y * 0.25 + right * (0.22 * side)).normalize_or(aim_dir)
                        } else {
                            aim_dir
                        };
                        let id = alloc_id(next_projectile);
                        let seed = shot_seed(match_seed, tick, net_id, *shot_seq);
                        *shot_seq = shot_seq.wrapping_add(1);
                        out.shots.push(Shot {
                            id,
                            slot: WeaponSlot::Shoulder,
                            position: origin + right * (0.6 * side),
                            dir: launch_dir,
                            target,
                            gun: *gun,
                            seed,
                        });
                    }
                    self.salvo_timer = self.shoulder.burst_interval.max(3.0);
                    self.salvo_index = self.salvo_index.wrapping_add(1);
                }
            }
        }

        out
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_gun_shots(
        gun: &Gun,
        slot: WeaponSlot,
        origin: Vec3,
        aim: Vec3,
        lock_target: NetId,
        tick: u32,
        net_id: NetId,
        match_seed: u64,
        next_projectile: &mut u32,
        shot_seq: &mut u32,
        power_scale: f32,
        pellets: usize,
        right: Vec3,
        out: &mut WeaponOutput,
    ) {
        debug_assert!(!gun.is_silent());
        if gun.kind == Kind::Missile {
            let id = alloc_id(next_projectile);
            let seed = shot_seed(match_seed, tick, net_id, *shot_seq);
            *shot_seq = shot_seq.wrapping_add(1);
            let mut shot_gun = *gun;
            shot_gun.damage_power *= power_scale.clamp(0.1, 2.0);
            shot_gun.damage_recoil *= power_scale.clamp(0.1, 2.0);
            out.shots.push(Shot {
                id,
                slot,
                position: origin,
                dir: aim,
                target: lock_target.is_some().then_some(lock_target),
                gun: shot_gun,
                seed,
            });
            return;
        }
        let pellets = pellets.max(1);
        let up = right.cross(aim).normalize_or(Vec3::Y);
        let spread = if gun.stance || gun.sniper {
            0.0
        } else if pellets > 1 {
            0.04
        } else {
            0.0015
        };
        for p in 0..pellets {
            let mut rng = Pcg32::for_shot(match_seed, tick, net_id, *shot_seq);
            *shot_seq = shot_seq.wrapping_add(1);
            let az = if pellets > 1 {
                std::f32::consts::TAU * (p as f32) / (pellets as f32)
            } else {
                rng.range_f32(0.0, std::f32::consts::TAU)
            };
            let ring = if pellets > 1 {
                let jitter = rng.range_f32(-0.006, 0.006);
                (spread + jitter).max(0.0)
            } else {
                spread
            };
            let dir = if ring > 0.0 {
                (aim + (right * az.cos() + up * az.sin()) * ring).normalize_or(aim)
            } else {
                aim
            };
            let id = alloc_id(next_projectile);
            let seed = shot_seed(match_seed, tick, net_id, *shot_seq);
            let mut shot_gun = *gun;
            let scale = power_scale.clamp(0.1, 2.0);
            shot_gun.damage_power *= scale;
            shot_gun.damage_recoil *= scale;
            out.shots.push(Shot {
                id,
                slot,
                position: origin,
                dir,
                target: None,
                gun: shot_gun,
                seed,
            });
        }
    }
}

fn alloc_id(next: &mut u32) -> ProjectileId {
    let id = ProjectileId(*next);
    *next = next.wrapping_add(1);
    if *next == 0 {
        *next = 1;
    }
    id
}

fn shot_seed(match_seed: u64, tick: u32, net_id: NetId, shot_seq: u32) -> u8 {
    Pcg32::for_shot(match_seed, tick, net_id, shot_seq).next_u32() as u8
}

/// One in-flight rigid/energy projectile (fixed 60 Hz integration).
#[derive(Clone, Copy, Debug)]
pub struct Projectile {
    pub id: ProjectileId,
    pub owner: NetId,
    pub slot: WeaponSlot,
    pub position: Vec3,
    pub velocity: Vec3,
    pub flight: Flight,
    pub decay: RangeDecay,
    pub damage_type: DamageType,
    pub damage_power: f32,
    pub damage_recoil: f32,
    pub distance: f32,
    pub stall_drop: f32,
    pub stall_frames: f32,
    pub life: f32,
    pub hit: i32,
    pub hit_low: i32,
    pub hit_hi: i32,
    pub hit_se: HitSe,
    pub blast: Option<ImpactBlast>,
    pub seed: u8,
    pub age_tick: u32,
}

impl Projectile {
    pub fn from_shot(shot: &Shot, owner: NetId, tick: u32) -> Self {
        let gun = &shot.gun;
        Self {
            id: shot.id,
            owner,
            slot: shot.slot,
            position: shot.position,
            velocity: shot.dir * gun.init_speed.max(1.0),
            flight: gun.flight,
            decay: gun.decay,
            damage_type: gun.damage_type,
            damage_power: gun.damage_power,
            damage_recoil: gun.damage_recoil,
            distance: 0.0,
            stall_drop: 0.0,
            stall_frames: 0.0,
            life: MAX_LIFE,
            hit: gun.fx.hit,
            hit_low: gun.fx.hit_low,
            hit_hi: gun.fx.hit_hi,
            hit_se: gun.hit_se,
            blast: None,
            seed: shot.seed,
            age_tick: tick,
        }
    }

    /// One fixed 60 Hz step. Returns the world displacement or `None` when the round vanishes.
    pub fn step_tick(&mut self) -> Option<Vec3> {
        self.life -= TICK_DT;
        if self.life <= 0.0 {
            return None;
        }
        let stalled = self.distance > self.decay.damp_range;
        let (cp, brake) = if stalled {
            (self.flight.stalled_air_cp_h, self.flight.brake)
        } else {
            (self.flight.air_cp, 0.0)
        };
        let speed = self.velocity.length();
        if speed > 0.0 {
            let dir = self.velocity / speed;
            let drag = (speed * cp + brake).min(speed);
            self.velocity -= dir * drag;
        }
        self.velocity.y -= self.flight.gravity;
        let new_speed = self.velocity.length();
        if new_speed > 0.0 && self.flight.max_speed > 0.0 {
            let clamped = new_speed.clamp(self.flight.min_speed, self.flight.max_speed);
            self.velocity *= clamped / new_speed;
        }
        if stalled {
            if self.flight.gravity > 0.0 || self.flight.stalled_air_cp_v > 0.0 {
                self.stall_drop = (self.stall_drop + self.flight.gravity)
                    - self.stall_drop * self.flight.stalled_air_cp_v;
            }
            if let Some(vanish) = self.flight.stall_to_vanish_frame {
                self.stall_frames += 1.0;
                if self.stall_frames > vanish {
                    return None;
                }
            }
        }
        let step_vel = Vec3::new(
            self.velocity.x,
            self.velocity.y - self.stall_drop,
            self.velocity.z,
        );
        let delta = step_vel * TICK_DT;
        self.distance += delta.length();
        Some(delta)
    }

    pub fn effective_damage(&self) -> (f32, f32) {
        let mult = self.decay.multiplier(self.distance);
        (self.damage_power * mult, self.damage_recoil * mult)
    }
}

/// One in-flight homing missile (fixed 60 Hz integration).
#[derive(Clone, Copy, Debug)]
pub struct Missile {
    pub id: ProjectileId,
    pub owner: NetId,
    pub slot: WeaponSlot,
    pub position: Vec3,
    pub velocity: Vec3,
    pub flight: MissileFlight,
    pub decay: RangeDecay,
    pub damage_type: DamageType,
    pub damage_power: f32,
    pub damage_recoil: f32,
    pub target: Option<NetId>,
    pub age_frames: f32,
    pub ignited: bool,
    pub distance: f32,
    pub life: f32,
    pub bullet_fx: i32,
    pub hit: i32,
    pub hit_low: i32,
    pub hit_hi: i32,
    pub delete_fx: i32,
    pub after_accel_fx: i32,
    pub hit_se: HitSe,
    pub split_depth: u8,
    pub blast: Option<ImpactBlast>,
    pub seed: u8,
}

impl Missile {
    pub fn from_shot(shot: &Shot, owner: NetId) -> Self {
        let gun = &shot.gun;
        let flight = gun.missile_flight.unwrap_or_default();
        let max_life = ((flight.accel_end_frame + 120.0) / TICK_RATE).clamp(5.0, 14.0);
        Self {
            id: shot.id,
            owner,
            slot: shot.slot,
            position: shot.position,
            velocity: shot.dir * gun.init_speed.max(flight.min_speed).max(10.0),
            flight,
            decay: gun.decay,
            damage_type: gun.damage_type,
            damage_power: gun.damage_power,
            damage_recoil: gun.damage_recoil,
            target: shot.target,
            age_frames: 0.0,
            ignited: false,
            distance: 0.0,
            life: max_life,
            bullet_fx: gun.fx.bullet,
            hit: gun.fx.hit,
            hit_low: gun.fx.hit_low,
            hit_hi: gun.fx.hit_hi,
            delete_fx: gun.fx.delete_fx,
            after_accel_fx: gun.fx.after_accel,
            hit_se: gun.hit_se,
            split_depth: 0,
            blast: None,
            seed: shot.seed,
        }
    }

    /// One fixed 60 Hz homing/boost/coast step. Returns `(delta, just_ignited)` or `None`.
    pub fn step_tick(&mut self, target_pos: Option<Vec3>) -> Option<(Vec3, bool)> {
        self.life -= TICK_DT;
        if self.life <= 0.0 {
            return None;
        }
        self.age_frames += 1.0;
        let in_launch = self.age_frames < self.flight.accel_start_frame;
        let in_boost = self.age_frames >= self.flight.accel_start_frame
            && self.age_frames <= self.flight.accel_end_frame;
        let just_ignited = in_boost && !self.ignited;
        if just_ignited {
            self.ignited = true;
        }
        let homing = self.age_frames >= self.flight.act_start_frame
            && self.age_frames <= self.flight.accel_end_frame;
        let mut speed = self.velocity.length();
        let mut dir = if speed > 1.0e-4 {
            self.velocity / speed
        } else {
            Vec3::NEG_Z
        };
        if homing {
            if let Some(tpos) = target_pos {
                let to_target = (tpos - self.position).normalize_or_zero();
                if to_target != Vec3::ZERO {
                    let dot = dir.dot(to_target).clamp(-1.0, 1.0);
                    let ang = dot.acos();
                    let max_turn = self.flight.trace_turn_deg_s.max(30.0).to_radians() * TICK_DT;
                    if ang > 1.0e-4 {
                        let frac = (max_turn / ang).min(1.0);
                        let rot = Quat::from_rotation_arc(dir, to_target);
                        dir = (Quat::IDENTITY.slerp(rot, frac) * dir).normalize_or_zero();
                    }
                }
            }
        }
        if in_boost {
            let drag = speed * self.flight.air_cp;
            speed = (speed + self.flight.accel - drag).max(0.0);
            if self.flight.max_speed > 0.0 {
                speed = speed.clamp(self.flight.min_speed, self.flight.max_speed);
            }
            self.velocity = dir * speed;
        } else if in_launch {
            self.velocity = dir * speed;
            self.velocity.y -= self.flight.before_accel_gravity;
        } else {
            let drag = (speed * self.flight.air_cp + self.flight.brake).min(speed);
            speed = (speed - drag).max(0.0);
            self.velocity = dir * speed;
            self.velocity.y -= self.flight.after_accel_gravity;
        }
        let delta = self.velocity * TICK_DT;
        self.distance += delta.length();
        Some((delta, just_ignited))
    }

    pub fn effective_damage(&self) -> (f32, f32) {
        let mult = self.decay.multiplier(self.distance);
        (self.damage_power * mult, self.damage_recoil * mult)
    }
}

/// Applies a melee blade swing hit window to a target AC.
pub fn blade_hits(
    ac: &SimAc,
    target_pos: Vec3,
    target_spheres: &[crate::damage::HitSphere],
) -> Option<Vec3> {
    let blade = ac.motion.blade.as_ref()?;
    if blade.phase != BladePhase::Swing {
        return None;
    }
    let forward = (Quat::from_rotation_y(ac.motion.yaw) * Vec3::NEG_Z).normalize_or(Vec3::NEG_Z);
    let origin = ac.position + Vec3::Y * 4.5 + forward * 2.0;
    let reach = blade.launch.hit_radius.clamp(6.0, 35.0);
    let sweep_r = blade.launch.blade_radius.max(2.0);
    let tip = origin + forward * reach;
    for sphere in target_spheres {
        let center = target_pos + Vec3::Y * sphere.offset_y;
        if let Some((_, p, _)) =
            crate::damage::swept_sphere_hits_sphere(origin, tip, sweep_r, center, sphere.radius)
        {
            return Some(p);
        }
    }
    None
}

/// Speed x weight scaling for the Boost Charge Kick (same params as `damage::SpeedWeightScale`).
pub fn kick_scaled(ac: &SimAc) -> (f32, f32) {
    let horiz_kmh = Vec3::new(ac.motion.velocity.x, 0.0, ac.motion.velocity.z).length()
        * TICK_RATE
        * KMH_PER_MS;
    crate::damage::SpeedWeightScale::kick().scale(
        ac.stats.ctrl.kick_power,
        ac.stats.ctrl.kick_recoil,
        horiz_kmh,
        ac.stats.ctrl.total_weight,
    )
}

/// Physical blade power/recoil scaling.
pub fn blade_scaled(ac: &SimAc, power: f32, recoil: f32, physical: bool) -> (f32, f32) {
    if !physical {
        return (power, recoil);
    }
    let horiz_kmh = Vec3::new(ac.motion.velocity.x, 0.0, ac.motion.velocity.z).length()
        * TICK_RATE
        * KMH_PER_MS;
    crate::damage::SpeedWeightScale::physical_blade().scale(
        power,
        recoil,
        horiz_kmh,
        ac.stats.ctrl.total_weight,
    )
}

fn silent() -> Gun {
    Gun {
        remaining: 0,
        magazine: 0,
        reload_time: 0.0,
        reload_stab_factor: 1.0,
        cooldown: 0.0,
        init_speed: 0.0,
        kind: Kind::Skip,
        flight: Flight::default(),
        missile_flight: None,
        blade: None,
        is_missile: false,
        weapon_lock_max: 0,
        burst_count: 1,
        burst_interval: 0.0,
        damage_type: DamageType::Ke,
        damage_power: 0.0,
        damage_recoil: 0.0,
        en_drain: 0.0,
        decay: RangeDecay::default(),
        fx: Fx::default(),
        shoot: 0,
        hit_se: HitSe::default(),
        stance: false,
        sniper: false,
        fire_mode: FireMode::Auto,
        charge_time: 0.0,
        charge_ticks: 0.0,
        burst_wait: 0.0,
        burst_remaining: 0,
        burst_timer: 0.0,
        trigger_held: false,
    }
}

fn gun(id: i64) -> Gun {
    gun_from_category(id, 10, 1)
}

fn shoulder_gun(id: i64, arms_mount: u8) -> Gun {
    gun_from_category(id, 12, arms_mount)
}

fn gun_from_category(id: i64, category: u8, arms_mount: u8) -> Gun {
    if id <= 0 {
        return silent();
    }
    if category == 12 && part_field(id, 12, "has_weapon_spec") == 0.0 {
        return silent();
    }
    let magazine = part_field(id, category, "magazine").max(0.0) as u16;
    let init_speed = part_field(id, category, "init_speed");
    let bullet_id = part_field(id, category, "bullet_id") as u32;
    let damage_type = DamageType::from_raw(part_field(id, category, "damage_type") as u8);
    let (kind, flight, missile_flight, blade, speed) =
        flight_or_blade(bullet_id, init_speed, damage_type, magazine);
    let hit_id = part_field(id, category, "hit_id") as u32;
    let (fx, hit_se) = fx_and_hit_se_for(hit_id);
    let is_missile = part_field(id, category, "is_missile") == 1.0 || kind == Kind::Missile;
    let base_burst = (part_field(id, category, "burst_count") as u8).max(1);
    let burst_count = if category == 12 {
        base_burst.saturating_mul(arms_mount.max(1))
    } else {
        base_burst
    };
    let lock_once = (part_field(id, category, "lock_once") as u8).max(1);
    let lock_use_burst = part_field(id, category, "lock_use_burst") != 0.0;
    let weapon_lock_max = if is_missile {
        if lock_use_burst {
            base_burst
        } else {
            lock_once
        }
    } else {
        0
    };
    let charge_time = if category == 10 {
        part_field(id, 10, "charge_time").max(0.0)
    } else {
        0.0
    };
    let burst_wait = if category == 10 {
        part_field(id, 10, "burst_wait").max(0.0)
    } else {
        0.0
    };
    let burst_interval = part_field(id, category, "burst_interval").max(0.0);
    let fire_mode = if category == 10 && charge_time > 0.0 {
        FireMode::Charge
    } else if category == 10
        && burst_count > 1
        && !is_missile
        && (burst_wait > 0.0 || burst_interval > 0.0)
    {
        FireMode::Burst
    } else if category == 10 && burst_count > 1 && !is_missile {
        FireMode::PelletSpread
    } else {
        FireMode::Auto
    };
    Gun {
        remaining: magazine,
        magazine,
        reload_time: part_field(id, category, "reload_time"),
        reload_stab_factor: if category == 11 {
            1.0
        } else {
            part_field(id, category, "reload_stab_factor")
        },
        cooldown: 0.0,
        init_speed: speed,
        kind,
        flight,
        missile_flight,
        blade,
        is_missile,
        weapon_lock_max,
        burst_count,
        burst_interval,
        damage_type,
        damage_power: part_field(id, category, "damage_power"),
        damage_recoil: part_field(id, category, "damage_recoil"),
        en_drain: part_field(id, category, "en_drain"),
        decay: RangeDecay {
            no_decay_range: part_field(id, category, "no_decay_range"),
            damp_range: part_field(id, category, "damp_range"),
            guarantee_rate: part_field(id, category, "guarantee_rate"),
            min_power_rate: part_field(id, category, "min_power_rate"),
        },
        fx,
        shoot: find(PARAM_ACWEAPONSOUNDPARAM_BIN, hit_id)
            .map(|r| r.data.shoot)
            .unwrap_or(DEFAULT_SHOOT),
        hit_se,
        stance: category == 10 && part_field(id, 10, "ready_position") != 0.0,
        sniper: category == 10 && part_field(id, 10, "weapon_kind") == 5.0,
        fire_mode,
        charge_time,
        charge_ticks: 0.0,
        burst_wait,
        burst_remaining: 0,
        burst_timer: 0.0,
        trigger_held: false,
    }
}

fn speed(init: f32, max_km_h: u16) -> f32 {
    if init > 0.0 {
        init
    } else {
        max_km_h as f32 / KMH_PER_MS
    }
}

fn flight_or_blade(
    id: u32,
    init_speed: f32,
    damage_type: DamageType,
    magazine: u16,
) -> (Kind, Flight, Option<MissileFlight>, Option<BladeSpec>, f32) {
    if let Some(row) = find(BULLET_BULLETBLADE_BIN, id) {
        let d = &row.data;
        let spec = BladeSpec {
            hit_radius: d.hit_radius,
            blade_radius: d.blade_radius,
            max_hits: d.damage_hit_count_chara.max(1),
            decay_power: d.decay_power,
            decay_recoil: d.decay_recoil,
            physical: damage_type != DamageType::Te,
            infinite_ammo: magazine == 0,
        };
        return (Kind::Blade, Flight::default(), None, Some(spec), 0.0);
    }
    let (kind, fl, mf, spd) = flight(id, init_speed);
    (kind, fl, mf, None, spd)
}

pub fn flight(id: u32, init_speed: f32) -> (Kind, Flight, Option<MissileFlight>, f32) {
    if let Some(row) = find(BULLET_BULLETMISSILE_BIN, id) {
        let d = &row.data;
        let mf = MissileFlight {
            accel: d.accel,
            air_cp: d.air_cp,
            before_accel_gravity: d.before_accel_gravity,
            after_accel_gravity: d.after_accel_gravity,
            brake: d.brake,
            accel_start_frame: d.accel_start_frame as f32,
            accel_end_frame: d.accel_end_frame as f32,
            act_start_frame: d.act_start_frame as f32,
            trace_turn_deg_s: d.trace_trun_angle,
            min_speed: d.min_speed_km_h as f32 / KMH_PER_MS,
            max_speed: d.max_speed_km_h as f32 / KMH_PER_MS,
            hit_radius: d.hit_radius,
            proximity_fuse_range: d.proximity_fuse_range,
            split: (d.close_act_id == 8 && d.shoot_bullet_id > 0 && d.once_shoot_count > 0)
                .then_some(MissileSplit {
                    force_trace_frame: d.force_trace_frame as f32,
                    start_frame: d.close_act_start_frame as f32,
                    start_dist: d.close_act_start_dist,
                    arise_id: d.shoot_bullet_id,
                    count: d.once_shoot_count,
                    shoot_speed: d.shoot_speed as f32,
                }),
        };
        return (
            Kind::Missile,
            Flight::default(),
            Some(mf),
            speed(init_speed, d.max_speed_km_h),
        );
    }
    if let Some(row) = find(BULLET_BULLETRIGID_BIN, id) {
        let d = &row.data;
        return (
            Kind::Rigid,
            Flight {
                min_speed: d.min_speed_km_h as f32 / KMH_PER_MS,
                max_speed: d.max_speed_km_h as f32 / KMH_PER_MS,
                air_cp: d.air_cp,
                stalled_air_cp_h: d.stalled_air_cp_h,
                stalled_air_cp_v: d.stalled_air_cp_v,
                gravity: d.gravity,
                brake: d.brake,
                stall_to_vanish_frame: None,
            },
            None,
            speed(init_speed, d.max_speed_km_h),
        );
    }
    if let Some(row) = find(BULLET_BULLETENERGY_BIN, id) {
        let d = &row.data;
        return (
            Kind::Energy,
            Flight {
                min_speed: d.min_speed_km_h as f32 / KMH_PER_MS,
                max_speed: d.max_speed_km_h as f32 / KMH_PER_MS,
                air_cp: d.air_cp,
                stalled_air_cp_h: d.stalled_air_cp,
                stalled_air_cp_v: 0.0,
                gravity: 0.0,
                brake: d.brake,
                stall_to_vanish_frame: Some(d.stall_to_vanish_frame as f32),
            },
            None,
            speed(init_speed, d.max_speed_km_h),
        );
    }
    (Kind::Skip, Flight::default(), None, init_speed)
}

fn fx_and_hit_se_for(hit_id: u32) -> (Fx, HitSe) {
    let Some(row) = find(PARAM_ACWEAPONSFXPARAM_BIN, hit_id)
        .or_else(|| find(PARAM_ACWEAPONSFXPARAM_BIN, 1))
        .map(|r| &r.data)
    else {
        return (Fx::default(), HitSe::default());
    };
    let hit_type = u32::try_from(row.hit_sfx_type).ok();
    let hit_sfx = hit_type
        .and_then(|t| find(PARAM_BULLETHITSFXPARAM_BIN, t))
        .map(|r| &r.data);
    let hit_se = hit_type
        .and_then(|t| find(PARAM_BULLETHITSEPARAM_BIN, t))
        .map(|r| HitSe {
            low: r.data.dry_ac_low_damage,
            hi: r.data.ac_hi_damage,
        })
        .unwrap_or_default();
    let fx = Fx {
        bullet: row.bullet_sfx_id.into(),
        muzzle: row.muzzle_sfx_id.into(),
        cartridge: row.cartridge_sfx_id.into(),
        hit: hit_sfx.map_or(0, |d| d.default2.into()),
        hit_low: hit_sfx.map_or(0, |d| d.dry_ac_low_damage.into()),
        hit_hi: hit_sfx.map_or(0, |d| d.ac_hi_damage.into()),
        after_accel: row.after_accel_follow_sfx_id.into(),
        delete_fx: row.delete_sfx_id.into(),
    };
    (fx, hit_se)
}

/// Resolves an OW secondary blast row; shared with loadout tooling/tests.
pub fn impact_blast_from_arise(arise_id: u32) -> Option<ImpactBlast> {
    if arise_id == 0 {
        return None;
    }
    let arise = &find(BULLET_BULLETARISE_BIN, arise_id)?.data;
    let radius = find(BULLET_BULLETEXPLOSION_BIN, arise.bullet_id)
        .map(|r| r.data.max_radius.max(r.data.init_radius))
        .or_else(|| find(BULLET_BULLETSTATUSERROR_BIN, arise.bullet_id).map(|r| r.data.hit_radius))
        .unwrap_or(0.0);
    if radius <= 0.0 {
        return None;
    }
    let sfx_id = u32::try_from(arise.sfx_type).unwrap_or(0);
    let (fx, hit_se) = if sfx_id > 0 {
        fx_and_hit_se_for(sfx_id)
    } else {
        (Fx::default(), HitSe::default())
    };
    Some(ImpactBlast {
        radius,
        damage_type: DamageType::from_raw(arise.damage_type),
        damage_power: arise.damage_power,
        damage_recoil: arise.damage_recoil,
        hit_fx: if fx.hit > 0 { fx.hit } else { fx.bullet },
        hit_se: if hit_se.hi > 0 { hit_se.hi } else { hit_se.low },
    })
}

/// Pose id used by the client for an active blade swing; kept here so the sim never names clips.
pub fn blade_pose() -> u8 {
    pose_state::BLADE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats;

    fn default_armament() -> Armament {
        let loadout = stats::default_loadout();
        let design = stats::design_from_loadout(&loadout);
        Armament::from_design(&design)
    }

    fn idle_input() -> InputFrame {
        InputFrame::default()
    }

    #[test]
    fn default_loadout_exposes_guns_and_a_left_hanger() {
        let arm = default_armament();
        assert!(!arm.hands[0].is_silent(), "right hand gun");
        assert!(
            !arm.bays[1].is_silent(),
            "left hanger gun (design 5010 part 811)"
        );
    }

    #[test]
    fn bay_swap_starts_on_input_and_flips_at_frame_150() {
        let mut arm = default_armament();
        let mut input = idle_input();
        input.bay_l = true;
        let mut en = 1000.0;
        let mut next = 1;
        let mut seq = 0;
        let out = arm.step_tick(
            &input,
            1,
            NetId(1),
            7,
            Vec3::ZERO,
            Vec3::NEG_Z,
            NetId::NONE,
            None,
            false,
            &mut en,
            &mut next,
            &mut seq,
        );
        assert!(out.swap_started[1]);
        assert_eq!(arm.swap_ticks[1], BAY_SWAP_TICKS);
        assert!(!arm.active_bay[1]);
        for tick in 2..=(1 + BAY_SWAP_TICKS as u32) {
            arm.step_tick(
                &idle_input(),
                tick,
                NetId(1),
                7,
                Vec3::ZERO,
                Vec3::NEG_Z,
                NetId::NONE,
                None,
                false,
                &mut en,
                &mut next,
                &mut seq,
            );
        }
        assert!(arm.active_bay[1]);
        assert!(!arm.swapping(1));
        assert_eq!(
            arm.combat_flags() & acvd_proto::combat_flags::LEFT_BAY_ACTIVE,
            acvd_proto::combat_flags::LEFT_BAY_ACTIVE
        );
    }

    #[test]
    fn shot_spread_is_pcg32_seeded_and_reproducible() {
        fn first_dir(match_seed: u64) -> Vec3 {
            let mut arm = default_armament();
            let mut input = idle_input();
            input.fire_r = true;
            let mut en = 1000.0;
            let mut next = 1;
            let mut seq = 0;
            let out = arm.step_tick(
                &input,
                10,
                NetId(3),
                match_seed,
                Vec3::ZERO,
                Vec3::NEG_Z,
                NetId::NONE,
                None,
                false,
                &mut en,
                &mut next,
                &mut seq,
            );
            assert_eq!(out.shots.len(), 1, "default rifle fires one round");
            out.shots[0].dir
        }
        let a = first_dir(1234);
        let b = first_dir(1234);
        let c = first_dir(1235);
        assert_eq!(
            a, b,
            "same (seed, tick, net_id, shot_seq) reproduces the spread"
        );
        assert_ne!(a, c, "a different match seed changes the spread roll");
        assert!((a - Vec3::NEG_Z).length() < 0.01);
    }

    #[test]
    fn fast_rounds_are_classified_for_rewind() {
        let mut arm = default_armament();
        let mut gun = arm.hands[0];
        gun.init_speed = FAST_PROJECTILE_MPS + 1.0;
        assert!(gun.is_hitscan());
        gun.init_speed = FAST_PROJECTILE_MPS - 1.0;
        assert!(!gun.is_hitscan());
    }
}
