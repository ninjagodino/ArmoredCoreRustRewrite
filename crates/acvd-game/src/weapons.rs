//! Hand-weapon fire: each arm slot reads its `acvparts.bin` firing fields
//! (`sheets/ac_part_fields.csv` category 10) and spawns a tracer that flies with the matching
//! `bullet/bulletrigid.bin` or `bullet/bulletenergy.bin` row. Blades and missiles are not fired
//! yet. Shots leave the weapon-root joint along `Pilot::aim_direction` (no upper-body aim).
//!
//! A hand part's `*_a.bnd.dcx` clips are `a00_000` stowed, `a00_001` deploy, `a00_002` fire and
//! `a00_003` stow (`ready_position` on that sheet). A weapon with `ready_position` and a deploy
//! clip stays stowed until fire is held, plays the deploy to its end before the first shot, and
//! plays the stow when fire is released. Any weapon with a fire clip restarts it on each shot.
//! The 360 call that starts these clips was not found; the flag and the clip names are the evidence.
//! The arm part's `a00_001` / `a00_003` play on that side while the weapon is deployed, but only
//! on the bones the clip actually moves (the shoulder pads). The other tracks are a two-key
//! identity hold; writing them would plant the arm on the bind and erase the body stance.
//! Every ready weapon also plays the body clips in
//! `sheets/ac_states.csv` (`sniper_ready_*`, then `sniper_stow_*`); their blend rows are named
//! キャノン構え / キャノン解除. A sniper rifle (`weapon_kind` 5, no `ready_position`) does not.
//! Holding fire raises that arm (`ArmAim`): `*_arm01` turns the barrel onto the aim, and it
//! lowers again about a second after fire is let go.
//! Every shot kicks the arm with the `param/jcondata.bin` control the 360 registers for that slot
//! (`0x8287f2d8`): `sniper_$(LR)` for a sniper rifle, `gun_$(LR)` for any other hand weapon.
//! Effects come from the `param/acweaponsfxparam.bin` row named by the part's `hit_id` (row 1
//! when it has none): the muzzle flash at the weapon's effect point 101, the bullet effect on
//! the shot, and the `param/bullethitsfxparam.bin` `default2` effect where it hits the ground
//! (collision triangles keep no material). The shot sound is the `acweaponsoundparam` row with
//! that same `hit_id` (`w%08d`); a missing row plays cue 299, and a shoot id ≤ 0 is silent.

use acvd_data::generated::ac_unit::{AcAssemblyDesignSt, PARAM_ACMOTION_BIN};
use acvd_data::generated::bullet::{
    BULLET_BULLETARISE_BIN, BULLET_BULLETBLADE_BIN, BULLET_BULLETENERGY_BIN,
    BULLET_BULLETEXPLOSION_BIN, BULLET_BULLETMISSILE_BIN, BULLET_BULLETRIGID_BIN,
    BULLET_BULLETSTATUSERROR_BIN, PARAM_BULLETHITSEPARAM_BIN, PARAM_BULLETHITSFXPARAM_BIN,
    PARAM_ENEMY_BULLET_ENEMYBULLETENERGY_BIN, PARAM_ENEMY_BULLET_ENEMYBULLETMISSILE_BIN,
    PARAM_ENEMY_BULLET_ENEMYBULLETRIGID_BIN,
};
use acvd_data::generated::ctrl::AcCtrlParam;
use acvd_data::generated::tuning::NEW_AC_BEHAVIOR;
use acvd_data::generated::sfx::{PARAM_ACWEAPONSFXPARAM_BIN, PARAM_ENEMYWEAPONSFXPARAM_BIN};
use acvd_data::generated::sound::{
    PARAM_ACSOUNDPARAM_BIN, PARAM_ACWEAPONSOUNDPARAM_BIN, PARAM_ENEMYWEAPONSOUNDPARAM_BIN,
};
use acvd_data::{ac_state, find, part_field};
use acvd_formats::reader::Be;
use acvd_formats::vfs::{self, Disc};
use acvd_formats::{acv_parts, ani, jcon};
use acvd_render::Rig;
use bevy::audio::AudioSource;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::assemble::Placement;
use crate::collision::{Collision, RayFilter, RAY_HALF};
use crate::control::{BladeLaunch, BladePhase, Held, Pilot, Piloting, ReadyCamera};
use crate::damage::{self, DamageType, PlayerHealth, SpeedWeightScale};
use crate::hud::Status;
use crate::pose::{self, Driven, Motion};
use crate::sfx::{EffectPoint, Sfx};
use crate::sound::{self, Cues};
use crate::targets::{Target, TargetHealth};

/// Hand-weapon muzzle effect point (FLVER dummy colour byte 1 on the arm weapons, 360 `0x8288f280`).
const MUZZLE_POINT: u8 = 101;
/// Hand-weapon cartridge ejection port dummy (`0x6f` = 111, registered in `0x8288f280` at `+0xfc`;
/// spawned by `0x829bee70` -> `0x82892df0` -> `0x82b88a78` at scale 1.0, `private/xenia/fx_polish.jsonl`).
const CARTRIDGE_POINT: u8 = 111;
/// Uniform scale the muzzle effect plays at on the 360 (`0x829c0160` -> `0x829bf848` ->
/// `0x829bfb48` -> `0x829bef38` -> `AcSfxCtrl` vtable `+0x08` = `0x82891f08` -> `0x82b88a78`).
/// `SfxRequest` (`0x82b88e88`, `DAT_82000fc4`) initializes `+0x30` scale = `1.0` and `0x82891f08`
/// never rescales it (verified `[r4+0x30]:f32 = 1.0` on all shots in `private/xenia/fx_polish.jsonl`
/// and `fx_polish2.jsonl`; f0001218's oversized 20 m card was action 107
/// `FXParticleAppearance_RadialBlur`, `0x835b3658`, not a world-space billboard).
const MUZZLE_SCALE: f32 = 1.0;

/// Game ticks per second (`DAT_82004408` = 1/60):
/// - `reload_time` (`+344`, `0x8247cb80`) is in 60 Hz ticks (`0x8286bd00` multiplies by `1/60`).
/// - `init_speed` (`+328`, `0x8247cbf8`) is in m/s (`0x8247dfc8`, `0x828a3294`, `0x829c0764`
///   multiply by `1/60` for m/tick).
/// - `BulletRigidSt.gravity` (`+0x2c`, `0x8247ce08`) is in m/s per 60 Hz tick (`0x8247dfc8` and
///   `0x828dd580` multiply by `1/60` for m/tick²).
const TICK_RATE: f32 = 60.0;
/// `BulletRigidSt` / `BulletEnergySt` `max_speed_km_h` (`+0x1a`) and `min_speed_km_h` (`+0x1c`)
/// are in km/h (`0x828dd460` / `0x828dd49c` multiply by `DAT_8200d118` = `1 / 216` = `1 / (3.6 * 60)`).
const KMH_PER_MS: f32 = 3.6;
/// Range beyond `damp_range` where damage decay reaches `min_power_rate`
/// (`0x820466cc` = 2000.0 m, passed by `0x8298be98` to `0x827ca448`).
const MAX_DECAY_RANGE: f32 = 2000.0;
/// Tracer lifetime in seconds; not game data.
const MAX_LIFE: f32 = 5.0;
/// Shoot id `FUN_82891e18` writes when the weapon has no `acweaponsoundparam` row.
const DEFAULT_SHOOT: i16 = 0x12B;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hand {
    Right,
    Left,
}

/// The two hand weapons, two bay weapons, shoulder unit, and optional Overed Weapon (`back_r`)
/// of the shown design.
/// `hands` / `bays` are `[right, left]`. When `weapon_arm` is true (`acvparts` cat-2 byte
/// `+0x13c == 1`, `0x824793c8`), `hands[0]` holds the unified built-in weapon-arm record
/// (`0x82479668` -> `0x8247e090`) and `hands[1]` / `bays` / `shoulder` are silent.
#[derive(Component)]
pub struct Armament {
    pub hands: [Gun; 2],
    pub bays: [Gun; 2],
    pub shoulder: Gun,
    /// Equipped Overed Weapon (`AC_ASSEMBLY_DESIGN_ST.back_r`, category 11, slot 12) and its
    /// live 360 state machine (`0x82885c40..0x82886948`).
    pub overed: Option<OveredWeaponState>,
    /// True when the equipped arms are weapon arms (`0x824793c8`).
    pub weapon_arm: bool,
    /// Queued shoulder salvo rounds (`Some(target)` for locked missiles, `None` for dumbfire/rockets).
    pub salvo: Vec<Option<Entity>>,
    /// Remaining 60 Hz ticks before the next missile/round leaves the shoulder pod.
    pub salvo_timer: f32,
    /// Counter within the current salvo for alternating left/right launch fan-out.
    pub salvo_index: u32,
    /// Queued missile/rocket salvo rounds for missile weapon arms (`am7210`/`7220`/`7230`).
    pub arm_salvo: Vec<Option<Entity>>,
    /// Remaining 60 Hz ticks before the next weapon-arm salvo round leaves the pod.
    pub arm_salvo_timer: f32,
    /// Counter within the current weapon-arm salvo for alternating right/left barrel muzzles.
    pub arm_salvo_index: u32,
}

/// Hand-weapon firing mode derived from `acvparts.bin` category-10 sub-record fields
/// (`0x823c7260`, `0x82475b00` / `0x8247d6e0` `charge_time` at `+0x3e`, `0x82475c98` `fire_mode`
/// at `+0x03`, `burst_count` at `+0x1d`, `burst_wait` at `+0x20`, `burst_interval` at `+0x22`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FireMode {
    /// Continuous automatic fire while trigger is held (rifles, gatlings, autocannons, etc.).
    #[default]
    Auto,
    /// Simultaneous multi-pellet cone spread per shot (shotguns, pulse guns/cannons, scatter cannons).
    PelletSpread,
    /// Multi-round sequential burst per trigger pull (burst rifles kind 6 `710..6520`, burst handguns).
    Burst,
    /// Hold trigger to charge up to `charge_time` ticks (`sub +0x3e`), release to fire
    /// (all 26 charge weapons: kind 11 laser rifles, kind 17 rail cannons, kind 22 laser cannons).
    Charge,
}

/// Result of stepping one weapon's trigger state for a frame (`Gun::step_trigger`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TriggerAction {
    None,
    /// Rising edge of a charge hold (plays `acweaponsoundparam.charge`).
    StartCharge,
    /// Fire `pellets` projectile(s) with damage/speed multiplier `power_scale`.
    Fire { power_scale: f32, pellets: usize },
}

/// Secondary AoE explosion blast spawned on projectile/missile impact (`BULLET_ARISE_ST` ->
/// `BULLET_EXPLOSION_ST` / `BULLET_STATUS_ERROR_ST`; e.g. Huge Cannon `99615` 50 m,
/// Multiple Pulse `99815` 30 m TE sphere, Giga Missile `101010` 100 m mega-blast).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImpactBlast {
    pub radius: f32,
    pub damage_type: DamageType,
    pub damage_power: f32,
    pub damage_recoil: f32,
    pub hit_fx: i32,
    pub hit_se: i16,
}

/// Static specification of an equipped Overed Weapon (`param/acvparts.bin` category 11, slot 12,
/// initialized by 360 `0x82858948` -> `0x82886418` -> `0x82885bf0`).
#[derive(Clone, Copy, Debug)]
pub struct OveredWeaponSpec {
    pub part_id: u16,
    /// Category-11 `+0x1a0`: `1` Grind Blade, `3` Mass Blade, `6` Huge Cannon,
    /// `8` Multiple Pulse, `10` Giga Missile, `60` Mega Blade.
    pub ow_type: u32,
    pub gun: Gun,
    /// Secondary AoE blast from category-11 `+0x144` (`arise_id`).
    pub blast: Option<ImpactBlast>,
    /// Target EN required for full charge (`AcCtrlParam::ow_charge_en`, `OwCtrl +0x08`).
    pub charge_en: f32,
    /// Generator OW EN/s output while active (`AcCtrlParam::ow_en_output`, `0x82868798` -> `0x8287ef68`).
    pub ow_en_output: f32,
    /// Post-fire reload cooldown in seconds (`AcCtrlParam::ow_reload`, `OwCtrl +0x0c`).
    pub ow_reload_secs: f32,
    /// Post-purge EN & FCS lockout duration in seconds (`cat-11 +0x28`, `OwCtrl +0x04`).
    pub lockout_secs: f32,
    /// Limit gauge passive drain per second (`owCalorificBaseVal * 60`, `OwCtrl +0x20`).
    pub calorific_per_sec: f32,
    /// Charge heat divisor (`heatValue_OwCharge = 500`, `OwCtrl +0x24` in `0x82885e08`).
    pub charge_heat_base: f32,
    /// Initial limit gauge capacity (`cat-11 +0xfe`, default `9000` in `0x82885bf0`, `OwCtrl +0x28`).
    pub limit_capacity: f32,
    /// AP self-damage per overload tick (`AcCtrlParam::ow_overload_dmg`, `0x828861d0`).
    pub overload_dmg: f32,
    /// Random self-damage interval bounds in seconds (`OwCtrl +0x14`/`+0x18`, `0x828861d0`).
    pub dmgtime_min: f32,
    pub dmgtime_max: f32,
    /// Activation hold duration in seconds (`NewAcBehavior` field 309 `ow_mode_hold_frames / 60`).
    pub activate_hold_secs: f32,
}

impl OveredWeaponSpec {
    pub fn is_melee(&self) -> bool {
        matches!(self.ow_type, 1 | 3 | 60)
    }

    pub fn from_design(design: &AcAssemblyDesignSt, ctrl: &AcCtrlParam) -> Option<Self> {
        let id = design.back_r as i64;
        if id <= 0 {
            return None;
        }
        let ow_type = part_field(id, 11, "ow_type") as u32;
        let gun = ow_gun(id);
        let arise_id = part_field(id, 11, "arise_id") as u32;
        let blast = impact_blast_from_arise(arise_id);
        let raw_cap = part_field(id, 11, "limit_capacity");
        let limit_capacity = if raw_cap > 0.0 { raw_cap } else { 9000.0 };
        let lockout_secs = part_field(id, 11, "lockout_secs").max(1.0);
        let charge_en = ctrl.ow_charge_en.max(1.0);
        let ow_en_output = if ctrl.ow_en_output > 0.0 {
            ctrl.ow_en_output
        } else {
            ctrl.en_output.max(10000.0)
        };
        Some(Self {
            part_id: design.back_r as u16,
            ow_type,
            gun,
            blast,
            charge_en,
            ow_en_output,
            ow_reload_secs: ctrl.ow_reload.max(0.5),
            lockout_secs,
            calorific_per_sec: (ctrl.ow_calorific_base * TICK_RATE).max(60.0),
            charge_heat_base: ctrl.ow_charge_heat_base.max(1.0),
            limit_capacity,
            overload_dmg: ctrl.ow_overload_dmg.max(0.0),
            dmgtime_min: ctrl.ow_dmgtime_min.max(0.5),
            dmgtime_max: ctrl.ow_dmgtime_max.max(ctrl.ow_dmgtime_min.max(0.5)),
            activate_hold_secs: (NEW_AC_BEHAVIOR.ow_mode_hold_frames / TICK_RATE).max(0.25),
        })
    }
}

/// 360 `OwCtrl` phase (`OwCtrl +0x44`, switched in `0x828865f8`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OwPhase {
    /// Phase 0: stowed on the core back pack; hold activate button to arm.
    #[default]
    Disarmed,
    /// Phase 1: deploying OW & purging/stowing the left-arm weapon.
    Arming,
    /// Phase 4: armed & overloading (`0x82885c40 == 1`); hold fire to charge.
    Armed,
    /// Phases 5/6: accumulating `charge_en` at `ow_en_output` (`0x82886468`).
    Charging,
    /// Phase 7: full charge reached (`0x82885d28`); release fire to unleash OW attack (`0x82886528`).
    Charged,
    /// Phase 9: executing OW attack & recoil/dash window (`0x828865f8` case 9).
    Attacking,
    /// Phases 2/3 + overload state 2 (`0x828861d0`): auto-purged, EN depleted & FCS disabled for `lockout_secs`.
    Lockout,
    /// Overload state 3 (`0x828861d0`): lockout expired, OW spent for the rest of the sortie.
    Spent,
}

/// Discrete event emitted by `OveredWeaponState::step`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwEvent {
    Armed,
    ChargeComplete,
    Fired,
    Purged,
    LockoutEnded,
}

/// Per-frame output of `OveredWeaponState::step`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct OwStepResult {
    pub event: Option<OwEvent>,
    /// Unmitigated overload AP self-damage triggered this step (`0x828861d0` slot `+0x44`).
    pub overload_damage: f32,
}

/// Live Overed Weapon controller matching 360 `OwCtrl` (`0x82885c40..0x82886948`).
#[derive(Clone, Copy, Debug)]
pub struct OveredWeaponState {
    pub spec: OveredWeaponSpec,
    pub phase: OwPhase,
    /// Seconds the activation button has been continuously held while `Disarmed`.
    pub activate_hold: f32,
    /// Remaining seconds in `Arming` or `Attacking`.
    pub phase_timer: f32,
    /// Accumulated charge EN (`OwCtrl +0x34`, `0..=spec.charge_en`).
    pub charge_en_cur: f32,
    /// Remaining reload cooldown after an attack (`OwCtrl +0x38`, `0x82886150`).
    pub reload_remaining: f32,
    /// Remaining limit gauge pool (`OwCtrl +0x3c`, `0..=spec.limit_capacity`, `0x828860a0`).
    pub limit_remaining: f32,
    /// Remaining seconds until the next overload AP self-damage tick (`OwCtrl +0x4c`, `0x828861d0`).
    pub dmg_timer: f32,
    /// Elapsed seconds in post-purge `Lockout` (`OwCtrl +0x50`, `0x828861d0` state 2).
    pub lockout_elapsed: f32,
    /// 360 LCG PRNG state (`OwCtrl +0x40`, `0x41c64e6d * s + 0x3039` in `0x828861d0`).
    rng_state: u32,
    /// Number of OW attacks fired this sortie.
    pub attacks_fired: u32,
}

impl OveredWeaponState {
    pub fn new(spec: OveredWeaponSpec) -> Self {
        Self {
            limit_remaining: spec.limit_capacity,
            spec,
            phase: OwPhase::Disarmed,
            activate_hold: 0.0,
            phase_timer: 0.0,
            charge_en_cur: 0.0,
            reload_remaining: 0.0,
            dmg_timer: 0.0,
            lockout_elapsed: 0.0,
            rng_state: 1,
            attacks_fired: 0,
        }
    }

    /// True while OW mode is active (`0x82885c40`: phases 1/4/5/6/7/9).
    pub fn is_active(&self) -> bool {
        matches!(
            self.phase,
            OwPhase::Arming
                | OwPhase::Armed
                | OwPhase::Charging
                | OwPhase::Charged
                | OwPhase::Attacking
        )
    }

    /// True when normal hand/shoulder fire is suppressed in favor of OW controls.
    pub fn blocks_normal_fire(&self) -> bool {
        self.is_active()
    }

    /// Charge gauge fraction in `0..=1` (`0x82885f10`).
    pub fn charge_fraction(&self) -> f32 {
        if self.spec.charge_en > 0.0 {
            (self.charge_en_cur / self.spec.charge_en).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Limit gauge fraction in `0..=1` (`0x82885fa8`).
    pub fn limit_fraction(&self) -> f32 {
        if self.spec.limit_capacity > 0.0 {
            (self.limit_remaining / self.spec.limit_capacity).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// 360 LCG roll (`0x828861d0`: `seed = seed * 0x41c64e6d + 0x3039`, `((seed >> 16) & 0x7fff) / 32767.0`).
    fn roll_dmg_interval(&mut self) -> f32 {
        self.rng_state = self.rng_state.wrapping_mul(0x41c6_4e6d).wrapping_add(0x3039);
        let u = ((self.rng_state >> 16) & 0x7fff) as f32 / 32767.0;
        self.spec.dmgtime_min + (self.spec.dmgtime_max - self.spec.dmgtime_min) * u
    }

    /// `0x82885da8`: decays charge EN by `0.8` (`0x83713e4c`) per 60 Hz frame, zeroing below `10.0` (`0x83713e48`).
    fn decay_charge(&mut self, dt: f32) {
        let frames = (dt * TICK_RATE).max(0.0);
        self.charge_en_cur *= 0.8_f32.powf(frames);
        if self.charge_en_cur < 10.0 {
            self.charge_en_cur = 0.0;
        }
    }

    /// Steps the OW state machine by `dt` seconds (`0x828868c0` -> `0x82886050`, `0x82886150`,
    /// `0x828865f8`, `0x828861d0`).
    pub fn step(&mut self, dt: f32, activate_held: bool, fire_held: bool) -> OwStepResult {
        let mut out = OwStepResult::default();
        if dt <= 0.0 {
            return out;
        }

        // `0x82886150`: tick reload cooldown while not in Attacking (phase 9).
        if self.phase != OwPhase::Attacking && self.reload_remaining > 0.0 {
            self.reload_remaining = (self.reload_remaining - dt).max(0.0);
        }

        match self.phase {
            OwPhase::Disarmed => {
                if activate_held {
                    self.activate_hold += dt;
                    if self.activate_hold >= self.spec.activate_hold_secs {
                        self.activate_hold = 0.0;
                        self.phase = OwPhase::Arming;
                        self.phase_timer = 0.45;
                        self.dmg_timer = self.roll_dmg_interval();
                    }
                } else {
                    self.activate_hold = 0.0;
                }
                self.decay_charge(dt);
            }
            OwPhase::Arming => {
                self.phase_timer -= dt;
                self.decay_charge(dt);
                if self.phase_timer <= 0.0 {
                    self.phase = OwPhase::Armed;
                    out.event = Some(OwEvent::Armed);
                }
            }
            OwPhase::Armed => {
                if self.limit_remaining <= 0.0 {
                    self.phase = OwPhase::Lockout;
                    self.lockout_elapsed = 0.0;
                    self.charge_en_cur = 0.0;
                    out.event = Some(OwEvent::Purged);
                } else if fire_held && self.reload_remaining <= 0.0 {
                    // `0x828865f8` case 5 -> 6 immediately calls `0x82886468` on the same tick.
                    self.phase = OwPhase::Charging;
                    let en_step = self.spec.ow_en_output * dt;
                    let extra_heat = en_step / self.spec.charge_heat_base;
                    self.limit_remaining = (self.limit_remaining - extra_heat).max(0.0);
                    self.charge_en_cur = (self.charge_en_cur + en_step).min(self.spec.charge_en);
                    if self.charge_en_cur >= self.spec.charge_en {
                        self.phase = OwPhase::Charged;
                        out.event = Some(OwEvent::ChargeComplete);
                    }
                } else {
                    self.decay_charge(dt);
                }
            }
            OwPhase::Charging => {
                // `0x82886468` + `0x82885e08`: charge accumulates by `ow_en_output * dt`,
                // draining extra limit gauge `en_step / charge_heat_base` (`(ow_en_output/60 / 500) * 60 * dt`).
                let en_step = self.spec.ow_en_output * dt;
                let extra_heat = en_step / self.spec.charge_heat_base;
                self.limit_remaining = (self.limit_remaining - extra_heat).max(0.0);
                if fire_held && self.limit_remaining > 0.0 && self.reload_remaining <= 0.0 {
                    self.charge_en_cur = (self.charge_en_cur + en_step).min(self.spec.charge_en);
                    if self.charge_en_cur >= self.spec.charge_en {
                        self.phase = OwPhase::Charged;
                        out.event = Some(OwEvent::ChargeComplete);
                    }
                } else {
                    // `0x828860f0`: early release or limit exhausted cancels charge.
                    self.decay_charge(dt);
                    self.phase = OwPhase::Armed;
                }
            }
            OwPhase::Charged => {
                // `0x82886528`: continues charge heat drain; releasing trigger fires OW!
                let en_step = self.spec.ow_en_output * dt;
                let extra_heat = en_step / self.spec.charge_heat_base;
                self.limit_remaining = (self.limit_remaining - extra_heat).max(0.0);
                if self.limit_remaining <= 0.0 {
                    self.decay_charge(dt);
                    self.phase = OwPhase::Armed;
                } else if !fire_held {
                    // Trigger released at full charge -> unleash OW attack (`0x82886594..0x828865c4`)!
                    self.charge_en_cur = 0.0;
                    self.reload_remaining = self.spec.ow_reload_secs;
                    self.phase = OwPhase::Attacking;
                    self.phase_timer =
                        (NEW_AC_BEHAVIOR.ow_attack_dash_frames as f32 / TICK_RATE).max(0.35);
                    self.attacks_fired += 1;
                    out.event = Some(OwEvent::Fired);
                }
            }
            OwPhase::Attacking => {
                self.phase_timer -= dt;
                if self.phase_timer <= 0.0 {
                    // Single-use ranged/massive OWs (`Huge Cannon`, `Giga Missile`) or depleted
                    // limit gauge purge right after the attack finishes; multi-hit/blade OWs
                    // can swing again while limit remains!
                    if matches!(self.spec.ow_type, 6 | 10) {
                        self.limit_remaining = 0.0;
                    }
                    self.phase = OwPhase::Armed;
                }
            }
            OwPhase::Lockout => {
                // `0x828861d0` state 2: count up to `lockout_secs` (`OwCtrl +0x04`).
                self.lockout_elapsed += dt;
                if self.lockout_elapsed >= self.spec.lockout_secs {
                    self.phase = OwPhase::Spent;
                    out.event = Some(OwEvent::LockoutEnded);
                }
            }
            OwPhase::Spent => {}
        }

        // `0x828861d0` state 1: while OW mode is active, drain limit gauge by `calorific_per_sec * dt`
        // and roll overload AP self-damage ticks in `[dmgtime_min, dmgtime_max]`.
        if self.is_active() {
            self.limit_remaining =
                (self.limit_remaining - self.spec.calorific_per_sec * dt).max(0.0);
            if self.dmg_timer <= 0.0 {
                out.overload_damage = self.spec.overload_dmg;
                self.dmg_timer = self.roll_dmg_interval();
            }
            self.dmg_timer = (self.dmg_timer - dt).max(0.0);
        }

        out
    }
}

/// Root joint of an equipped Overed Weapon (`back_r`, root `"50"`).
#[derive(Component)]
pub struct OwHardpoint;

/// Skinned mesh belonging to an equipped Overed Weapon (`back_r`), hidden on auto-purge.
#[derive(Component)]
pub struct OwMesh;

/// Dynamic re-attachment state for an Overed Weapon's secondary root `"60"` (`ow0110`, `ow0310`,
/// `ow0610`, `ow6010`): stowed under root `"50"` (`acattachinfo` record 0: `(12, 60, 12, 1, "60")`),
/// and dynamically re-attached onto arm dummy `60` (`acattachinfo` record 1: `(2, 60, 12, 0, "60")`)
/// during OW deployment so the weapon swings into firing position.
#[derive(Resource, Clone, Copy, Debug)]
pub struct OwArmAttach {
    pub root_60: Entity,
    pub stowed_parent: Entity,
    pub stowed_local: Transform,
    pub deployed_parent: Entity,
    pub deployed_local: Transform,
    pub deployed: bool,
}

impl OwArmAttach {
    /// Switches root `"60"` between stowed and arm-deployed mounting when `want_deployed`
    /// differs from `self.deployed`. Returns `true` if a reparent occurred.
    pub fn set_deployed(&mut self, commands: &mut Commands, want_deployed: bool) -> bool {
        if self.deployed == want_deployed {
            return false;
        }
        self.deployed = want_deployed;
        let (parent, local) = if want_deployed {
            (self.deployed_parent, self.deployed_local)
        } else {
            (self.stowed_parent, self.stowed_local)
        };
        commands.entity(self.root_60).insert((ChildOf(parent), local));
        true
    }
}

/// A bay shift in progress. While this exists the fire buttons do not shoot: they already
/// started the exchange (`sheets/ac_states.csv` `bay_shift_*` / `bay_purge_*`).
#[derive(Resource)]
pub(crate) struct Shift {
    pub hand: Hand,
    pub purge: bool,
    /// Body-clip frame where the equipped weapon is parented onto the rack.
    pub swap_at: f32,
    /// Body-clip frame where the hand, up at the shoulder again, takes the weapon that rode the spin.
    pub grab_at: f32,
    pub swapped: bool,
    pub grabbed: bool,
    /// 0 = stowed prong, 1 = swung forward (`a00_011` around frame 90).
    pub present: f32,
    /// The weapon that started on the rack. It stays there until `grab_at`.
    pub racked: Option<Entity>,
}

/// Range-decay curve packed by `0x8247d350` and evaluated by `0x8298be98` -> `0x827ca448`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RangeDecay {
    pub no_decay_range: f32,
    pub damp_range: f32,
    pub guarantee_rate: f32,
    pub min_power_rate: f32,
}

impl RangeDecay {
    /// Power/recoil multiplier at flight distance `dist` in metres (`0x8298be98` -> `0x827ca448`).
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

/// Ballistic parameters from `bullet/bulletrigid.bin` (`0x828dd528`) or
/// `bullet/bulletenergy.bin` (`0x828dd708`), stepped by `0x828dd2e0` -> `0x828dd218`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Flight {
    pub min_speed: f32,
    pub max_speed: f32,
    pub air_cp: f32,
    pub stalled_air_cp_h: f32,
    pub stalled_air_cp_v: f32,
    /// `BulletRigidSt.gravity`: m/s per 60 Hz tick (`0x8247dfc8`, `0x828dd580`).
    pub gravity: f32,
    /// `brake`: m/s per 60 Hz tick after `damp_range` (`0x828dd5bc`, `0x828dd758`).
    pub brake: f32,
    /// `BulletEnergySt.stall_to_vanish_frame` (`None` for rigid bullets).
    pub stall_to_vanish_frame: Option<f32>,
}

/// Sub-munition split specification from `BulletMissileSt` (`0x828ec1a0`, `0x828eaac0`,
/// `0x828e9f48`, `0x828de278` -> `0x828de0c8` -> `0x828dded8` -> `0x828ddca8`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MissileSplit {
    /// Minimum 60 Hz flight frames before `close_act` can trigger (`BulletMissileSt.force_trace_frame`, `0x828eaac0`).
    pub force_trace_frame: f32,
    /// Split trigger flight frame at 60 Hz (`BulletMissileSt.close_act_start_frame`; `<= 0` disables timer trigger).
    pub start_frame: f32,
    /// Split trigger target distance in m (`BulletMissileSt.close_act_start_dist`, `0x828eaac0`).
    pub start_dist: f32,
    /// `BULLET_BULLETARISE_BIN` row id (`BulletMissileSt.shoot_bullet_id`, `+0x7c`).
    pub arise_id: u32,
    /// Number of sub-munitions spawned (`BulletMissileSt.once_shoot_count`, `+0x80`).
    pub count: u8,
    /// Additional child launch speed in m/s (`BulletMissileSt.shoot_speed`, `+0x82`, `0x828ddca8`).
    pub shoot_speed: f32,
}

/// Missile flight parameters from `bullet/bulletmissile.bin` (`0x828ec898` -> `0x828e98c0`,
/// `0x828e9b50`, `0x828ea520`, `0x828eb9c8` -> `0x828eaf88`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MissileFlight {
    /// Booster acceleration in m/s per 60 Hz tick (`+0x28`, `0x828e98c0`).
    pub accel: f32,
    /// Aerodynamic drag coefficient (`+0x24`, `0x828e9b50`).
    pub air_cp: f32,
    /// Pre-ignition gravity in m/s per 60 Hz tick (`+0x8c`).
    pub before_accel_gravity: f32,
    /// Post-burnout gravity in m/s per 60 Hz tick (`+0x2c`, `0x828ec8e4`).
    pub after_accel_gravity: f32,
    /// Post-burnout brake in m/s per 60 Hz tick (`+0x88`, `0x828ec8f0`).
    pub brake: f32,
    /// Booster ignition start frame at 60 Hz (`+0x30`).
    pub accel_start_frame: f32,
    /// Booster burnout frame at 60 Hz (`+0x34`, `0x828eca04`).
    pub accel_end_frame: f32,
    /// Homing activation frame at 60 Hz (`+0x38`).
    pub act_start_frame: f32,
    /// Maximum homing turn rate in deg/s (`+0x48` `trace_trun_angle`).
    pub trace_turn_deg_s: f32,
    /// Minimum speed in m/s (`+0x86` `min_speed_km_h / 3.6`, `0x828eaf88`).
    pub min_speed: f32,
    /// Maximum speed in m/s (`+0x84` `max_speed_km_h / 3.6`, `0x828eaf88`).
    pub max_speed: f32,
    /// Missile collision radius in m (`+0x0c`).
    pub hit_radius: f32,
    /// Proximity fuse detonation distance in m (`+0x10`; 0 = direct impact only).
    pub proximity_fuse_range: f32,
    /// Sub-munition split specification (`close_act_id == 8 && shoot_bullet_id > 0 && once_shoot_count > 0`).
    pub split: Option<MissileSplit>,
}

/// Melee blade parameters from `bullet/bulletblade.bin` (`BulletBladeSt`, `0x828a4928`) and
/// `acvparts.bin` (`0x8247f170`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BladeSpec {
    /// Blade reach / hit length in m (`BulletBladeSt.hit_radius`).
    pub hit_radius: f32,
    /// Blade capsule thickness radius in m (`BulletBladeSt.blade_radius`).
    pub blade_radius: f32,
    /// Max targets struck per swing (`BulletBladeSt.damage_hit_count_chara`).
    pub max_hits: u8,
    /// Multi-target cleave power multiplier (`BulletBladeSt.decay_power`).
    pub decay_power: f32,
    /// Multi-target cleave recoil multiplier (`BulletBladeSt.decay_recoil`).
    pub decay_recoil: f32,
    /// True for KE physical blades & CE heat piles (`0x8247f170`: `damage_type != TE`), which
    /// scale power & recoil by speed × weight (`0x828a43e8`). False for TE laser blades.
    pub physical: bool,
    /// True when `magazine == 0` (infinite swings: laser blades & physical blades; Heat Piles use ammo).
    pub infinite_ammo: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Gun {
    pub remaining: u16,
    pub magazine: u16,
    pub reload_time: f32,
    /// Base lock-on time in 60 Hz ticks (`sub +0x06`, `0x8247cf48`, scaled by
    /// `AcCtrlParam::lock_effective_rate` / `missile_lock_effective_rate` `/ 60.0` in `0x82857688`).
    pub lock_time_ticks: f32,
    pub cooldown: f32,
    pub init_speed: f32,
    pub kind: Kind,
    pub flight: Flight,
    pub missile_flight: Option<MissileFlight>,
    pub blade: Option<BladeSpec>,
    /// True when `sub +0x00 == 1` (`0x82478850`): enables multi-lock missile acquisition.
    pub is_missile: bool,
    /// Weapon's own max simultaneous missile locks (`0x82478cd0`: `burst_count` if `lock_use_burst != 0`, else `lock_once.max(1)`).
    pub weapon_lock_max: u8,
    /// Rounds per burst/salvo (`0x82478980`, multiplied by `arms.shoulder_mount_count` for shoulder units in `0x82858638`).
    pub burst_count: u8,
    /// Interval between salvo rounds in 60 Hz ticks (`0x82478a48`).
    pub burst_interval: f32,
    pub damage_type: DamageType,
    pub damage_power: f32,
    pub damage_recoil: f32,
    /// Per-shot EN drain (`sub +0x18`, `0x8247cec0`, drained via `0x828a1820` ->
    /// `0x82830910`; 0 on kinetic weapons).
    pub en_drain: f32,
    pub decay: RangeDecay,
    pub fx: Fx,
    /// `acweaponsoundparam.shoot`. `≤ 0` plays nothing.
    pub shoot: i16,
    /// `bullethitseparam` ricochet / effective hit sound cue ids (`w%08d` in `acv2_se_bombs.fev`).
    pub hit_se: HitSe,
    /// Ready weapon (`ready_position`). Holding fire plays the body stance before the shot.
    /// A sniper rifle (`weapon_kind` 5) is not one of these.
    pub stance: bool,
    /// Sniper rifle (`weapon_kind` 5): shots kick with `sniper_$(LR)` instead of `gun_$(LR)`.
    pub sniper: bool,
    /// Ready-position zoom vertical FOV in degrees (`cat-10 +0x168`, `0x8247b550` -> `0x8285e418`).
    pub snipe_fov: f32,
    /// Ready-position aim turn speed multiplier (`cat-10 +0x16c`, `0x82478e00` -> `0x8287b658`).
    pub snipe_turn_scale: f32,
    /// Weapon reload stability interpolation factor (`sub +0x2c`, `0x82478b90`, used by `0x8286adf8`).
    pub reload_stab_factor: f32,
    /// Firing mode (`Auto`, `PelletSpread`, `Burst`, or `Charge`).
    pub fire_mode: FireMode,
    /// Charge hold duration in 60 Hz ticks (`sub +0x3e`, `0x8247d6e0` -> `0x8247ea28`; 0 = non-charge).
    pub charge_time: f32,
    /// Live charge accumulation in 60 Hz ticks (`0..=charge_time`).
    pub charge_ticks: f32,
    /// Charge sound cue id (`acweaponsoundparam.charge`; `<= 0` silent).
    pub charge_se: i16,
    /// Per-round sequential burst delay in 60 Hz ticks (`sub +0x20`, burst rifles `710..6520`).
    pub burst_wait: f32,
    /// Remaining rounds queued in the current sequential burst.
    pub burst_remaining: u8,
    /// Remaining 60 Hz ticks before the next sequential burst round fires.
    pub burst_timer: f32,
    /// Previous-frame trigger state for charge release edge detection.
    pub trigger_held: bool,
}

/// Exact 360 arm firing-stability & attack-balance reload multiplier (`0x8286adf8`, called by
/// `0x8286bd00` from `0x82858638` for every weapon slot):
/// - `balance = snipe_attack_balance` if `ready_position`, else `attack_balance` (`0x8286a0b0`).
/// - `t = (reload_stab_factor - 1.0) / (attack_stab_max_rate - 1.0)`.
/// - `factor = (1.0 - t) + t * balance`.
/// - When `!ready_position`, multiplies by `ctrl.arm_shoot_stability` (`AcCtrlParam +0x18c`).
pub fn reload_multiplier(
    ctrl: &AcCtrlParam,
    reload_stab_factor: f32,
    ready_position: bool,
) -> f32 {
    let balance = if ready_position {
        ctrl.snipe_attack_balance
    } else {
        ctrl.attack_balance
    };
    let denom = ctrl.attack_stab_max_rate - 1.0;
    let mut factor = if denom.abs() > 1.0e-6 {
        let t = (reload_stab_factor - 1.0) / denom;
        (1.0 - t) + t * balance
    } else {
        1.0
    };
    if !ready_position {
        factor *= ctrl.arm_shoot_stability;
    }
    factor.max(0.05)
}

impl Gun {
    /// Applies the 360 firing-stability & attack-balance reload multiplier (`0x8286bd00` / `0x8286adf8`).
    pub fn apply_reload_stability(&mut self, ctrl: &AcCtrlParam) {
        if self.kind != Kind::Skip && self.reload_time > 0.0 {
            self.reload_time *= reload_multiplier(ctrl, self.reload_stab_factor, self.stance);
        }
    }

    /// Effective `(damage_power, damage_recoil)` at flight distance `dist` in metres
    /// (`0x827cf1c0` -> `0x827cee08` / `0x827cee18`).
    #[cfg(test)]
    pub fn damage_at(&self, dist: f32) -> (f32, f32) {
        let mult = self.decay.multiplier(dist);
        (self.damage_power * mult, self.damage_recoil * mult)
    }

    /// Charge progress fraction in `0..=1` for charge weapons (`sub +0x3e > 0`).
    pub fn charge_fraction(&self) -> f32 {
        if self.charge_time > 0.0 {
            (self.charge_ticks / self.charge_time).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Steps this hand weapon's firing mode (`Auto`, `PelletSpread`, `Burst`, `Charge`) by
    /// `frames` (60 Hz ticks) and returns whether to start charging or fire this frame.
    pub fn step_trigger(
        &mut self,
        want: bool,
        can: bool,
        stance_ready: bool,
        frames: f32,
    ) -> TriggerAction {
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
                // Active sequential burst finishes even if the trigger is released mid-burst.
                if self.burst_remaining > 0 {
                    self.burst_timer = (self.burst_timer - frames).max(0.0);
                    if self.burst_timer <= 0.0 && self.remaining > 0 {
                        self.burst_remaining -= 1;
                        let step_delay = self.burst_wait.max(self.burst_interval).max(2.0);
                        self.burst_timer = step_delay;
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
                // `0x82475b00` / `0x8247d6e0` -> `0x8247ea28`: hold to charge up to `charge_time`,
                // release to fire (partial charge scales 0.5..1.0, full charge = 1.0).
                if want && can && stance_ready {
                    let started = self.charge_ticks <= 0.0;
                    self.charge_ticks = (self.charge_ticks + frames).min(self.charge_time.max(1.0));
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
}

/// FFX effect ids of a weapon; 0 plays nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct Fx {
    pub bullet: i32,
    pub muzzle: i32,
    /// `acweaponsfxparam.cartridge_sfx_id` (`+0x04`, spawned by `0x829bee70` -> `0x82892df0` on
    /// dummy `111` at scale `1.0`, `private/xenia/fx_polish.jsonl`).
    pub cartridge: i32,
    pub hit: i32,
    /// `bullethitsfxparam.dry_ac_low_damage` (ricochet hit VFX).
    pub hit_low: i32,
    /// `bullethitsfxparam.ac_hi_damage` (effective hit VFX).
    pub hit_hi: i32,
    /// `acweaponsfxparam.after_accel_follow_sfx_id` (missile booster trail after ignition).
    pub after_accel: i32,
    /// `acweaponsfxparam.delete_sfx_id` (missile detonation fallback VFX).
    pub delete_fx: i32,
}

/// Target hit sound cue ids (`param/bullethitseparam.bin`, `acv2_se_bombs.fev` `w%08d`).
#[derive(Clone, Copy, Debug, Default)]
pub struct HitSe {
    pub low: i16,
    pub hi: i16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Rigid,
    Energy,
    Missile,
    Blade,
    Skip,
}

/// Weapon-root joint the shot leaves from.
#[derive(Component)]
pub struct Hardpoint {
    pub hand: Hand,
}

/// Shoulder-unit root joint (`core` dummy 50) that shoulder missiles/rockets launch from.
#[derive(Component)]
pub struct ShoulderHardpoint;

/// A weapon-clip joint. Body-driven joints keep the locomotion pose.
#[derive(Component)]
pub struct ClipJoint;

/// Weapon-clip players for the shown AC. Replaced when the design changes.
#[derive(Resource, Default)]
pub struct WeaponAnims {
    rigs: Vec<WeaponRig>,
    /// Arm `a00` clips, posed over locomotion while the weapon on that side is deployed.
    arms: Vec<WeaponRig>,
    /// Weapon-arm `am*_a.bnd.dcx` player (`0x824793c8`, `0x82882d28`, `0x8284cecc`), driving all
    /// non-root bones (`*_armwepunit_*`, `*_armroot`, `*_arm01..*_armhand`) for stowed, deploy,
    /// combat-idle, ready, fire, and stow states.
    weapon_arm: Option<WeaponArmRig>,
    recoil_bones: Vec<RecoilBone>,
    /// Right, left.
    aim: [ArmAim; 2],
    sniper: SniperStance,
}

/// Raise blend of the weapon-arm aim: `f1` of every `0x8287f448` call from `0x8289e468`.
const AIM_RAISE: f32 = 10.0;
/// Lower blend: `f1` of the per-frame release `0x82853660` -> `0x8287f5a8` on slots 0x0a/0x0b.
const AIM_LOWER: f32 = 60.0;
/// Frames the aim stays requested after fire is let go. The probe shows `AimArm` re-sent each
/// frame from the trigger press until about 1 s after the last shot; the timer is not traced.
const AIM_HOLD: f32 = 60.0;

/// The weapon-arm aim (AC vtable `0x82044fc0` +0x90 `0x8284a570`, sent every frame from the
/// weapon method `0x828a2488` while firing, weight 1.0). It turns `*_arm01` so the barrel
/// points along the pilot's aim, within the joint's LockMin/MaxX. Probe file
/// `private/xenia/armraise.txt`.
#[derive(Clone, Copy, Debug, Default)]
struct ArmAim {
    shoulder: Option<Entity>,
    limit: f32,
    /// The hand weapon is a sniper rifle, so the `sniper_$(LR)` bone carries the turn.
    sniper: bool,
    hold: f32,
    weight: f32,
    frame: Option<AimFrame>,
}

/// Last frame's world rotation of the shoulder's parent, the barrel in shoulder space, and the
/// aim direction.
#[derive(Clone, Copy, Debug)]
struct AimFrame {
    parent: Quat,
    barrel: Vec3,
    aim: Vec3,
}

/// Frames the arm takes to settle after a kick: the `f2` the AC's shot handler `0x82847f18`
/// passes to the kick start (`0x8210a498` = 30.0), applied when the kick ends (`0x82beeb30`).
const KICK_RETURN: f32 = 30.0;

struct RecoilBone {
    entity: Entity,
    hand: Hand,
    /// From a `sniper_$(LR)` control rather than `gun_$(LR)`.
    sniper: bool,
    kick: Kick,
    /// Rotation before the kick, and the kicked rotation last written. Joints the clip does
    /// not key keep the written value, so the kick must not stack on it.
    base: Quat,
    written: Option<Quat>,
}

/// One `jcondata.bin` object's shot reaction, stepped like the 360 per-object update
/// `0x82beea90`. A shot (`0x82bee808`, weight 1.0 from `0x82cf8110`) restarts the clock. After
/// ReactDelay frames the weight ramps 0 → 1 over ReactTime frames, then drops to 0; the solver
/// `0x82cb4d98` turns ReactAng × weight into the extra rotation. The joint eases toward that
/// target over ReactTime frames while kicking and `KICK_RETURN` frames after
/// (`0x82c42048` with ReactTime, then 30): read as a linear blend from where the joint was.
#[derive(Clone, Copy, Debug)]
struct Kick {
    angle: f32,
    delay: f32,
    time: f32,
    clock: Option<f32>,
    ramping: bool,
    shown: f32,
    from: f32,
    blend: f32,
    progress: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum SniperPhase {
    #[default]
    Off,
    Engage,
    Hold,
    Release,
}

struct SniperStance {
    phase: SniperPhase,
    hand: Hand,
}

impl Default for SniperStance {
    fn default() -> Self {
        Self {
            phase: SniperPhase::Off,
            hand: Hand::Right,
        }
    }
}

/// Raw `param/enemy/parts/acvenemyparts.bin` loaded once from the 360 disc (`0x824a14e4`).
#[derive(Resource, Default, Clone)]
pub struct EnemyParts {
    pub bytes: Vec<u8>,
}

impl EnemyParts {
    pub fn load(disc: &Disc) -> Self {
        let bytes = disc
            .read("param/enemy/parts/acvenemyparts.bin")
            .unwrap_or_default();
        Self { bytes }
    }

    /// Resolves one enemy weapon (`EnemyWeaponSt.weapon_id`, category 10 in `acvenemyparts.bin`)
    /// into a `Gun` with real `enemybullet*`, `enemyweaponsfxparam`, and `enemyweaponsoundparam`
    /// stats.
    pub fn weapon(&self, weapon_id: u16) -> Gun {
        if weapon_id == 0 || self.bytes.is_empty() {
            return silent();
        }
        let Ok(parsed) = acv_parts::read(&self.bytes, acv_parts::record_size) else {
            return silent();
        };
        let Some(rec) = parsed
            .records
            .iter()
            .find(|r| r.category == 10 && r.id == weapon_id)
        else {
            return silent();
        };
        enemy_gun_from_record(rec.data)
    }
}

/// Which side fired a `Projectile` or `Missile`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Owner {
    #[default]
    Player,
    Enemy,
}

#[derive(Component, Clone, Copy, Debug)]
pub struct Projectile {
    pub owner: Owner,
    velocity: Vec3,
    flight: Flight,
    decay: RangeDecay,
    damage_type: DamageType,
    damage_power: f32,
    damage_recoil: f32,
    distance: f32,
    stall_drop: f32,
    stall_frames: f32,
    life: f32,
    hit: i32,
    hit_low: i32,
    hit_hi: i32,
    hit_se: HitSe,
    pub blast: Option<ImpactBlast>,
}

impl Projectile {
    /// Creates a player projectile fired from `gun` along `aim` (`0x828a3138` -> `0x829c0160`).
    pub fn from_gun(gun: &Gun, aim: Vec3) -> Self {
        Self::from_gun_owned(gun, aim, Owner::Player)
    }

    /// Creates a projectile fired by `owner` from `gun` along `aim`.
    pub fn from_gun_owned(gun: &Gun, aim: Vec3, owner: Owner) -> Self {
        Self {
            owner,
            velocity: aim * gun.init_speed.max(1.0),
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
        }
    }

    pub fn with_power_scale(mut self, scale: f32) -> Self {
        let s = scale.clamp(0.1, 2.0);
        self.damage_power *= s;
        self.damage_recoil *= s;
        self
    }

    pub fn with_blast(mut self, blast: Option<ImpactBlast>) -> Self {
        self.blast = blast;
        self
    }

    /// Steps bullet velocity and returns the world displacement `delta` for `dt` seconds,
    /// or `None` when the round vanishes (`0x828dd528`, `0x828dd708`, `0x828dd2e0` -> `0x828dd218`).
    pub fn step(&mut self, dt: f32) -> Option<Vec3> {
        self.life -= dt;
        if self.life <= 0.0 {
            return None;
        }
        let frames = dt * TICK_RATE;
        // `0x8298bdf8`: compares distance traveled (`bullet + 0x2a0`) against `damp_range` (`bullet + 0x294`).
        let stalled = self.distance > self.decay.damp_range;
        let (cp, brake) = if stalled {
            (self.flight.stalled_air_cp_h, self.flight.brake)
        } else {
            (self.flight.air_cp, 0.0)
        };
        // `0x828dd218`: drag `(speed * cp + brake)` along velocity, gravity along -Y, speed clamp.
        let speed = self.velocity.length();
        if speed > 0.0 {
            let dir = self.velocity / speed;
            let drag = (speed * cp + brake) * frames;
            self.velocity -= dir * drag.min(speed);
        }
        self.velocity.y -= self.flight.gravity * frames;
        let new_speed = self.velocity.length();
        if new_speed > 0.0 && self.flight.max_speed > 0.0 {
            let clamped = new_speed.clamp(self.flight.min_speed, self.flight.max_speed);
            self.velocity *= clamped / new_speed;
        }
        if stalled {
            if self.flight.gravity > 0.0 || self.flight.stalled_air_cp_v > 0.0 {
                // `0x828dd528`: extra vertical drop velocity (`controller + 0x0c`) after `damp_range`.
                self.stall_drop = (self.stall_drop + self.flight.gravity * frames)
                    - self.stall_drop * self.flight.stalled_air_cp_v * frames;
            }
            if let Some(vanish) = self.flight.stall_to_vanish_frame {
                // `0x828dd708`: energy bullet stall timer (`controller + 0x08`).
                self.stall_frames += frames;
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
        let delta = step_vel * dt;
        self.distance += delta.length();
        Some(delta)
    }

    /// Effective `(damage_power, damage_recoil)` at the bullet's current flight distance
    /// (`0x827cf1c0` -> `0x827cee08` / `0x827cee18`).
    pub fn effective_damage(&self) -> (f32, f32) {
        let mult = self.decay.multiplier(self.distance);
        (self.damage_power * mult, self.damage_recoil * mult)
    }
}

/// Homing missile projectile (`BULLET_BULLETMISSILE_BIN`, `0x828ec898` -> `0x828e98c0`,
/// `0x828e9b50`, `0x828ea520`, `0x828eb9c8` -> `0x828eaf88`).
#[derive(Component, Clone, Copy, Debug)]
pub struct Missile {
    pub owner: Owner,
    pub velocity: Vec3,
    pub flight: MissileFlight,
    pub decay: RangeDecay,
    pub damage_type: DamageType,
    pub damage_power: f32,
    pub damage_recoil: f32,
    pub target: Option<Entity>,
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
    /// Recursion depth for sub-munition splitting (`0` for parent, `1` for spawned children).
    pub split_depth: u8,
    pub blast: Option<ImpactBlast>,
}

/// One sub-munition spawned when a split missile executes `close_act_id == 8` (`0x828de0c8`).
#[derive(Clone, Copy, Debug)]
pub enum SplitChild {
    Missile(Missile),
    Projectile {
        projectile: Projectile,
        kind: Kind,
        bullet_fx: i32,
    },
}

/// Complete sub-munition burst generated by `Missile::build_split_burst`.
#[derive(Clone, Debug)]
pub struct SplitBurst {
    pub children: Vec<(Vec3, SplitChild)>,
    pub split_fx: i32,
    pub split_se: i16,
}

impl Missile {
    /// Creates a player homing missile launched from `gun` along `launch_dir` toward `target`.
    pub fn from_gun(gun: &Gun, launch_dir: Vec3, target: Option<Entity>) -> Self {
        Self::from_gun_owned(gun, launch_dir, target, Owner::Player)
    }

    /// Creates a homing missile owned by `owner`.
    pub fn from_gun_owned(
        gun: &Gun,
        launch_dir: Vec3,
        target: Option<Entity>,
        owner: Owner,
    ) -> Self {
        let flight = gun.missile_flight.unwrap_or_default();
        let dir = launch_dir.normalize_or_zero();
        let dir = if dir == Vec3::ZERO { Vec3::NEG_Z } else { dir };
        let init_speed = gun.init_speed.max(flight.min_speed).max(10.0);
        let max_life = ((flight.accel_end_frame + 120.0) / TICK_RATE).clamp(5.0, 14.0);
        Self {
            owner,
            velocity: dir * init_speed,
            flight,
            decay: gun.decay,
            damage_type: gun.damage_type,
            damage_power: gun.damage_power,
            damage_recoil: gun.damage_recoil,
            target,
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
        }
    }

    pub fn with_blast(mut self, blast: Option<ImpactBlast>) -> Self {
        self.blast = blast;
        self
    }

    /// Returns `Some(split)` when `close_act_id == 8` triggers (`0x828eaac0`):
    /// 1. `age_frames >= force_trace_frame` (when `force_trace_frame > 0`).
    /// 2. Either `start_frame > 0 && age_frames >= start_frame`, OR `target_pos` is within
    ///    `start_dist` metres.
    pub fn should_split(&self, pos: Vec3, target_pos: Option<Vec3>) -> Option<MissileSplit> {
        if self.split_depth >= 2 {
            return None;
        }
        let split = self.flight.split?;
        if split.count == 0 || split.arise_id == 0 {
            return None;
        }
        if split.force_trace_frame > 0.0 && self.age_frames < split.force_trace_frame {
            return None;
        }
        let by_frame = split.start_frame > 0.0 && self.age_frames >= split.start_frame;
        let by_dist = split.start_dist > 0.0
            && target_pos
                .is_some_and(|t| pos.distance_squared(t) < split.start_dist * split.start_dist);
        if by_frame || by_dist {
            Some(split)
        } else {
            None
        }
    }

    /// Builds the `once_shoot_count` sub-munition children from `BULLET_BULLETARISE_BIN`
    /// (`0x828de0c8` -> `0x828dded8` -> `0x828ddca8`).
    pub fn build_split_burst(
        &self,
        pos: Vec3,
        target_pos: Option<Vec3>,
        split: MissileSplit,
    ) -> SplitBurst {
        let Some(arise_row) = find(BULLET_BULLETARISE_BIN, split.arise_id) else {
            return SplitBurst {
                children: Vec::new(),
                split_fx: self.delete_fx,
                split_se: self.hit_se.low,
            };
        };
        let arise = &arise_row.data;
        let count = split.count.max(1) as usize;
        let (kind, child_flight, child_mf, child_spd) = match self.owner {
            Owner::Player => flight(arise.bullet_id, split.shoot_speed),
            Owner::Enemy => enemy_flight(arise.bullet_id, split.shoot_speed),
        };
        let sfx_id = u32::try_from(arise.sfx_type).unwrap_or(0);
        let (mut child_fx, mut child_hit_se) = match self.owner {
            Owner::Player => fx_and_hit_se_for(sfx_id),
            Owner::Enemy => enemy_fx_and_hit_se_for(sfx_id),
        };
        if arise.hit_effect_id > 0 {
            if let Some(h) = find(PARAM_BULLETHITSFXPARAM_BIN, arise.hit_effect_id) {
                child_fx.hit = h.data.default2.into();
                child_fx.hit_low = h.data.dry_ac_low_damage.into();
                child_fx.hit_hi = h.data.ac_hi_damage.into();
            }
            if let Some(s) = find(PARAM_BULLETHITSEPARAM_BIN, arise.hit_effect_id) {
                child_hit_se.low = s.data.dry_ac_low_damage;
                child_hit_se.hi = s.data.ac_hi_damage;
            }
        }
        let (damage_type, damage_power, damage_recoil) = if arise.damage_power > 0.0 {
            (
                DamageType::from_raw(arise.damage_type),
                arise.damage_power,
                arise.damage_recoil,
            )
        } else {
            (
                self.damage_type,
                self.damage_power / count as f32,
                self.damage_recoil / count as f32,
            )
        };
        let fwd = target_pos
            .map(|t| (t - pos).normalize_or_zero())
            .filter(|&d| d != Vec3::ZERO)
            .unwrap_or_else(|| self.velocity.normalize_or(Vec3::NEG_Z));
        let up_hint = if fwd.y.abs() > 0.98 { Vec3::X } else { Vec3::Y };
        let right = fwd.cross(up_hint).normalize_or(Vec3::X);
        let up = right.cross(fwd).normalize_or(Vec3::Y);
        let cone = if count <= 4 {
            20.0_f32.to_radians()
        } else {
            28.0_f32.to_radians()
        };

        let mut children = Vec::with_capacity(count);
        for k in 0..count {
            let fan_dir = if count == 1 {
                fwd
            } else {
                let az = std::f32::consts::TAU * (k as f32) / (count as f32) + 0.35;
                let radial = right * az.cos() + up * az.sin();
                (fwd * cone.cos() + radial * cone.sin()).normalize_or(fwd)
            };
            let spawn_pos = pos + fan_dir * 1.2;
            match kind {
                Kind::Missile => {
                    let mf = child_mf.unwrap_or_default();
                    // `0x828ddca8`: child_vel = parent_vel + launch_dir * shoot_speed.
                    let vel = if split.shoot_speed > 0.0 {
                        self.velocity + fan_dir * split.shoot_speed
                    } else {
                        fan_dir * self.velocity.length().max(mf.min_speed).max(40.0)
                    };
                    let max_life = ((mf.accel_end_frame + 120.0) / TICK_RATE).clamp(5.0, 14.0);
                    children.push((
                        spawn_pos,
                        SplitChild::Missile(Missile {
                            owner: self.owner,
                            velocity: vel,
                            flight: mf,
                            decay: self.decay,
                            damage_type,
                            damage_power,
                            damage_recoil,
                            target: self.target,
                            age_frames: 0.0,
                            ignited: false,
                            distance: 0.0,
                            life: max_life,
                            bullet_fx: child_fx.bullet,
                            hit: child_fx.hit,
                            hit_low: child_fx.hit_low,
                            hit_hi: child_fx.hit_hi,
                            delete_fx: child_fx.delete_fx,
                            after_accel_fx: child_fx.after_accel,
                            hit_se: child_hit_se,
                            split_depth: self.split_depth + 1,
                            blast: None,
                        }),
                    ));
                }
                Kind::Rigid | Kind::Energy => {
                    let vel = if split.shoot_speed > 0.0 {
                        self.velocity + fan_dir * split.shoot_speed
                    } else {
                        fan_dir * child_spd.max(self.velocity.length()).max(40.0)
                    };
                    children.push((
                        spawn_pos,
                        SplitChild::Projectile {
                            projectile: Projectile {
                                owner: self.owner,
                                velocity: vel,
                                flight: child_flight,
                                decay: self.decay,
                                damage_type,
                                damage_power,
                                damage_recoil,
                                distance: 0.0,
                                stall_drop: 0.0,
                                stall_frames: 0.0,
                                life: MAX_LIFE,
                                hit: child_fx.hit,
                                hit_low: child_fx.hit_low,
                                hit_hi: child_fx.hit_hi,
                                hit_se: child_hit_se,
                                blast: None,
                            },
                            kind,
                            bullet_fx: child_fx.bullet,
                        },
                    ));
                }
                _ => {}
            }
        }
        let split_fx = if self.delete_fx > 0 {
            self.delete_fx
        } else if child_fx.muzzle > 0 {
            child_fx.muzzle
        } else {
            self.hit
        };
        let split_se = match self.owner {
            Owner::Player => find(PARAM_ACWEAPONSOUNDPARAM_BIN, sfx_id)
                .map(|r| r.data.shoot)
                .filter(|&s| s > 0)
                .unwrap_or(self.hit_se.low),
            Owner::Enemy => find(PARAM_ENEMYWEAPONSOUNDPARAM_BIN, sfx_id)
                .map(|r| r.data.shoot)
                .filter(|&s| s > 0)
                .unwrap_or(self.hit_se.low),
        };
        SplitBurst {
            children,
            split_fx,
            split_se,
        }
    }

    /// Steps missile flight & homing for `dt` seconds (`0x828ec898` -> `0x828eaf88`).
    /// Returns `Some((world_delta, just_ignited))` or `None` when expired.
    pub fn step(&mut self, pos: Vec3, target_pos: Option<Vec3>, dt: f32) -> Option<(Vec3, bool)> {
        self.life -= dt;
        if self.life <= 0.0 {
            return None;
        }
        let frames = dt * TICK_RATE;
        self.age_frames += frames;

        let in_launch = self.age_frames < self.flight.accel_start_frame;
        let in_boost = self.age_frames >= self.flight.accel_start_frame
            && self.age_frames <= self.flight.accel_end_frame;
        let just_ignited = in_boost && !self.ignited;
        if just_ignited {
            self.ignited = true;
        }

        // Homing steering (`0x828ec300` -> `0x828ea520`): active from `act_start_frame` through `accel_end_frame`.
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
                let to_target = (tpos - pos).normalize_or_zero();
                if to_target != Vec3::ZERO {
                    let dot = dir.dot(to_target).clamp(-1.0, 1.0);
                    let ang = dot.acos();
                    let max_turn = self.flight.trace_turn_deg_s.max(30.0).to_radians() * dt;
                    if ang > 1.0e-4 {
                        let frac = (max_turn / ang).min(1.0);
                        let rot = Quat::from_rotation_arc(dir, to_target);
                        dir = (Quat::IDENTITY.slerp(rot, frac) * dir).normalize_or_zero();
                    }
                }
            }
        }

        // Speed & gravity integration (`0x828e98c0`, `0x828e9b50`, `0x828eaf88`).
        if in_boost {
            let drag = speed * self.flight.air_cp * frames;
            speed = (speed + self.flight.accel * frames - drag).max(0.0);
            if self.flight.max_speed > 0.0 {
                speed = speed.clamp(self.flight.min_speed, self.flight.max_speed);
            }
            self.velocity = dir * speed;
        } else if in_launch {
            self.velocity = dir * speed;
            self.velocity.y -= self.flight.before_accel_gravity * frames;
        } else {
            // Post-burnout coast (`0x828ec8dc..0x828ec8f8`): brake + after_accel_gravity.
            let drag = (speed * self.flight.air_cp + self.flight.brake) * frames;
            speed = (speed - drag).max(0.0);
            self.velocity = dir * speed;
            self.velocity.y -= self.flight.after_accel_gravity * frames;
        }

        let delta = self.velocity * dt;
        self.distance += delta.length();
        Some((delta, just_ignited))
    }

    pub fn effective_damage(&self) -> (f32, f32) {
        let mult = self.decay.multiplier(self.distance);
        (self.damage_power * mult, self.damage_recoil * mult)
    }
}

#[derive(Resource)]
pub(crate) struct Tracers {
    mesh: Handle<Mesh>,
    rigid: Handle<StandardMaterial>,
    energy: Handle<StandardMaterial>,
    missile: Handle<StandardMaterial>,
    enemy_rigid: Handle<StandardMaterial>,
    enemy_energy: Handle<StandardMaterial>,
    enemy_missile: Handle<StandardMaterial>,
}

impl Armament {
    pub fn from_design(design: &AcAssemblyDesignSt) -> Self {
        let ctrl = AcCtrlParam::calculate(design);
        let overed = OveredWeaponSpec::from_design(design, &ctrl).map(OveredWeaponState::new);
        let weapon_arm = part_field(design.arms as i64, 2, "is_weapon_arm") != 0.0;
        if weapon_arm {
            // `0x824793c8` -> `0x82479668` -> `0x8247e090`: weapon arms embed a single unified
            // category-10 weapon record (9020..9620) and disable hand/hanger/shoulder slots.
            let wid = part_field(design.arms as i64, 2, "weapon_arm_id") as i64;
            let mut wa = gun(wid);
            wa.apply_reload_stability(&ctrl);
            return Self {
                hands: [wa, silent()],
                bays: [silent(), silent()],
                shoulder: silent(),
                overed,
                weapon_arm: true,
                salvo: Vec::new(),
                salvo_timer: 0.0,
                salvo_index: 0,
                arm_salvo: Vec::new(),
                arm_salvo_timer: 0.0,
                arm_salvo_index: 0,
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
            overed,
            weapon_arm: false,
            salvo: Vec::new(),
            salvo_timer: 0.0,
            salvo_index: 0,
            arm_salvo: Vec::new(),
            arm_salvo_timer: 0.0,
            arm_salvo_index: 0,
        }
    }
}

pub fn hardpoint(placement: &Placement) -> Option<Hardpoint> {
    Some(Hardpoint {
        hand: match placement.column {
            "armwep_r" => Hand::Right,
            "armwep_l" => Hand::Left,
            _ => return None,
        },
    })
}

pub fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(Tracers {
        mesh: meshes.add(Cuboid::new(0.2, 0.2, 1.6)),
        rigid: materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.85, 0.35),
            emissive: LinearRgba::rgb(8.0, 5.0, 1.0),
            unlit: true,
            ..default()
        }),
        energy: materials.add(StandardMaterial {
            base_color: Color::srgb(0.4, 0.85, 1.0),
            emissive: LinearRgba::rgb(1.0, 4.0, 10.0),
            unlit: true,
            ..default()
        }),
        missile: materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.55, 0.15),
            emissive: LinearRgba::rgb(10.0, 4.0, 0.6),
            unlit: true,
            ..default()
        }),
        enemy_rigid: materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.35, 0.15),
            emissive: LinearRgba::rgb(12.0, 3.0, 0.8),
            unlit: true,
            ..default()
        }),
        enemy_energy: materials.add(StandardMaterial {
            base_color: Color::srgb(0.45, 0.95, 1.0),
            emissive: LinearRgba::rgb(1.5, 6.0, 12.0),
            unlit: true,
            ..default()
        }),
        enemy_missile: materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.25, 0.25),
            emissive: LinearRgba::rgb(12.0, 2.5, 1.0),
            unlit: true,
            ..default()
        }),
    });
}

/// Spawns one enemy projectile or homing missile (`Owner::Enemy`) from `origin` along `aim`,
/// with muzzle flash FFX, bullet FFX + glowing tracer mesh, and sound cue.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_enemy_shot(
    commands: &mut Commands,
    assets: &Tracers,
    cues: &mut Cues,
    sources: &mut Assets<AudioSource>,
    gun: &Gun,
    origin: Vec3,
    aim: Vec3,
    target_entity: Option<Entity>,
) {
    let dir = aim.normalize_or_zero();
    if dir == Vec3::ZERO || gun.kind == Kind::Skip {
        return;
    }
    if gun.shoot > 0 {
        sound::shot(commands, cues, sources, gun.shoot);
    }
    if gun.fx.muzzle > 0 {
        commands.spawn((
            Sfx::new(gun.fx.muzzle),
            Transform::from_translation(origin)
                .looking_to(-dir, Vec3::Y)
                .with_scale(Vec3::splat(MUZZLE_SCALE)),
        ));
    }
    if gun.fx.cartridge > 0 {
        commands.spawn((
            Sfx::new(gun.fx.cartridge),
            Transform::from_translation(origin).looking_to(-dir, Vec3::Y),
        ));
    }
    let tf = Transform::from_translation(origin).looking_to(dir, Vec3::Y);
    if gun.kind == Kind::Missile {
        let missile = Missile::from_gun_owned(gun, dir, target_entity, Owner::Enemy);
        if gun.fx.bullet > 0 {
            commands
                .spawn((
                    tf,
                    missile,
                    Visibility::default(),
                    Mesh3d(assets.mesh.clone()),
                    MeshMaterial3d(assets.enemy_missile.clone()),
                ))
                .with_child((Sfx::new(gun.fx.bullet), Transform::default()));
        } else {
            commands.spawn((
                Mesh3d(assets.mesh.clone()),
                MeshMaterial3d(assets.enemy_missile.clone()),
                tf,
                missile,
            ));
        }
    } else {
        let shot = Projectile::from_gun_owned(gun, dir, Owner::Enemy);
        let material = match gun.kind {
            Kind::Energy => assets.enemy_energy.clone(),
            _ => assets.enemy_rigid.clone(),
        };
        if gun.fx.bullet > 0 {
            commands
                .spawn((
                    tf,
                    shot,
                    Visibility::default(),
                    Mesh3d(assets.mesh.clone()),
                    MeshMaterial3d(material),
                ))
                .with_child((Sfx::new(gun.fx.bullet), Transform::default()));
        } else {
            commands.spawn((Mesh3d(assets.mesh.clone()), MeshMaterial3d(material), tf, shot));
        }
    }
}

/// One system parameter, so `fire` stays inside Bevy's 16-parameter limit.
#[derive(SystemParam)]
pub(crate) struct Speaker<'w> {
    cues: ResMut<'w, Cues>,
    sources: ResMut<'w, Assets<AudioSource>>,
}

/// The body clip and the weapon players. Grouped so `fire` stays within Bevy's parameter limit.
#[derive(SystemParam)]
pub(crate) struct Body<'w> {
    anims: Option<ResMut<'w, WeaponAnims>>,
    garage: Res<'w, crate::Garage>,
    motion: Option<ResMut<'w, Motion>>,
    shift: Option<Res<'w, Shift>>,
}

/// World transforms of the weapon roots and of any joint (last frame's propagation).
#[derive(SystemParam)]
pub(crate) struct Rigging<'w, 's> {
    hardpoints: Query<'w, 's, (&'static Hardpoint, &'static GlobalTransform)>,
    shoulder: Query<'w, 's, &'static GlobalTransform, With<ShoulderHardpoint>>,
    globals: Query<'w, 's, (&'static GlobalTransform, Option<&'static ChildOf>)>,
}

impl Rigging<'_, '_> {
    fn aim_frame(
        &self,
        shoulder: Entity,
        hand: Hand,
        muzzle: Option<Entity>,
        aim: Vec3,
    ) -> Option<AimFrame> {
        let (joint, child_of) = self.globals.get(shoulder).ok()?;
        let (parent, _) = self.globals.get(child_of?.parent()).ok()?;
        let (_, root) = self.hardpoints.iter().find(|(h, _)| h.hand == hand)?;
        let barrel = muzzle
            .and_then(|m| self.globals.get(m).ok())
            .map(|(m, _)| m.translation() - root.translation())
            .filter(|b| b.length_squared() > 1.0e-4)
            .unwrap_or_else(|| root.forward().into());
        Some(AimFrame {
            parent: parent.rotation(),
            barrel: joint.rotation().inverse() * barrel.normalize(),
            aim,
        })
    }
}

pub fn fire(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    held: Res<Held>,
    piloting: Res<Piloting>,
    assets: Res<Tracers>,
    pads: Query<&Gamepad>,
    mut speaker: Speaker,
    mut commands: Commands,
    mut acs: Query<
        (
            &mut Pilot,
            &mut Armament,
            &Transform,
            Option<&mut Status>,
            Option<&mut crate::fcs::Fcs>,
            Option<&PlayerHealth>,
        ),
        Without<ClipJoint>,
    >,
    rigging: Rigging,
    points: Query<(Entity, &EffectPoint, &GlobalTransform)>,
    mut body: Body,
    mut posed: Query<(&mut Transform, Option<&Driven>), With<ClipJoint>>,
) {
    if !piloting.0 {
        return;
    }
    let dt = time.delta_secs();
    let Ok((mut pilot, mut arms, ac_tf, mut status, mut fcs, player_health)) = acs.single_mut()
    else {
        return;
    };
    if player_health.is_some_and(|h| h.destroyed) {
        return;
    }
    let down = |k: KeyCode| keys.pressed(k) || held.0.contains(&k);
    let mut want = [
        down(KeyCode::KeyF) || mouse.pressed(MouseButton::Left),
        down(KeyCode::KeyC) || mouse.pressed(MouseButton::Right),
    ];
    let mut want_shoulder = down(KeyCode::KeyG) || down(KeyCode::KeyX) || down(KeyCode::Tab);
    // Manual (lang/en/text/menu/manual.fmg): R2 right arm weapon, L2 left, R1 shoulder weapon.
    for pad in &pads {
        want[0] |= pad.pressed(GamepadButton::RightTrigger2);
        want[1] |= pad.pressed(GamepadButton::LeftTrigger2);
        want_shoulder |= pad.pressed(GamepadButton::RightTrigger);
    }
    // Bay shift (manual 20403): while the modifier is held, the fire buttons exchange that
    // arm with its bay weapon instead of shooting. The clip keeps the block after release.
    // Scan Mode (`AC + 0xdb0 != 0`, `0x828b51e0` -> `0x82879d58(_, 0)`, `manual.fmg` 20407):
    // weapon fire is disabled while in Scan Mode (R2 becomes Target Scan).
    // Kick / Blade active (`AcActRestrictionParamSt` rows 2, 3, 9: `b_shoot == 0`): block fire.
    let shifting = body.shift.is_some()
        || keys.pressed(KeyCode::KeyR)
        || held.0.contains(&KeyCode::KeyR)
        || pads.iter().any(|pad| pad.pressed(GamepadButton::North));
    let ow_blocks = arms
        .overed
        .as_ref()
        .is_some_and(OveredWeaponState::blocks_normal_fire);
    if shifting || pilot.scan_mode || pilot.in_kick() || pilot.in_blade() || ow_blocks {
        want = [false, false];
        want_shoulder = false;
    }
    let aim = pilot.aim_direction();
    let frames = dt * TICK_RATE;
    let sniper_hand = (0..2).find(|&i| arms.hands[i].stance && want[i]).map(|i| {
        if i == 0 {
            Hand::Right
        } else {
            Hand::Left
        }
    });
    let disc = body.garage.disc.clone();
    if body.shift.is_none() {
        if let Some(motion) = body.motion.as_mut() {
            if let Some(anims) = body.anims.as_mut() {
                anims.sniper.update(sniper_hand, motion, &disc, &mut *pilot);
            }
        }
    }
    // Ready-position camera stance (`0x8285db68` row 40 -> 49 + `0x8285e418` zoom FOV override).
    pilot.ready_cam = if let Some(hand) = sniper_hand {
        let gun = &arms.hands[usize::from(hand == Hand::Left)];
        let deployed = body
            .anims
            .as_ref()
            .is_none_or(|a| a.sniper.allows(hand));
        if deployed && gun.snipe_fov > 0.0 {
            ReadyCamera::Zoomed {
                fov_deg: gun.snipe_fov,
                turn_scale: gun.snipe_turn_scale,
            }
        } else {
            ReadyCamera::Deploying
        }
    } else if arms.weapon_arm && arms.hands[0].stance && (want[0] || want[1]) {
        let gun = &arms.hands[0];
        if gun.snipe_fov > 0.0 {
            ReadyCamera::Zoomed {
                fov_deg: gun.snipe_fov,
                turn_scale: gun.snipe_turn_scale,
            }
        } else {
            ReadyCamera::Deploying
        }
    } else {
        ReadyCamera::Off
    };
    if arms.weapon_arm {
        let want_wa = want[0] || want[1];
        let mut wa_fired = false;
        // Collect weapon-arm muzzle effect points (even IDs 102/104/106/108 = right arm,
        // odd IDs 101/103/105/107 = left arm), interleaved right-then-left.
        let mut right_muzzles: Vec<(u8, Entity, Vec3)> = Vec::new();
        let mut left_muzzles: Vec<(u8, Entity, Vec3)> = Vec::new();
        for (ent, pt, gt) in points.iter() {
            if pt.column == "arms" && (101..=112).contains(&pt.id) {
                if pt.id % 2 == 0 {
                    right_muzzles.push((pt.id, ent, gt.translation()));
                } else {
                    left_muzzles.push((pt.id, ent, gt.translation()));
                }
            }
        }
        right_muzzles.sort_by_key(|m| m.0);
        left_muzzles.sort_by_key(|m| m.0);
        let mut muzzles: Vec<(Entity, Vec3)> = Vec::new();
        let max_len = right_muzzles.len().max(left_muzzles.len());
        for k in 0..max_len {
            if let Some(&(_, e, p)) = right_muzzles.get(k) {
                muzzles.push((e, p));
            }
            if let Some(&(_, e, p)) = left_muzzles.get(k) {
                muzzles.push((e, p));
            }
        }
        let fallback_origin = rigging
            .hardpoints
            .iter()
            .next()
            .map_or(ac_tf.translation + Vec3::Y * 4.5, |(_, t)| t.translation());

        arms.hands[0].cooldown = (arms.hands[0].cooldown - frames).max(0.0);
        if arms.hands[0].kind == Kind::Blade {
            let gun = &mut arms.hands[0];
            if let Some(blade) = gun.blade {
                let has_ammo = blade.infinite_ammo || gun.remaining > 0;
                if want_wa
                    && has_ammo
                    && gun.cooldown <= 0.0
                    && !pilot.in_blade()
                    && !pilot.in_kick()
                    && pilot.en >= gun.en_drain
                {
                    let target = fcs
                        .as_deref()
                        .and_then(|f| f.locked.as_ref())
                        .map(|l| (l.entity, l.world_pos));
                    let launch = BladeLaunch {
                        hand_idx: 0,
                        physical: blade.physical,
                        target,
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
                    if pilot.start_blade(launch) {
                        wa_fired = true;
                        let _ = pilot.try_drain_en(gun.en_drain);
                        if !blade.infinite_ammo {
                            gun.remaining = gun.remaining.saturating_sub(1);
                        }
                        gun.cooldown = gun.reload_time.max(1.0);
                        if let Some(status) = status.as_deref_mut() {
                            status.en = pilot.en;
                            status.en_max = pilot.en_max;
                        }
                        sound::shot(
                            &mut commands,
                            &mut speaker.cues,
                            &mut speaker.sources,
                            gun.shoot,
                        );
                    }
                }
            }
        } else if arms.hands[0].kind == Kind::Missile {
            if arms.arm_salvo.is_empty()
                && want_wa
                && arms.hands[0].remaining > 0
                && arms.hands[0].cooldown <= 0.0
                && pilot.en >= arms.hands[0].en_drain
            {
                let burst = arms.hands[0].burst_count.max(1) as usize;
                let rem = arms.hands[0].remaining as usize;
                if arms.hands[0].is_missile {
                    let locks = fcs
                        .as_deref_mut()
                        .map_or_else(Vec::new, crate::fcs::Fcs::take_missile_locks);
                    if locks.is_empty() {
                        for _ in 0..burst.min(rem) {
                            arms.arm_salvo.push(None);
                        }
                    } else {
                        for target in locks {
                            for _ in 0..burst {
                                if arms.arm_salvo.len() >= rem {
                                    break;
                                }
                                arms.arm_salvo.push(Some(target));
                            }
                        }
                    }
                } else {
                    // Rocket / dumbfire weapon arm (e.g. am7230 wep 9230: burst_count=2, lock_once=8).
                    let salvo_count = (burst * arms.hands[0].weapon_lock_max.max(1) as usize).min(rem);
                    for _ in 0..salvo_count {
                        arms.arm_salvo.push(None);
                    }
                }
                if !arms.arm_salvo.is_empty() {
                    wa_fired = true;
                    arms.arm_salvo_timer = 0.0;
                    arms.arm_salvo_index = 0;
                    arms.hands[0].cooldown = arms.hands[0].reload_time.max(1.0);
                }
            }
            if !arms.arm_salvo.is_empty() {
                arms.arm_salvo_timer = (arms.arm_salvo_timer - frames).max(0.0);
                if arms.arm_salvo_timer <= 0.0 {
                    let target_opt = arms.arm_salvo.remove(0);
                    if arms.hands[0].remaining > 0 && pilot.try_drain_en(arms.hands[0].en_drain) {
                        if let Some(status) = status.as_deref_mut() {
                            status.en = pilot.en;
                            status.en_max = pilot.en_max;
                        }
                        let (muzzle_ent, origin) = if muzzles.is_empty() {
                            (None, fallback_origin)
                        } else {
                            let (e, p) = muzzles[(arms.arm_salvo_index as usize) % muzzles.len()];
                            (Some(e), p)
                        };
                        arms.hands[0].remaining = arms.hands[0].remaining.saturating_sub(1);
                        sound::shot(
                            &mut commands,
                            &mut speaker.cues,
                            &mut speaker.sources,
                            arms.hands[0].shoot,
                        );
                        if arms.hands[0].fx.muzzle > 0 {
                            if let Some(point) = muzzle_ent {
                                commands.spawn((
                                    Sfx::new(arms.hands[0].fx.muzzle),
                                    Transform::from_scale(Vec3::splat(MUZZLE_SCALE)),
                                    Visibility::default(),
                                    ChildOf(point),
                                ));
                            } else {
                                commands.spawn((
                                    Sfx::new(arms.hands[0].fx.muzzle),
                                    Transform::from_translation(origin)
                                        .looking_to(-aim, Vec3::Y)
                                        .with_scale(Vec3::splat(MUZZLE_SCALE)),
                                ));
                            }
                        }
                        let wa_aim = if target_opt.is_none() {
                            fcs.as_deref()
                                .and_then(|f| f.locked.as_ref())
                                .map(|l| (l.world_pos - origin).normalize_or_zero())
                                .filter(|&d| d != Vec3::ZERO)
                                .unwrap_or(aim)
                        } else {
                            aim
                        };
                        let right = wa_aim.cross(Vec3::Y).normalize_or_zero();
                        let side = if arms.arm_salvo_index % 2 == 0 { 1.0 } else { -1.0 };
                        let launch_dir = if target_opt.is_some() {
                            (wa_aim + Vec3::Y * 0.25 + right * (0.22 * side)).normalize_or(wa_aim)
                        } else {
                            wa_aim
                        };
                        arms.arm_salvo_index = arms.arm_salvo_index.wrapping_add(1);
                        let missile = Missile::from_gun(&arms.hands[0], launch_dir, target_opt);
                        let tf = Transform::from_translation(origin).looking_to(launch_dir, Vec3::Y);
                        if arms.hands[0].fx.bullet > 0 {
                            commands
                                .spawn((
                                    tf,
                                    missile,
                                    Visibility::default(),
                                    Mesh3d(assets.mesh.clone()),
                                    MeshMaterial3d(assets.missile.clone()),
                                ))
                                .with_child((Sfx::new(arms.hands[0].fx.bullet), Transform::default()));
                        } else {
                            commands.spawn((
                                Mesh3d(assets.mesh.clone()),
                                MeshMaterial3d(assets.missile.clone()),
                                tf,
                                missile,
                            ));
                        }
                    }
                    arms.arm_salvo_timer = arms.hands[0].burst_interval.max(3.0);
                }
            }
        } else {
            let gun = &mut arms.hands[0];
            let can = gun.kind != Kind::Skip
                && gun.remaining > 0
                && gun.cooldown <= 0.0
                && pilot.en >= gun.en_drain;
            if want_wa && can && pilot.try_drain_en(gun.en_drain) {
                wa_fired = true;
                if gun.stance {
                    pilot.trigger_impact_shake(220.0);
                    pilot.trigger_rumble_shake(0.14, Vec2::new(0.06, 0.12), Vec2::new(0.02, 0.02));
                }
                if let Some(anims) = body.anims.as_mut() {
                    anims.kick(Hand::Right, gun.sniper);
                    anims.kick(Hand::Left, gun.sniper);
                }
                if let Some(status) = status.as_deref_mut() {
                    status.en = pilot.en;
                    status.en_max = pilot.en_max;
                }
                let shots = (gun.burst_count.max(1) as usize).min(gun.remaining as usize);
                gun.remaining = gun.remaining.saturating_sub(shots as u16);
                gun.cooldown = gun.reload_time.max(1.0);
                sound::shot(
                    &mut commands,
                    &mut speaker.cues,
                    &mut speaker.sources,
                    gun.shoot,
                );
                // Spawn muzzle flash once per active barrel (up to `shots`).
                let barrel_count = muzzles.len().min(shots);
                if gun.fx.muzzle > 0 {
                    if barrel_count > 0 {
                        for &(point, _) in muzzles.iter().take(barrel_count) {
                            commands.spawn((
                                Sfx::new(gun.fx.muzzle),
                                Transform::from_scale(Vec3::splat(MUZZLE_SCALE)),
                                Visibility::default(),
                                ChildOf(point),
                            ));
                        }
                    } else {
                        commands.spawn((
                            Sfx::new(gun.fx.muzzle),
                            Transform::from_translation(fallback_origin)
                                .looking_to(-aim, Vec3::Y)
                                .with_scale(Vec3::splat(MUZZLE_SCALE)),
                        ));
                    }
                }
                if gun.fx.cartridge > 0 {
                    let carts: Vec<Entity> = points
                        .iter()
                        .filter(|(_, p, _)| {
                            (p.column == "arms" || p.column == "armwep_r" || p.column == "armwep_l")
                                && p.id == CARTRIDGE_POINT
                        })
                        .map(|(e, _, _)| e)
                        .collect();
                    if !carts.is_empty() {
                        for &point in carts.iter().take(shots) {
                            commands.spawn((
                                Sfx::new(gun.fx.cartridge),
                                Transform::default(),
                                Visibility::default(),
                                ChildOf(point),
                            ));
                        }
                    } else if barrel_count > 0 {
                        for &(point, _) in muzzles.iter().take(barrel_count) {
                            commands.spawn((
                                Sfx::new(gun.fx.cartridge),
                                Transform::default(),
                                Visibility::default(),
                                ChildOf(point),
                            ));
                        }
                    } else {
                        commands.spawn((
                            Sfx::new(gun.fx.cartridge),
                            Transform::from_translation(fallback_origin).looking_to(-aim, Vec3::Y),
                        ));
                    }
                }
                let pellets_per_barrel = if muzzles.is_empty() {
                    shots
                } else {
                    shots.div_ceil(muzzles.len())
                };
                for k in 0..shots {
                    let origin = if muzzles.is_empty() {
                        fallback_origin
                    } else {
                        muzzles[k % muzzles.len()].1
                    };
                    let base_aim = fcs
                        .as_deref()
                        .map_or(aim, |f| f.aim_for_hand(0, origin, gun.init_speed, aim));
                    let shot_aim = if pellets_per_barrel > 1 {
                        // Scatter/shotgun weapon arm (e.g. am7410/7420): slight deterministic pellet cone.
                        let right = base_aim.cross(Vec3::Y).normalize_or(Vec3::X);
                        let up = right.cross(base_aim).normalize_or(Vec3::Y);
                        let az = std::f32::consts::TAU * (k as f32) / (shots as f32);
                        let spread = 0.035;
                        (base_aim + (right * az.cos() + up * az.sin()) * spread)
                            .normalize_or(base_aim)
                    } else {
                        base_aim
                    };
                    let shot = (
                        Transform::from_translation(origin).looking_to(shot_aim, Vec3::Y),
                        Projectile::from_gun(gun, shot_aim),
                    );
                    if gun.fx.bullet > 0 {
                        commands
                            .spawn((shot, Visibility::default()))
                            .with_child((Sfx::new(gun.fx.bullet), Transform::default()));
                    } else {
                        let material = match gun.kind {
                            Kind::Energy => assets.energy.clone(),
                            _ => assets.rigid.clone(),
                        };
                        commands.spawn((
                            Mesh3d(assets.mesh.clone()),
                            MeshMaterial3d(material),
                            shot,
                        ));
                    }
                }
            }
        }
        if let Some(anims) = body.anims.as_mut() {
            if let Some(wa) = anims.weapon_arm.as_mut() {
                wa.tick(!pilot.scan_mode, want_wa, wa_fired, frames);
                wa.apply(&mut posed);
            }
            for (i, hand) in [Hand::Right, Hand::Left].into_iter().enumerate() {
                let muzzle = if hand == Hand::Right {
                    right_muzzles.first().or_else(|| left_muzzles.first()).map(|m| m.1)
                } else {
                    left_muzzles.first().or_else(|| right_muzzles.first()).map(|m| m.1)
                };
                let origin = rigging
                    .hardpoints
                    .iter()
                    .find(|(h, _)| h.hand == hand)
                    .map_or(fallback_origin, |(_, t)| t.translation());
                let hand_aim = fcs
                    .as_deref()
                    .map_or(aim, |f| f.aim_for_hand(0, origin, arms.hands[0].init_speed, aim));
                let arm = &mut anims.aim[i];
                arm.sniper = arms.hands[0].sniper;
                arm.step(want_wa && arms.hands[0].kind != Kind::Skip && arms.hands[0].kind != Kind::Blade, frames);
                arm.frame = arm
                    .shoulder
                    .and_then(|s| rigging.aim_frame(s, hand, muzzle, hand_aim));
            }
            anims.apply_recoil(frames, &mut posed);
        }
        return;
    }

    for (i, gun) in arms.hands.iter_mut().enumerate() {
        gun.cooldown = (gun.cooldown - dt * TICK_RATE).max(0.0);
        let hand = if i == 0 { Hand::Right } else { Hand::Left };
        if gun.kind == Kind::Blade {
            let Some(blade) = gun.blade else { continue };
            let has_ammo = blade.infinite_ammo || gun.remaining > 0;
            if want[i]
                && has_ammo
                && gun.cooldown <= 0.0
                && !pilot.in_blade()
                && !pilot.in_kick()
                && pilot.en >= gun.en_drain
            {
                let target = fcs
                    .as_deref()
                    .and_then(|f| f.locked.as_ref())
                    .map(|l| (l.entity, l.world_pos));
                let launch = BladeLaunch {
                    hand_idx: i,
                    physical: blade.physical,
                    target,
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
                if pilot.start_blade(launch) {
                    let _ = pilot.try_drain_en(gun.en_drain);
                    if !blade.infinite_ammo {
                        gun.remaining = gun.remaining.saturating_sub(1);
                    }
                    gun.cooldown = gun.reload_time.max(1.0);
                    if let Some(status) = status.as_deref_mut() {
                        status.en = pilot.en;
                        status.en_max = pilot.en_max;
                    }
                    sound::shot(
                        &mut commands,
                        &mut speaker.cues,
                        &mut speaker.sources,
                        gun.shoot,
                    );
                }
            }
            continue;
        }
        let can = gun.kind != Kind::Skip
            && gun.remaining > 0
            && gun.cooldown <= 0.0
            && pilot.en >= gun.en_drain;
        // Ready weapons wait out the body stance. A sniper rifle does not take one.
        let stance_ready = !gun.stance
            || body.motion.is_none()
            || body.anims.as_ref().is_none_or(|a| a.sniper.allows(hand));
        // Weapon deploy / fire clip playback. For charge weapons, deploy holds while charging.
        let clip_ready = if let Some(anims) = body.anims.as_mut() {
            let (clip_ok, deploy) = match anims
                .rigs
                .iter_mut()
                .find(|r| r.hand == hand && !r.bay && !r.retired)
            {
                Some(rig) => {
                    let clip_ok = rig.playback.tick(want[i], can, frames);
                    let deploy = rig.playback.deploys();
                    rig.apply(&mut posed);
                    (clip_ok, deploy)
                }
                None => (want[i] && can, false),
            };
            if deploy {
                if let Some(arm) = anims.arms.iter_mut().find(|r| r.hand == hand) {
                    arm.playback.tick(want[i], false, frames);
                    arm.apply(&mut posed);
                }
            }
            if gun.fire_mode == FireMode::Charge || gun.fire_mode == FireMode::Burst {
                stance_ready
            } else {
                clip_ok && stance_ready
            }
        } else {
            stance_ready
        };
        let action = gun.step_trigger(want[i], can, clip_ready, frames);
        let (power_scale, pellets) = match action {
            TriggerAction::None => continue,
            TriggerAction::StartCharge => {
                if gun.charge_se > 0 {
                    sound::shot(
                        &mut commands,
                        &mut speaker.cues,
                        &mut speaker.sources,
                        gun.charge_se,
                    );
                }
                continue;
            }
            TriggerAction::Fire {
                power_scale,
                pellets,
            } => (power_scale, pellets),
        };
        if !pilot.try_drain_en(gun.en_drain) {
            continue;
        }
        if gun.stance {
            pilot.trigger_impact_shake(220.0);
            pilot.trigger_rumble_shake(0.14, Vec2::new(0.06, 0.12), Vec2::new(0.02, 0.02));
        }
        if let Some(anims) = body.anims.as_mut() {
            anims.kick(hand, gun.sniper);
        }
        if let Some(status) = status.as_deref_mut() {
            status.en = pilot.en;
            status.en_max = pilot.en_max;
        }
        let origin = rigging
            .hardpoints
            .iter()
            .find(|(h, _)| h.hand == hand)
            .map_or(ac_tf.translation, |(_, t)| t.translation());
        let hand_aim = fcs
            .as_deref()
            .map_or(aim, |f| f.aim_for_hand(i, origin, gun.init_speed, aim));
        gun.remaining = gun.remaining.saturating_sub(1);
        // Sequential bursts set full `reload_time` after the final round in the burst.
        if gun.burst_remaining == 0 {
            gun.cooldown = gun.reload_time.max(1.0);
        }
        sound::shot(
            &mut commands,
            &mut speaker.cues,
            &mut speaker.sources,
            gun.shoot,
        );
        let column = if hand == Hand::Right {
            "armwep_r"
        } else {
            "armwep_l"
        };
        if gun.fx.muzzle > 0 {
            match points
                .iter()
                .find(|(_, p, _)| p.column == column && p.id == MUZZLE_POINT)
            {
                Some((point, _, _)) => commands.spawn((
                    Sfx::new(gun.fx.muzzle),
                    Transform::from_scale(Vec3::splat(MUZZLE_SCALE)),
                    Visibility::default(),
                    ChildOf(point),
                )),
                None => commands.spawn((
                    Sfx::new(gun.fx.muzzle),
                    Transform::from_translation(origin)
                        .looking_to(-hand_aim, Vec3::Y)
                        .with_scale(Vec3::splat(MUZZLE_SCALE)),
                )),
            };
        }
        // Ejected shell cartridge (`0x829bee70` -> `0x82892df0` -> `0x82b88a78` at scale 1.0 on
        // dummy 111 registered in `0x8288f280`; falls back to muzzle dummy 101 if absent).
        if gun.fx.cartridge > 0 {
            let port = points
                .iter()
                .find(|(_, p, _)| p.column == column && p.id == CARTRIDGE_POINT)
                .or_else(|| {
                    points
                        .iter()
                        .find(|(_, p, _)| p.column == column && p.id == MUZZLE_POINT)
                });
            match port {
                Some((point, _, _)) => commands.spawn((
                    Sfx::new(gun.fx.cartridge),
                    Transform::default(),
                    Visibility::default(),
                    ChildOf(point),
                )),
                None => commands.spawn((
                    Sfx::new(gun.fx.cartridge),
                    Transform::from_translation(origin).looking_to(-hand_aim, Vec3::Y),
                )),
            };
        }
        let pellets = pellets.max(1);
        let right_axis = hand_aim.cross(Vec3::Y).normalize_or(Vec3::X);
        let up_axis = right_axis.cross(hand_aim).normalize_or(Vec3::Y);
        for p in 0..pellets {
            let shot_aim = if pellets > 1 {
                let az = std::f32::consts::TAU * (p as f32) / (pellets as f32);
                let ring = if p == 0 && pellets >= 6 { 0.0 } else { 0.04 };
                (hand_aim + (right_axis * az.cos() + up_axis * az.sin()) * ring)
                    .normalize_or(hand_aim)
            } else {
                hand_aim
            };
            let proj = Projectile::from_gun(gun, shot_aim).with_power_scale(power_scale);
            let shot = (
                Transform::from_translation(origin).looking_to(shot_aim, Vec3::Y),
                proj,
            );
            if gun.fx.bullet > 0 {
                commands
                    .spawn((shot, Visibility::default()))
                    .with_child((Sfx::new(gun.fx.bullet), Transform::default()));
            } else {
                let material = match gun.kind {
                    Kind::Energy => assets.energy.clone(),
                    _ => assets.rigid.clone(),
                };
                commands.spawn((Mesh3d(assets.mesh.clone()), MeshMaterial3d(material), shot));
            }
        }
    }

    // Shoulder unit firing & multi-lock missile salvo (`0x82858638`, manual.fmg R1).
    arms.shoulder.cooldown = (arms.shoulder.cooldown - frames).max(0.0);
    if arms.salvo.is_empty()
        && want_shoulder
        && arms.shoulder.kind != Kind::Skip
        && arms.shoulder.remaining > 0
        && arms.shoulder.cooldown <= 0.0
        && pilot.en >= arms.shoulder.en_drain
    {
        let burst = arms.shoulder.burst_count.max(1) as usize;
        if arms.shoulder.is_missile {
            let locks = fcs
                .as_deref_mut()
                .map_or_else(Vec::new, crate::fcs::Fcs::take_missile_locks);
            if locks.is_empty() {
                for _ in 0..burst.min(arms.shoulder.remaining as usize) {
                    arms.salvo.push(None);
                }
            } else {
                for target in locks {
                    for _ in 0..burst {
                        if arms.salvo.len() >= arms.shoulder.remaining as usize {
                            break;
                        }
                        arms.salvo.push(Some(target));
                    }
                }
            }
        } else {
            for _ in 0..burst.min(arms.shoulder.remaining as usize) {
                arms.salvo.push(None);
            }
        }
        if !arms.salvo.is_empty() {
            arms.salvo_timer = 0.0;
            arms.salvo_index = 0;
            arms.shoulder.cooldown = arms.shoulder.reload_time.max(1.0);
        }
    }
    if !arms.salvo.is_empty() {
        arms.salvo_timer = (arms.salvo_timer - frames).max(0.0);
        if arms.salvo_timer <= 0.0 {
            let target_opt = arms.salvo.remove(0);
            if arms.shoulder.remaining > 0 && pilot.try_drain_en(arms.shoulder.en_drain) {
                if let Some(status) = status.as_deref_mut() {
                    status.en = pilot.en;
                    status.en_max = pilot.en_max;
                }
                let origins: Vec<Vec3> = rigging
                    .shoulder
                    .iter()
                    .map(GlobalTransform::translation)
                    .collect();
                let origin = if origins.is_empty() {
                    ac_tf.translation + Vec3::Y * 5.0
                } else {
                    origins[(arms.salvo_index as usize) % origins.len()]
                };
                arms.shoulder.remaining = arms.shoulder.remaining.saturating_sub(1);
                sound::shot(
                    &mut commands,
                    &mut speaker.cues,
                    &mut speaker.sources,
                    arms.shoulder.shoot,
                );
                if arms.shoulder.fx.muzzle > 0 {
                    commands.spawn((
                        Sfx::new(arms.shoulder.fx.muzzle),
                        Transform::from_translation(origin)
                            .looking_to(-aim, Vec3::Y)
                            .with_scale(Vec3::splat(MUZZLE_SCALE)),
                    ));
                }
                let shoulder_aim = if target_opt.is_none() {
                    fcs.as_deref()
                        .and_then(|f| f.locked.as_ref())
                        .map(|l| (l.world_pos - origin).normalize_or_zero())
                        .filter(|&d| d != Vec3::ZERO)
                        .unwrap_or(aim)
                } else {
                    aim
                };
                let right = shoulder_aim.cross(Vec3::Y).normalize_or_zero();
                let side = if arms.salvo_index % 2 == 0 { 1.0 } else { -1.0 };
                let launch_dir = if arms.shoulder.kind == Kind::Missile && target_opt.is_some() {
                    (shoulder_aim + Vec3::Y * 0.35 + right * (0.28 * side)).normalize_or_zero()
                } else {
                    shoulder_aim
                };
                arms.salvo_index = arms.salvo_index.wrapping_add(1);
                let spawn_pos = origin + right * (0.6 * side) + Vec3::Y * 0.4;
                if arms.shoulder.kind == Kind::Missile {
                    let missile = Missile::from_gun(&arms.shoulder, launch_dir, target_opt);
                    let tf = Transform::from_translation(spawn_pos).looking_to(launch_dir, Vec3::Y);
                    if arms.shoulder.fx.bullet > 0 {
                        commands
                            .spawn((
                                tf,
                                missile,
                                Visibility::default(),
                                Mesh3d(assets.mesh.clone()),
                                MeshMaterial3d(assets.missile.clone()),
                            ))
                            .with_child((Sfx::new(arms.shoulder.fx.bullet), Transform::default()));
                    } else {
                        commands.spawn((
                            Mesh3d(assets.mesh.clone()),
                            MeshMaterial3d(assets.missile.clone()),
                            tf,
                            missile,
                        ));
                    }
                } else {
                    let shot = (
                        Transform::from_translation(spawn_pos).looking_to(launch_dir, Vec3::Y),
                        Projectile::from_gun(&arms.shoulder, launch_dir),
                    );
                    if arms.shoulder.fx.bullet > 0 {
                        commands
                            .spawn((shot, Visibility::default()))
                            .with_child((Sfx::new(arms.shoulder.fx.bullet), Transform::default()));
                    } else {
                        let material = match arms.shoulder.kind {
                            Kind::Energy => assets.energy.clone(),
                            _ => assets.rigid.clone(),
                        };
                        commands.spawn((
                            Mesh3d(assets.mesh.clone()),
                            MeshMaterial3d(material),
                            shot,
                        ));
                    }
                }
            }
            arms.salvo_timer = arms.shoulder.burst_interval.max(3.0);
        }
    }
    if body.shift.is_none() {
        if let Some(anims) = body.anims.as_mut() {
            for (i, gun) in arms.hands.iter().enumerate() {
                let (hand, column) = if i == 0 {
                    (Hand::Right, "armwep_r")
                } else {
                    (Hand::Left, "armwep_l")
                };
                let muzzle = points
                    .iter()
                    .find(|(_, p, _)| p.column == column && p.id == MUZZLE_POINT)
                    .map(|(e, _, _)| e);
                let origin = rigging
                    .hardpoints
                    .iter()
                    .find(|(h, _)| h.hand == hand)
                    .map_or(ac_tf.translation, |(_, t)| t.translation());
                let hand_aim = fcs
            .as_deref()
            .map_or(aim, |f| f.aim_for_hand(i, origin, gun.init_speed, aim));
                let arm = &mut anims.aim[i];
                arm.sniper = gun.sniper;
                arm.step(want[i] && gun.kind != Kind::Skip && !gun.stance, frames);
                arm.frame = arm
                    .shoulder
                    .and_then(|s| rigging.aim_frame(s, hand, muzzle, hand_aim));
            }
            anims.apply_recoil(frames, &mut posed);
        }
    }
}

/// Applies a secondary AoE explosion blast (`ImpactBlast` from `BULLET_ARISE_ST` ->
/// `BULLET_EXPLOSION_ST` / `BULLET_STATUS_ERROR_ST`) to all living targets within `blast.radius`.
fn apply_impact_blast(
    blast: ImpactBlast,
    center: Vec3,
    direct_entity: Option<Entity>,
    commands: &mut Commands,
    speaker: &mut Speaker,
    targets: &mut Query<
        (Entity, &Target, &mut TargetHealth, &Transform),
        (Without<Projectile>, Without<PlayerHealth>),
    >,
) {
    if blast.hit_fx > 0 {
        commands.spawn((Sfx::new(blast.hit_fx), Transform::from_translation(center)));
    }
    let flare_scale = (blast.radius / 25.0).clamp(0.7, 2.4);
    crate::lens_flare::spawn_point_flare(
        commands,
        center,
        Vec3::new(1.0, 0.72, 0.35),
        1.1,
        flare_scale,
        0.38,
    );
    if blast.hit_se > 0 {
        sound::cue(
            commands,
            &mut speaker.cues,
            &mut speaker.sources,
            &format!("w{:08}", blast.hit_se),
        );
    }
    let r2 = blast.radius * blast.radius;
    for (t_ent, target, mut health, t_tf) in targets.iter_mut() {
        if health.destroyed || Some(t_ent) == direct_entity {
            continue;
        }
        let tpos = t_tf.translation + Vec3::Y * 4.0;
        if tpos.distance_squared(center) <= r2 {
            let (res, lethal) =
                health.apply_hit(blast.damage_type, blast.damage_power, blast.damage_recoil);
            health.record_hit_from(center);
            info!(
                "OW blast hit {} ({}) -{:.0} AP ({:.0}/{:.0}){}",
                target.name,
                target.model,
                res.damage,
                health.ap,
                health.ap_max,
                if lethal { " DESTROYED" } else { "" },
            );
        }
    }
}

/// Same secondary AoE explosion helper for missile impacts (`Without<Missile>` query filter).
fn apply_missile_impact_blast(
    blast: ImpactBlast,
    center: Vec3,
    direct_entity: Option<Entity>,
    commands: &mut Commands,
    speaker: &mut Speaker,
    targets: &mut Query<
        (Entity, &Target, &mut TargetHealth, &Transform),
        (Without<Missile>, Without<PlayerHealth>),
    >,
) {
    if blast.hit_fx > 0 {
        commands.spawn((Sfx::new(blast.hit_fx), Transform::from_translation(center)));
    }
    let flare_scale = (blast.radius / 25.0).clamp(0.7, 2.4);
    crate::lens_flare::spawn_point_flare(
        commands,
        center,
        Vec3::new(1.0, 0.72, 0.35),
        1.1,
        flare_scale,
        0.38,
    );
    if blast.hit_se > 0 {
        sound::cue(
            commands,
            &mut speaker.cues,
            &mut speaker.sources,
            &format!("w{:08}", blast.hit_se),
        );
    }
    let r2 = blast.radius * blast.radius;
    for (t_ent, target, mut health, t_tf) in targets.iter_mut() {
        if health.destroyed || Some(t_ent) == direct_entity {
            continue;
        }
        let tpos = t_tf.translation + Vec3::Y * 4.0;
        if tpos.distance_squared(center) <= r2 {
            let (res, lethal) =
                health.apply_hit(blast.damage_type, blast.damage_power, blast.damage_recoil);
            health.record_hit_from(center);
            info!(
                "OW missile blast hit {} ({}) -{:.0} AP ({:.0}/{:.0}){}",
                target.name,
                target.model,
                res.damage,
                health.ap,
                health.ap_max,
                if lethal { " DESTROYED" } else { "" },
            );
        }
    }
}

/// Drives an equipped Overed Weapon (`AC_ASSEMBLY_DESIGN_ST.back_r`, category 11, slot 12)
/// through its 360 state machine (`0x82885c40..0x82886948`):
/// - Hold `U` (or hold `R` / `△` with no fire button held for `ow_mode_hold_frames` = 60 ticks,
///   `manual.fmg` 20405) to arm OW and purge/stow the left-arm weapon.
/// - Hold `F` / LMB / `R2` to charge `charge_en` at `ow_en_output`; release at full charge to fire.
/// - Drains limit gauge and applies periodic overload AP self-damage (`ow_overload_dmg`,
///   `ow_dmgtime_min..max`) while armed; auto-purges OW meshes and enters `lockout_secs` EN/FCS
///   lockout when limit hits zero (`0x828861d0`).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
#[derive(bevy::ecs::system::SystemParam)]
pub struct OwInput<'w, 's> {
    keys: Res<'w, ButtonInput<KeyCode>>,
    mouse: Res<'w, ButtonInput<MouseButton>>,
    held: Res<'w, Held>,
    pads: Query<'w, 's, &'static Gamepad>,
}

#[derive(bevy::ecs::system::SystemParam)]
pub struct OwAnimCtx<'w> {
    garage: Res<'w, crate::Garage>,
    motion: Option<ResMut<'w, Motion>>,
    ow_attach: Option<ResMut<'w, OwArmAttach>>,
    loadout: Option<Res<'w, crate::bay::Loadout>>,
}

pub fn step_overed_weapon(
    time: Res<Time>,
    input: OwInput,
    piloting: Res<Piloting>,
    assets: Res<Tracers>,
    mut speaker: Speaker,
    mut commands: Commands,
    mut anim: OwAnimCtx,
    mut acs: Query<(
        &mut Pilot,
        &mut Armament,
        &Transform,
        Option<&mut PlayerHealth>,
        Option<&mut Status>,
        Option<&crate::fcs::Fcs>,
    )>,
    ow_hp: Query<&GlobalTransform, With<OwHardpoint>>,
    mut ow_meshes: Query<&mut Visibility, (With<OwMesh>, Without<crate::bay::WeaponMesh>)>,
    mut wep_meshes: Query<(&crate::bay::WeaponMesh, &mut Visibility), Without<OwMesh>>,
    points: Query<(&EffectPoint, &GlobalTransform)>,
) {
    let keys = &input.keys;
    let mouse = &input.mouse;
    let held = &input.held;
    let pads = &input.pads;
    let garage = &anim.garage;
    let motion = &mut anim.motion;
    let ow_attach = &mut anim.ow_attach;
    let loadout = &anim.loadout;
    if !piloting.0 {
        return;
    }
    let dt = time.delta_secs();
    let Ok((mut pilot, mut arms, ac_tf, mut player_health, mut status, fcs)) = acs.single_mut()
    else {
        return;
    };
    if player_health.as_ref().is_some_and(|h| h.destroyed) {
        pilot.ow_active = false;
        pilot.ow_lockout = false;
        return;
    }
    let Some(mut ow) = arms.overed else {
        pilot.ow_active = false;
        pilot.ow_lockout = false;
        if let Some(s) = status.as_deref_mut() {
            s.ow_phase = OwPhase::Disarmed;
            s.ow_charge = 0.0;
            s.ow_limit = 0.0;
        }
        return;
    };

    let down = |k: KeyCode| keys.pressed(k) || held.0.contains(&k);
    let mut fire_held = down(KeyCode::KeyF) || mouse.pressed(MouseButton::Left);
    let mut left_fire = down(KeyCode::KeyC) || mouse.pressed(MouseButton::Right);
    let mut tri_held = down(KeyCode::KeyR);
    for pad in pads {
        fire_held |= pad.pressed(GamepadButton::RightTrigger2);
        left_fire |= pad.pressed(GamepadButton::LeftTrigger2);
        tri_held |= pad.pressed(GamepadButton::North);
    }
    // `manual.fmg` 20405: hold `△` (or dedicated `U` key) without pressing fire buttons to arm OW.
    let activate_held = !pilot.scan_mode
        && (down(KeyCode::KeyU) || (tri_held && !fire_held && !left_fire));
    let trigger_for_ow = !pilot.scan_mode && fire_held;

    let res = ow.step(dt, activate_held, trigger_for_ow);

    pilot.ow_active = ow.is_active();
    pilot.ow_lockout = ow.phase == OwPhase::Lockout;

    // Switch body animation bank (`motion_ow_equip`, 360 `0x8284e9d8`) and dynamically re-attach
    // OW root `"60"` onto arm dummy `60` (`acattachinfo` record 1) while OW is active.
    if let Some(m) = motion.as_deref_mut() {
        m.set_ow_bank(ow.is_active());
    }
    if let Some(att) = ow_attach.as_deref_mut() {
        if att.set_deployed(&mut commands, ow.is_active()) {
            info!(
                "OW root 60 {} arm dummy 60",
                if ow.is_active() { "re-attached to" } else { "stowed from" }
            );
        }
    }
    if matches!(ow.phase, OwPhase::Charging | OwPhase::Charged) {
        if let Some(m) = motion.as_deref_mut() {
            if m.upper.as_ref().map(|u| u.anim_id) != Some(209) && m.has_ow_clip(209) {
                let _ = m.play_upper(&garage.disc, 209, true, 1.0, [0.15; 8], 8);
            }
        }
    }

    if res.overload_damage > 0.0 {
        if let Some(health) = player_health.as_deref_mut() {
            let (hit, _) = health.apply_hit(DamageType::Unmitigated, res.overload_damage, 0.0);
            if let Some(s) = status.as_deref_mut() {
                s.ap = health.ap_u32();
            }
            info!(
                "OW overload self-damage -{:.0} AP ({:.0}/{:.0})",
                hit.damage, health.ap, health.ap_max
            );
        }
    }

    if let Some(event) = res.event {
        match event {
            OwEvent::Armed => {
                // Purge/stow left-arm weapon so the left arm can interface with the Overed Weapon.
                arms.hands[1] = silent();
                if let Some(l) = loadout.as_deref() {
                    if let Some(left_root) = l.slots[1].as_ref().and_then(|m| m.weapon) {
                        for (wm, mut vis) in &mut wep_meshes {
                            if wm.root == left_root {
                                *vis = Visibility::Hidden;
                            }
                        }
                    }
                }
                sound::cue(&mut commands, &mut speaker.cues, &mut speaker.sources, "b00000000");
                if let Some(m) = motion.as_deref_mut() {
                    if m.has_ow_clip(200) {
                        let _ = m.play_upper(&garage.disc, 200, false, 1.0, [0.15; 8], 9);
                    }
                }
                info!(
                    "OW {} (type {}) ARMED: limit {:.0}, charge_en {:.0}, ow_en_out {:.0}/s",
                    ow.spec.part_id,
                    ow.spec.ow_type,
                    ow.spec.limit_capacity,
                    ow.spec.charge_en,
                    ow.spec.ow_en_output
                );
            }
            OwEvent::ChargeComplete => {
                sound::cue(&mut commands, &mut speaker.cues, &mut speaker.sources, "c00000024");
                info!("OW {} CHARGE COMPLETE — release trigger to unleash!", ow.spec.part_id);
            }
            OwEvent::Fired => {
                pilot.trigger_impact_shake(250.0);
                pilot.trigger_rumble_shake(0.35, Vec2::new(0.20, 0.30), Vec2::new(0.025, 0.025));
                let aim = pilot.aim_direction();
                let ow_muzzle = points
                    .iter()
                    .find(|(p, _)| p.column == "back_r" && (101..=112).contains(&p.id))
                    .map(|(_, gt)| gt.translation())
                    .or_else(|| ow_hp.iter().next().map(GlobalTransform::translation))
                    .unwrap_or(ac_tf.translation + Vec3::Y * 5.0);
                let gun = &ow.spec.gun;
                if gun.shoot > 0 {
                    sound::shot(&mut commands, &mut speaker.cues, &mut speaker.sources, gun.shoot);
                }
                if let Some(m) = motion.as_deref_mut() {
                    if m.has_ow_clip(202) {
                        let _ = m.play_upper(&garage.disc, 202, false, 1.0, [0.1; 8], 10);
                    }
                }
                if gun.fx.muzzle > 0 {
                    commands.spawn((
                        Sfx::new(gun.fx.muzzle),
                        Transform::from_translation(ow_muzzle)
                            .looking_to(-aim, Vec3::Y)
                            .with_scale(Vec3::splat(MUZZLE_SCALE)),
                    ));
                }
                crate::lens_flare::spawn_point_flare(
                    &mut commands,
                    ow_muzzle,
                    Vec3::new(1.0, 0.85, 0.55),
                    1.4,
                    2.2,
                    0.55,
                );
                let locked = fcs.and_then(|f| f.locked.as_ref());
                let fire_aim = locked
                    .map(|l| (l.world_pos - ow_muzzle).normalize_or_zero())
                    .filter(|&d| d != Vec3::ZERO)
                    .unwrap_or(aim);

                if ow.spec.is_melee() {
                    // Grind Blade (1), Mass Blade (3), Mega Blade (60): OW dash lunge + giant melee sweep.
                    pilot.start_ow_dash(fire_aim);
                    if let Some(blade) = gun.blade {
                        let launch = BladeLaunch {
                            hand_idx: 0,
                            physical: blade.physical,
                            target: locked.map(|l| (l.entity, l.world_pos)),
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
                        pilot.start_blade(launch);
                    }
                } else if ow.spec.ow_type == 10 || gun.kind == Kind::Missile {
                    // Giga Missile (1010, bullet 59010 + 100 m mega-blast 101010 -> 70010).
                    let target_ent = locked.map(|l| l.entity);
                    let launch_dir = (fire_aim + Vec3::Y * 0.25).normalize_or(fire_aim);
                    let missile =
                        Missile::from_gun(gun, launch_dir, target_ent).with_blast(ow.spec.blast);
                    let tf = Transform::from_translation(ow_muzzle).looking_to(launch_dir, Vec3::Y);
                    if gun.fx.bullet > 0 {
                        commands
                            .spawn((
                                tf,
                                missile,
                                Visibility::default(),
                                Mesh3d(assets.mesh.clone()),
                                MeshMaterial3d(assets.missile.clone()),
                            ))
                            .with_child((Sfx::new(gun.fx.bullet), Transform::default()));
                    } else {
                        commands.spawn((
                            Mesh3d(assets.mesh.clone()),
                            MeshMaterial3d(assets.missile.clone()),
                            tf,
                            missile,
                        ));
                    }
                } else if ow.spec.ow_type == 8 {
                    // Multiple Pulse (810, 扇キャノン): 7-beam fan volley with TE sphere blast (99815 -> 69810).
                    let right = fire_aim.cross(Vec3::Y).normalize_or(Vec3::X);
                    for k in -3..=3_i32 {
                        let yaw_off = (k as f32) * 5.0_f32.to_radians();
                        let dir = (fire_aim * yaw_off.cos() + right * yaw_off.sin())
                            .normalize_or(fire_aim);
                        let proj = Projectile::from_gun(gun, dir).with_blast(ow.spec.blast);
                        let tf = Transform::from_translation(ow_muzzle).looking_to(dir, Vec3::Y);
                        if gun.fx.bullet > 0 {
                            commands
                                .spawn((tf, proj, Visibility::default()))
                                .with_child((Sfx::new(gun.fx.bullet), Transform::default()));
                        } else {
                            commands.spawn((
                                Mesh3d(assets.mesh.clone()),
                                MeshMaterial3d(assets.energy.clone()),
                                tf,
                                proj,
                            ));
                        }
                    }
                } else {
                    // Huge Cannon (610, bullet 10050, 1800 m/s + 50 m nuclear blast 99615).
                    let proj = Projectile::from_gun(gun, fire_aim).with_blast(ow.spec.blast);
                    let tf = Transform::from_translation(ow_muzzle).looking_to(fire_aim, Vec3::Y);
                    if gun.fx.bullet > 0 {
                        commands
                            .spawn((tf, proj, Visibility::default()))
                            .with_child((Sfx::new(gun.fx.bullet), Transform::default()));
                    } else {
                        commands.spawn((
                            Mesh3d(assets.mesh.clone()),
                            MeshMaterial3d(assets.rigid.clone()),
                            tf,
                            proj,
                        ));
                    }
                }
                info!(
                    "OW {} (type {}) FIRED (power {:.0}, recoil {:.0})",
                    ow.spec.part_id, ow.spec.ow_type, gun.damage_power, gun.damage_recoil
                );
            }
            OwEvent::Purged => {
                // Auto-purge OW meshes (`0x828865f8` case 2 -> 3) and enter post-OW EN/FCS lockout.
                for mut vis in &mut ow_meshes {
                    *vis = Visibility::Hidden;
                }
                pilot.en = 0.0;
                if let Some(m) = motion.as_deref_mut() {
                    if m.has_ow_clip(205) {
                        let _ = m.play_upper(&garage.disc, 205, false, 1.0, [0.15; 8], 10);
                    }
                }
                sound::cue(&mut commands, &mut speaker.cues, &mut speaker.sources, "c00000024");
                info!(
                    "OW {} AUTO-PURGED — entering {:.0}s EN/FCS lockout",
                    ow.spec.part_id, ow.spec.lockout_secs
                );
            }
            OwEvent::LockoutEnded => {
                info!("OW {} lockout ended — EN/FCS systems restored", ow.spec.part_id);
            }
        }
    }

    if let Some(s) = status.as_deref_mut() {
        s.en = pilot.en;
        s.en_max = pilot.en_max;
        s.ow_phase = ow.phase;
        s.ow_charge = ow.charge_fraction();
        s.ow_limit = ow.limit_fraction();
    }
    arms.overed = Some(ow);
}

/// Steps in-flight projectiles against target hit spheres (for `Owner::Player`), the player AC's
/// hit spheres (for `Owner::Enemy`), and map ground collision, applying the 360 damage formula
/// (`0x827d0b50`) and spawning hit VFX/SFX.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn step_projectiles(
    time: Res<Time>,
    piloting: Res<Piloting>,
    collision: Res<Collision>,
    mut speaker: Speaker,
    mut commands: Commands,
    mut targets: Query<
        (Entity, &Target, &mut TargetHealth, &Transform),
        (Without<Projectile>, Without<PlayerHealth>),
    >,
    mut player: Query<
        (&mut PlayerHealth, Option<&mut Status>, &Transform),
        (Without<Projectile>, Without<Target>),
    >,
    mut props: Query<&mut crate::map::DestructibleProp>,
    mut shots: Query<(Entity, &mut Projectile, &mut Transform)>,
) {
    if !piloting.0 {
        return;
    }
    let dt = time.delta_secs();
    for (e, mut shot, mut transform) in &mut shots {
        let Some(delta) = shot.step(dt) else {
            commands.entity(e).despawn();
            continue;
        };
        let prev = transform.translation;
        let next = prev + delta;

        let terrain = collision.raycast_with_prop(prev, next, RayFilter::Bullet);

        match shot.owner {
            Owner::Player => {
                // Test against all living target hit spheres, keeping the earliest intersection.
                let mut best_target: Option<(f32, Vec3, Vec3, Entity)> = None;
                for (t_entity, _, health, t_tf) in &targets {
                    if health.destroyed {
                        continue;
                    }
                    for sphere in &health.spheres {
                        let center = t_tf.translation + Vec3::Y * sphere.offset_y;
                        if let Some((t, pos, normal)) =
                            damage::segment_hits_sphere(prev, next, center, sphere.radius)
                        {
                            if best_target.is_none_or(|(best_t, _, _, _)| t < best_t) {
                                best_target = Some((t, pos, normal, t_entity));
                            }
                        }
                    }
                }
                if let Some((t_hit, pos, normal, t_entity)) = best_target {
                    if terrain.is_none_or(|(t_g, _, _, _)| t_hit <= t_g) {
                        if let Ok((_, target, mut health, _)) = targets.get_mut(t_entity) {
                            let (power, recoil) = shot.effective_damage();
                            let (res, lethal) = health.apply_hit(shot.damage_type, power, recoil);
                            health.record_hit_from(prev);
                            let ffx = if res.effective {
                                if shot.hit_hi > 0 { shot.hit_hi } else { shot.hit }
                            } else if shot.hit_low > 0 {
                                shot.hit_low
                            } else {
                                shot.hit
                            };
                            if ffx > 0 {
                                let rot = Quat::from_rotation_arc(Vec3::Z, normal);
                                commands.spawn((
                                    Sfx::new(ffx),
                                    Transform::from_translation(pos).with_rotation(rot),
                                ));
                            }
                            let se = if res.effective {
                                shot.hit_se.hi
                            } else {
                                shot.hit_se.low
                            };
                            if se > 0 {
                                sound::cue(
                                    &mut commands,
                                    &mut speaker.cues,
                                    &mut speaker.sources,
                                    &format!("w{se:08}"),
                                );
                            }
                            info!(
                                "hit {} ({}) [{}] -{:.0} AP ({:.0}/{:.0}){}{}",
                                target.name,
                                target.model,
                                if res.effective { "EFFECTIVE" } else { "RICOCHET" },
                                res.damage,
                                health.ap,
                                health.ap_max,
                                if health.is_staggered() { " STAGGER" } else { "" },
                                if lethal { " DESTROYED" } else { "" },
                            );
                        }
                        if let Some(blast) = shot.blast {
                            apply_impact_blast(
                                blast,
                                pos,
                                Some(t_entity),
                                &mut commands,
                                &mut speaker,
                                &mut targets,
                            );
                        }
                        commands.entity(e).despawn();
                        continue;
                    }
                }
            }
            Owner::Enemy => {
                // Sweep enemy projectile against the player AC's `ac_hit_spheres()` (`0x82863b60`).
                if let Ok((mut health, mut status, p_tf)) = player.single_mut() {
                    if !health.destroyed {
                        let mut best: Option<(f32, Vec3, Vec3)> = None;
                        for sphere in &health.spheres {
                            let center = p_tf.translation + Vec3::Y * sphere.offset_y;
                            if let Some((t, pos, normal)) =
                                damage::segment_hits_sphere(prev, next, center, sphere.radius)
                            {
                                if best.is_none_or(|(best_t, _, _)| t < best_t) {
                                    best = Some((t, pos, normal));
                                }
                            }
                        }
                        if let Some((t_hit, pos, normal)) = best {
                            if terrain.is_none_or(|(t_g, _, _, _)| t_hit <= t_g) {
                                let (power, recoil) = shot.effective_damage();
                                let (res, lethal) =
                                    health.apply_hit(shot.damage_type, power, recoil);
                                if let Some(s) = status.as_deref_mut() {
                                    s.ap = health.ap_u32();
                                }
                                let ffx = if res.effective {
                                    if shot.hit_hi > 0 { shot.hit_hi } else { shot.hit }
                                } else if shot.hit_low > 0 {
                                    shot.hit_low
                                } else {
                                    shot.hit
                                };
                                if ffx > 0 {
                                    let rot = Quat::from_rotation_arc(Vec3::Z, normal);
                                    commands.spawn((
                                        Sfx::new(ffx),
                                        Transform::from_translation(pos).with_rotation(rot),
                                    ));
                                }
                                let se = if res.effective {
                                    shot.hit_se.hi
                                } else {
                                    shot.hit_se.low
                                };
                                if se > 0 {
                                    sound::cue(
                                        &mut commands,
                                        &mut speaker.cues,
                                        &mut speaker.sources,
                                        &format!("w{se:08}"),
                                    );
                                }
                                info!(
                                    "player hit [{}] -{:.0} AP ({:.0}/{:.0}){}{}",
                                    if res.effective { "EFFECTIVE" } else { "RICOCHET" },
                                    res.damage,
                                    health.ap,
                                    health.ap_max,
                                    if health.is_staggered() { " STAGGER" } else { "" },
                                    if lethal { " DESTROYED" } else { "" },
                                );
                                commands.entity(e).despawn();
                                continue;
                            }
                        }
                    }
                }
            }
        }

        if let Some((_, at, normal, prop_id)) = terrain {
            if let Some(pid) = prop_id {
                let (power, _) = shot.effective_damage();
                for mut prop in &mut props {
                    if prop.prop_id == pid {
                        prop.apply_damage(power.max(1.0));
                        break;
                    }
                }
            }
            if shot.hit > 0 {
                commands.spawn((
                    Sfx::new(shot.hit),
                    Transform::from_translation(at)
                        .with_rotation(Quat::from_rotation_arc(Vec3::Z, normal)),
                ));
            }
            if shot.hit_se.low > 0 {
                sound::cue(
                    &mut commands,
                    &mut speaker.cues,
                    &mut speaker.sources,
                    &format!("w{:08}", shot.hit_se.low),
                );
            }
            if shot.owner == Owner::Player {
                if let Some(blast) = shot.blast {
                    apply_impact_blast(blast, at, None, &mut commands, &mut speaker, &mut targets);
                    for mut prop in &mut props {
                        if !prop.broken && prop.center.distance(at) <= blast.radius + prop.radius {
                            prop.apply_damage(blast.damage_power.max(1.0));
                        }
                    }
                }
            }
            commands.entity(e).despawn();
            continue;
        }

        transform.translation = next;
        if delta.length_squared() > 0.0 {
            transform.look_to(delta.normalize(), Vec3::Y);
        }
    }
}

/// Steps in-flight homing missiles (`BulletMissile`, `0x828ec898` -> `0x828eaf88`) toward their
/// locked targets, spawns booster-ignition trail VFX, and resolves hits against target spheres
/// (or player AC spheres for `Owner::Enemy`) and map terrain.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn step_missiles(
    time: Res<Time>,
    piloting: Res<Piloting>,
    collision: Res<Collision>,
    assets: Res<Tracers>,
    mut speaker: Speaker,
    mut commands: Commands,
    mut targets: Query<
        (Entity, &Target, &mut TargetHealth, &Transform),
        (Without<Missile>, Without<PlayerHealth>),
    >,
    mut player: Query<
        (Entity, &mut PlayerHealth, Option<&mut Status>, &Transform),
        (Without<Missile>, Without<Target>),
    >,
    mut props: Query<&mut crate::map::DestructibleProp>,
    mut missiles: Query<(Entity, &mut Missile, &mut Transform)>,
) {
    if !piloting.0 {
        return;
    }
    let dt = time.delta_secs();
    for (e, mut missile, mut transform) in &mut missiles {
        let target_pos = match missile.owner {
            Owner::Player => missile.target.and_then(|t_ent| {
                targets
                    .get(t_ent)
                    .ok()
                    .filter(|(_, _, h, _)| !h.destroyed)
                    .map(|(_, _, h, tf)| tf.translation + Vec3::Y * h.center_offset_y())
            }),
            Owner::Enemy => player
                .single()
                .ok()
                .filter(|(_, h, _, _)| !h.destroyed)
                .map(|(_, _, _, tf)| tf.translation + Vec3::Y * 4.5),
        };
        let prev = transform.translation;
        let Some((delta, just_ignited)) = missile.step(prev, target_pos, dt) else {
            commands.entity(e).despawn();
            continue;
        };
        if just_ignited && missile.after_accel_fx > 0 {
            commands
                .entity(e)
                .with_child((Sfx::new(missile.after_accel_fx), Transform::default()));
        }
        let next = prev + delta;

        // Sub-munition split detonation (`close_act_id == 8` / `shoot_bullet_id` ->
        // `BULLET_BULLETARISE_BIN`, `0x828eaac0` -> `0x828e9f48` -> `0x828de0c8` -> `0x828ddca8`).
        if let Some(split) = missile.should_split(next, target_pos) {
            let burst = missile.build_split_burst(next, target_pos, split);
            if burst.split_fx > 0 {
                commands.spawn((Sfx::new(burst.split_fx), Transform::from_translation(next)));
            }
            if burst.split_se > 0 {
                sound::cue(
                    &mut commands,
                    &mut speaker.cues,
                    &mut speaker.sources,
                    &format!("w{:08}", burst.split_se),
                );
            }
            for (spawn_pos, child) in burst.children {
                match child {
                    SplitChild::Missile(cm) => {
                        let dir = cm.velocity.normalize_or(Vec3::NEG_Z);
                        let tf = Transform::from_translation(spawn_pos).looking_to(dir, Vec3::Y);
                        let mat = match cm.owner {
                            Owner::Player => assets.missile.clone(),
                            Owner::Enemy => assets.enemy_missile.clone(),
                        };
                        if cm.bullet_fx > 0 {
                            commands
                                .spawn((
                                    tf,
                                    cm,
                                    Visibility::default(),
                                    Mesh3d(assets.mesh.clone()),
                                    MeshMaterial3d(mat),
                                ))
                                .with_child((Sfx::new(cm.bullet_fx), Transform::default()));
                        } else {
                            commands.spawn((
                                Mesh3d(assets.mesh.clone()),
                                MeshMaterial3d(mat),
                                tf,
                                cm,
                            ));
                        }
                    }
                    SplitChild::Projectile {
                        projectile,
                        kind,
                        bullet_fx,
                    } => {
                        let dir = projectile.velocity.normalize_or(Vec3::NEG_Z);
                        let tf = Transform::from_translation(spawn_pos).looking_to(dir, Vec3::Y);
                        let mat = match (projectile.owner, kind) {
                            (Owner::Player, Kind::Energy) => assets.energy.clone(),
                            (Owner::Player, _) => assets.rigid.clone(),
                            (Owner::Enemy, Kind::Energy) => assets.enemy_energy.clone(),
                            (Owner::Enemy, _) => assets.enemy_rigid.clone(),
                        };
                        if bullet_fx > 0 {
                            commands
                                .spawn((
                                    tf,
                                    projectile,
                                    Visibility::default(),
                                    Mesh3d(assets.mesh.clone()),
                                    MeshMaterial3d(mat),
                                ))
                                .with_child((Sfx::new(bullet_fx), Transform::default()));
                        } else {
                            commands.spawn((
                                Mesh3d(assets.mesh.clone()),
                                MeshMaterial3d(mat),
                                tf,
                                projectile,
                            ));
                        }
                    }
                }
            }
            commands.entity(e).despawn();
            continue;
        }

        let extra_r = missile
            .flight
            .proximity_fuse_range
            .max(missile.flight.hit_radius.min(2.0));

        let terrain = collision.raycast_with_prop(prev, next, RayFilter::Bullet);

        match missile.owner {
            Owner::Player => {
                let mut best_target: Option<(f32, Vec3, Vec3, Entity)> = None;
                for (t_entity, _, health, t_tf) in &targets {
                    if health.destroyed {
                        continue;
                    }
                    for sphere in &health.spheres {
                        let center = t_tf.translation + Vec3::Y * sphere.offset_y;
                        if let Some((t, pos, normal)) =
                            damage::segment_hits_sphere(prev, next, center, sphere.radius + extra_r)
                        {
                            if best_target.is_none_or(|(best_t, _, _, _)| t < best_t) {
                                best_target = Some((t, pos, normal, t_entity));
                            }
                        }
                    }
                }
                if let Some((t_hit, pos, normal, t_entity)) = best_target {
                    if terrain.is_none_or(|(t_g, _, _, _)| t_hit <= t_g) {
                        if let Ok((_, target, mut health, _)) = targets.get_mut(t_entity) {
                            let (power, recoil) = missile.effective_damage();
                            let (res, lethal) =
                                health.apply_hit(missile.damage_type, power, recoil);
                            health.record_hit_from(prev);
                            let ffx = if res.effective {
                                if missile.hit_hi > 0 {
                                    missile.hit_hi
                                } else if missile.hit > 0 {
                                    missile.hit
                                } else {
                                    missile.delete_fx
                                }
                            } else if missile.hit_low > 0 {
                                missile.hit_low
                            } else if missile.hit > 0 {
                                missile.hit
                            } else {
                                missile.delete_fx
                            };
                            if ffx > 0 {
                                let rot = Quat::from_rotation_arc(Vec3::Z, normal);
                                commands.spawn((
                                    Sfx::new(ffx),
                                    Transform::from_translation(pos).with_rotation(rot),
                                ));
                            }
                            let se = if res.effective {
                                missile.hit_se.hi
                            } else {
                                missile.hit_se.low
                            };
                            if se > 0 {
                                sound::cue(
                                    &mut commands,
                                    &mut speaker.cues,
                                    &mut speaker.sources,
                                    &format!("w{se:08}"),
                                );
                            }
                            info!(
                                "missile hit {} ({}) [{}] -{:.0} AP ({:.0}/{:.0}){}{}",
                                target.name,
                                target.model,
                                if res.effective { "EFFECTIVE" } else { "RICOCHET" },
                                res.damage,
                                health.ap,
                                health.ap_max,
                                if health.is_staggered() { " STAGGER" } else { "" },
                                if lethal { " DESTROYED" } else { "" },
                            );
                        }
                        if let Some(blast) = missile.blast {
                            apply_missile_impact_blast(
                                blast,
                                pos,
                                Some(t_entity),
                                &mut commands,
                                &mut speaker,
                                &mut targets,
                            );
                        }
                        commands.entity(e).despawn();
                        continue;
                    }
                }
            }
            Owner::Enemy => {
                if let Ok((_, mut health, mut status, p_tf)) = player.single_mut() {
                    if !health.destroyed {
                        let mut best: Option<(f32, Vec3, Vec3)> = None;
                        for sphere in &health.spheres {
                            let center = p_tf.translation + Vec3::Y * sphere.offset_y;
                            if let Some((t, pos, normal)) = damage::segment_hits_sphere(
                                prev,
                                next,
                                center,
                                sphere.radius + extra_r,
                            ) {
                                if best.is_none_or(|(best_t, _, _)| t < best_t) {
                                    best = Some((t, pos, normal));
                                }
                            }
                        }
                        if let Some((t_hit, pos, normal)) = best {
                            if terrain.is_none_or(|(t_g, _, _, _)| t_hit <= t_g) {
                                let (power, recoil) = missile.effective_damage();
                                let (res, lethal) =
                                    health.apply_hit(missile.damage_type, power, recoil);
                                if let Some(s) = status.as_deref_mut() {
                                    s.ap = health.ap_u32();
                                }
                                let ffx = if res.effective {
                                    if missile.hit_hi > 0 {
                                        missile.hit_hi
                                    } else if missile.hit > 0 {
                                        missile.hit
                                    } else {
                                        missile.delete_fx
                                    }
                                } else if missile.hit_low > 0 {
                                    missile.hit_low
                                } else if missile.hit > 0 {
                                    missile.hit
                                } else {
                                    missile.delete_fx
                                };
                                if ffx > 0 {
                                    let rot = Quat::from_rotation_arc(Vec3::Z, normal);
                                    commands.spawn((
                                        Sfx::new(ffx),
                                        Transform::from_translation(pos).with_rotation(rot),
                                    ));
                                }
                                let se = if res.effective {
                                    missile.hit_se.hi
                                } else {
                                    missile.hit_se.low
                                };
                                if se > 0 {
                                    sound::cue(
                                        &mut commands,
                                        &mut speaker.cues,
                                        &mut speaker.sources,
                                        &format!("w{se:08}"),
                                    );
                                }
                                info!(
                                    "player missile hit [{}] -{:.0} AP ({:.0}/{:.0}){}{}",
                                    if res.effective { "EFFECTIVE" } else { "RICOCHET" },
                                    res.damage,
                                    health.ap,
                                    health.ap_max,
                                    if health.is_staggered() { " STAGGER" } else { "" },
                                    if lethal { " DESTROYED" } else { "" },
                                );
                                commands.entity(e).despawn();
                                continue;
                            }
                        }
                    }
                }
            }
        }

        if let Some((_, at, normal, prop_id)) = terrain {
            if let Some(pid) = prop_id {
                let (power, _) = missile.effective_damage();
                for mut prop in &mut props {
                    if prop.prop_id == pid {
                        prop.apply_damage(power.max(1.0));
                        break;
                    }
                }
            }
            let ffx = if missile.hit > 0 {
                missile.hit
            } else {
                missile.delete_fx
            };
            if ffx > 0 {
                commands.spawn((
                    Sfx::new(ffx),
                    Transform::from_translation(at)
                        .with_rotation(Quat::from_rotation_arc(Vec3::Z, normal)),
                ));
            }
            if missile.hit_se.low > 0 {
                sound::cue(
                    &mut commands,
                    &mut speaker.cues,
                    &mut speaker.sources,
                    &format!("w{:08}", missile.hit_se.low),
                );
            }
            if missile.owner == Owner::Player {
                if let Some(blast) = missile.blast {
                    apply_missile_impact_blast(
                        blast,
                        at,
                        None,
                        &mut commands,
                        &mut speaker,
                        &mut targets,
                    );
                    for mut prop in &mut props {
                        if !prop.broken && prop.center.distance(at) <= blast.radius + prop.radius {
                            prop.apply_damage(blast.damage_power.max(1.0));
                        }
                    }
                }
            }
            commands.entity(e).despawn();
            continue;
        }

        transform.translation = next;
        if delta.length_squared() > 0.0 {
            transform.look_to(delta.normalize(), Vec3::Y);
        }
    }
}

/// Steps the player AC's stagger timer, plays destruction explosion on AP == 0, and cleanly
/// respawns the player at the map start point after `PLAYER_RESPAWN_SECS`.
#[allow(clippy::too_many_arguments)]
pub fn tick_player_health(
    time: Res<Time>,
    start: Res<crate::map::Start>,
    collision: Res<Collision>,
    mut speaker: Speaker,
    mut commands: Commands,
    mut acs: Query<(&mut PlayerHealth, &mut Pilot, &mut Transform, Option<&mut Status>)>,
) {
    let dt = time.delta_secs();
    let Ok((mut health, mut pilot, mut tf, mut status)) = acs.single_mut() else {
        return;
    };
    if let Some((recoil, staggered)) = health.hit_shake.take() {
        // Scale incoming recoil into the `ShakeImpactAccelMin..Max` (150..250) band and add
        // a timed rumble impulse (`0x82bf8f00`), stronger on stagger hits.
        let accel = 150.0 + (recoil / 12.0).min(100.0);
        pilot.trigger_impact_shake(accel);
        if staggered {
            pilot.trigger_rumble_shake(0.28, Vec2::new(0.16, 0.24), Vec2::new(0.022, 0.022));
        } else {
            let s = (recoil / 1500.0).clamp(0.25, 1.0);
            pilot.trigger_rumble_shake(0.12, Vec2::new(0.05 * s, 0.08 * s), Vec2::new(0.02, 0.02));
        }
    }
    if health.just_destroyed {
        health.just_destroyed = false;
        pilot.velocity = Vec3::ZERO;
        let center = tf.translation + Vec3::Y * 4.5;
        commands.spawn((Sfx::new(4041), Transform::from_translation(center)));
        crate::lens_flare::spawn_point_flare(
            &mut commands,
            center,
            Vec3::new(1.0, 0.65, 0.28),
            1.5,
            2.4,
            0.65,
        );
        let destroy_se = find(PARAM_ACSOUNDPARAM_BIN, 0)
            .filter(|r| r.data.se_destroy > 0)
            .map(|r| format!("c{:08}", r.data.se_destroy))
            .unwrap_or_else(|| "c00000089".to_string());
        sound::cue(
            &mut commands,
            &mut speaker.cues,
            &mut speaker.sources,
            &destroy_se,
        );
        info!(
            "player AC destroyed at ({:.1}, {:.1}, {:.1}); respawning in {:.1}s",
            center.x, center.y, center.z, damage::PLAYER_RESPAWN_SECS
        );
    }
    if health.destroyed {
        pilot.velocity = Vec3::ZERO;
    }
    let respawn = health.tick(dt);
    if respawn {
        health.reset();
        pilot.velocity = Vec3::ZERO;
        pilot.en = pilot.en_max;
        pilot.yaw = start.yaw;
        let spawn_y = collision
            .ground_below(start.position + Vec3::Y * RAY_HALF)
            .unwrap_or(start.position.y);
        tf.translation = start.position.with_y(spawn_y);
        tf.rotation = Quat::from_rotation_y(start.yaw);
        if let Some(s) = status.as_deref_mut() {
            s.ap = health.ap_u32();
            s.en = pilot.en;
        }
        info!("player AC respawned at start with {:.0} AP", health.ap);
    } else if let Some(s) = status.as_deref_mut() {
        s.ap = health.ap_u32();
    }
}

/// Sweeps active Boost Charge Kick (`KickState`, `BULLET_BULLETEXPLOSION_BIN` row 4100
/// `init_radius = 7.0 m`, `0x8286ac40` speed×weight scaling, hit VFX/SE row `99000`) and
/// active Melee Blade swings (`BladeState`, `BULLET_BULLETBLADE_BIN`, `0x828a4928` → `0x828a43e8`)
/// against living `TargetHealth` spheres.
#[allow(clippy::too_many_arguments)]
pub fn step_melee(
    piloting: Res<Piloting>,
    mut speaker: Speaker,
    mut commands: Commands,
    mut acs: Query<(&mut Pilot, &Transform, Option<&PlayerHealth>)>,
    hardpoints: Query<(&Hardpoint, &GlobalTransform)>,
    mut targets: Query<
        (Entity, &Target, &mut TargetHealth, &mut Transform),
        Without<Pilot>,
    >,
    mut props: Query<&mut crate::map::DestructibleProp>,
) {
    if !piloting.0 {
        return;
    }
    let Ok((mut pilot, ac_tf, player_health)) = acs.single_mut() else {
        return;
    };
    if player_health.is_some_and(|h| h.destroyed) {
        return;
    }

    let cur_pos = ac_tf.translation;
    let prev_pos = pilot.prev_position;
    let horiz_kmh = pilot.horiz_speed_kmh();
    let total_weight = pilot.ctrl.total_weight;

    // Keep blade homing target position fresh while target is alive.
    if let Some(blade) = pilot.blade.as_mut() {
        if let Some((tent, _)) = blade.launch.target {
            if let Ok((_, _, health, tf)) = targets.get(tent) {
                if !health.destroyed {
                    blade.target_pos = Some(tf.translation + Vec3::Y * health.center_offset_y());
                } else {
                    blade.target_pos = None;
                }
            }
        }
    }

    // 1. Boost Charge Kick hit sweep (`BULLET_BULLETEXPLOSION_BIN` row 4100 = 7.0 m sphere,
    //    `PARAM_BULLETHITSFXPARAM_BIN` / `PARAM_BULLETHITSEPARAM_BIN` row 99000).
    if let Some(kick) = pilot.kick {
        if !kick.hit_applied {
            let kick_radius = find(BULLET_BULLETEXPLOSION_BIN, 4100)
                .map_or(7.0, |r| r.data.init_radius.max(3.0));
            let offset = Vec3::Y * 4.0 + kick.dir * 2.5;
            let a = prev_pos + offset;
            let b = cur_pos + offset;

            let mut best: Option<(f32, Entity, Vec3)> = None;
            for (entity, _, health, tf) in &targets {
                if health.destroyed {
                    continue;
                }
                for sphere in &health.spheres {
                    let center = tf.translation + Vec3::Y * sphere.offset_y;
                    if let Some((t, hit_pos, _)) =
                        damage::swept_sphere_hits_sphere(a, b, kick_radius, center, sphere.radius)
                    {
                        if best.is_none_or(|(bt, _, _)| t < bt) {
                            best = Some((t, entity, hit_pos));
                        }
                    }
                }
            }

            // Also shatter any destructible map props swept by the kick sphere.
            let (kick_dmg, _) = SpeedWeightScale::kick().scale(
                pilot.ctrl.kick_power,
                pilot.ctrl.kick_recoil,
                horiz_kmh,
                total_weight,
            );
            for mut prop in &mut props {
                if !prop.broken
                    && damage::swept_sphere_hits_sphere(
                        a,
                        b,
                        kick_radius,
                        prop.center,
                        prop.radius,
                    )
                    .is_some()
                {
                    prop.apply_damage(kick_dmg.max(1.0));
                }
            }

            if let Some((_, entity, hit_pos)) = best {
                let (power, recoil) = SpeedWeightScale::kick().scale(
                    pilot.ctrl.kick_power,
                    pilot.ctrl.kick_recoil,
                    horiz_kmh,
                    total_weight,
                );
                if let Ok((_, target, mut health, mut tf)) = targets.get_mut(entity) {
                    let (res, lethal) = health.apply_hit(DamageType::Ke, power, recoil);
                    health.record_hit_from(cur_pos);
                    if !lethal {
                        // Knockback along kick direction (`NewAcBehavior +0x524`).
                        tf.translation += kick.dir * 5.0;
                    }
                    let (kick_fx, kick_se) = fx_and_hit_se_for(99000);
                    let ffx = if res.effective {
                        kick_fx.hit_hi
                    } else {
                        kick_fx.hit_low
                    };
                    if ffx > 0 {
                        commands.spawn((Sfx::new(ffx), Transform::from_translation(hit_pos)));
                    }
                    let se = if res.effective {
                        kick_se.hi
                    } else {
                        kick_se.low
                    };
                    if se > 0 {
                        sound::shot(&mut commands, &mut speaker.cues, &mut speaker.sources, se);
                    }
                    info!(
                        "BOOST KICK {} ({}) at {:.1} km/h: {} {:.0} dmg (recoil {:.0}, AP {:.0}/{:.0}){}{}",
                        target.name,
                        target.model,
                        horiz_kmh,
                        if res.effective { "EFFECTIVE" } else { "RICOCHET" },
                        res.damage,
                        res.recoil,
                        health.ap,
                        health.ap_max,
                        if health.is_staggered() { " STAGGER" } else { "" },
                        if lethal { " DESTROYED" } else { "" },
                    );
                }
                pilot.on_kick_contact();
                return;
            }
        }
    }

    // 2. Melee Blade active swing sweep (`BladePhase::Swing`, `BULLET_BULLETBLADE_BIN`, `0x828a4928`).
    if let Some(mut blade) = pilot.blade.take() {
        if blade.phase == BladePhase::Swing {
            let hand = if blade.launch.hand_idx == 0 {
                Hand::Right
            } else {
                Hand::Left
            };
            let forward = (Quat::from_rotation_y(pilot.yaw) * Vec3::NEG_Z).normalize_or(Vec3::NEG_Z);
            let origin = hardpoints
                .iter()
                .find(|(h, _)| h.hand == hand)
                .map_or(cur_pos + Vec3::Y * 4.5 + forward * 2.0, |(_, t)| t.translation());

            // Spawn blade trail / laser beam FFX once on entering Swing (e.g. FFX 4104).
            if !blade.trail_spawned {
                blade.trail_spawned = true;
                if blade.launch.trail_fx > 0 {
                    commands.spawn((
                        Sfx::new(blade.launch.trail_fx),
                        Transform::from_translation(origin).looking_to(forward, Vec3::Y),
                    ));
                }
            }

            let reach = blade.launch.hit_radius.clamp(6.0, 35.0);
            let sweep_r = blade.launch.blade_radius.max(2.0);
            let tip = origin + forward * reach;
            let max_hits = blade.launch.max_hits.max(1) as usize;

            for mut prop in &mut props {
                if !prop.broken
                    && damage::swept_sphere_hits_sphere(
                        origin,
                        tip,
                        sweep_r,
                        prop.center,
                        prop.radius,
                    )
                    .is_some()
                {
                    prop.apply_damage(blade.launch.damage_power.max(1.0));
                }
            }

            for (entity, target, mut health, tf) in &mut targets {
                if blade.hit_entities.len() >= max_hits {
                    break;
                }
                if health.destroyed || blade.hit_entities.contains(&entity) {
                    continue;
                }
                let mut hit_pos = None;
                for sphere in &health.spheres {
                    let center = tf.translation + Vec3::Y * sphere.offset_y;
                    if let Some((_, p, _)) =
                        damage::swept_sphere_hits_sphere(origin, tip, sweep_r, center, sphere.radius)
                    {
                        hit_pos = Some(p);
                        break;
                    }
                }
                let Some(hit_pos) = hit_pos else { continue };

                let (mut power, mut recoil) = if blade.launch.physical {
                    SpeedWeightScale::physical_blade().scale(
                        blade.launch.damage_power,
                        blade.launch.damage_recoil,
                        horiz_kmh,
                        total_weight,
                    )
                } else {
                    (blade.launch.damage_power, blade.launch.damage_recoil)
                };
                if !blade.hit_entities.is_empty() {
                    power *= blade.launch.decay_power.clamp(0.0, 1.0);
                    recoil *= blade.launch.decay_recoil.clamp(0.0, 1.0);
                }
                let (res, lethal) = health.apply_hit(blade.launch.damage_type, power, recoil);
                health.record_hit_from(cur_pos);
                let ffx = if res.effective {
                    blade.launch.hit_hi_fx
                } else {
                    blade.launch.hit_low_fx
                };
                if ffx > 0 {
                    commands.spawn((Sfx::new(ffx), Transform::from_translation(hit_pos)));
                }
                let se = if res.effective {
                    blade.launch.hit_hi_se
                } else {
                    blade.launch.hit_low_se
                };
                if se > 0 {
                    sound::shot(&mut commands, &mut speaker.cues, &mut speaker.sources, se);
                }
                blade.hit_entities.push(entity);
                pilot.blade_hits += 1;
                info!(
                    "BLADE {} ({}) ({}): {} {:.0} dmg (recoil {:.0}, AP {:.0}/{:.0}){}{}",
                    target.name,
                    target.model,
                    if blade.launch.physical { "physical/pile" } else { "laser" },
                    if res.effective { "EFFECTIVE" } else { "RICOCHET" },
                    res.damage,
                    res.recoil,
                    health.ap,
                    health.ap_max,
                    if health.is_staggered() { " STAGGER" } else { "" },
                    if lethal { " DESTROYED" } else { "" },
                );
            }
        }
        pilot.blade = Some(blade);
    }
}

pub(crate) fn silent() -> Gun {
    Gun {
        remaining: 0,
        magazine: 0,
        reload_time: 0.0,
        lock_time_ticks: 0.0,
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
        snipe_fov: 0.0,
        snipe_turn_scale: 1.0,
        reload_stab_factor: 1.0,
        fire_mode: FireMode::Auto,
        charge_time: 0.0,
        charge_ticks: 0.0,
        charge_se: 0,
        burst_wait: 0.0,
        burst_remaining: 0,
        burst_timer: 0.0,
        trigger_held: false,
    }
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
    let is_missile = part_field(id, category, "is_missile") == 1.0;
    let base_burst = (part_field(id, category, "burst_count") as u8).max(1);
    let burst_count = if category == 12 {
        base_burst.saturating_mul(arms_mount.max(1))
    } else {
        base_burst
    };
    let lock_once = (part_field(id, category, "lock_once") as u8).max(1);
    let lock_use_burst = part_field(id, category, "lock_use_burst") != 0.0;
    // `0x82478cd0`: if sub +0x1c != 0, max weapon locks = burst_count, else lock_once.max(1).
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
        // `0x82475b00` / `0x8247d6e0`: charge-to-fire weapon (laser rifles, rail/laser cannons).
        FireMode::Charge
    } else if category == 10
        && burst_count > 1
        && !is_missile
        && (burst_wait > 0.0 || burst_interval > 0.0)
    {
        // Multi-round sequential burst (`0x82475c98` / `sub +0x20` / `sub +0x22`: burst rifles, burst handguns).
        FireMode::Burst
    } else if category == 10 && burst_count > 1 && !is_missile {
        // Simultaneous multi-pellet cone spread (shotguns, pulse guns/cannons, scatter cannons).
        FireMode::PelletSpread
    } else {
        FireMode::Auto
    };
    let reload_stab_factor = if category == 11 {
        1.0
    } else {
        part_field(id, category, "reload_stab_factor")
    };
    Gun {
        remaining: magazine,
        magazine,
        reload_time: part_field(id, category, "reload_time"),
        lock_time_ticks: part_field(id, category, "missile_lock_time"),
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
        shoot: shoot_of_hit(hit_id),
        hit_se,
        stance: category == 10 && part_field(id, 10, "ready_position") != 0.0,
        sniper: category == 10 && part_field(id, 10, "weapon_kind") == 5.0,
        snipe_fov: if category == 10 {
            part_field(id, 10, "snipe_fov")
        } else {
            0.0
        },
        snipe_turn_scale: if category == 10 {
            let s = part_field(id, 10, "snipe_turn_scale");
            if s > 0.0 { s } else { 1.0 }
        } else {
            1.0
        },
        reload_stab_factor,
        fire_mode,
        charge_time,
        charge_ticks: 0.0,
        charge_se: charge_of_hit(hit_id),
        burst_wait,
        burst_remaining: 0,
        burst_timer: 0.0,
        trigger_held: false,
    }
}

fn gun(id: i64) -> Gun {
    gun_from_category(id, 10, 1)
}

fn ow_gun(id: i64) -> Gun {
    gun_from_category(id, 11, 1)
}

fn shoulder_gun(id: i64, arms_mount: u8) -> Gun {
    gun_from_category(id, 12, arms_mount)
}

/// Resolves an OW secondary blast row (`BULLET_BULLETARISE_BIN` -> `BULLET_BULLETEXPLOSION_BIN` or
/// `BULLET_BULLETSTATUSERROR_BIN`, e.g. Huge Cannon `99615` 50 m, Multiple Pulse `99815` 30 m,
/// Giga Missile `101010` 100 m).
fn impact_blast_from_arise(arise_id: u32) -> Option<ImpactBlast> {
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

/// Decodes one category-10 enemy weapon record from `param/enemy/parts/acvenemyparts.bin`
/// (`0x824a14e4` -> `0x824a0430`, identical category-10 weapon sub-record offsets to `acvparts.bin`).
fn enemy_gun_from_record(data: &[u8]) -> Gun {
    let be = Be(data);
    let Ok(bullet_id) = be.u32(316) else {
        return silent();
    };
    if bullet_id == 0 {
        return silent();
    }
    let hit_id = be.u32(320).unwrap_or(0);
    let init_speed = be.f32(328).unwrap_or(0.0);
    let (kind, flight, missile_flight, speed) = enemy_flight(bullet_id, init_speed);
    if kind == Kind::Skip {
        return silent();
    }
    let (fx, hit_se) = enemy_fx_and_hit_se_for(hit_id);
    let is_missile = be.u8(308).unwrap_or(0) == 1 || kind == Kind::Missile;
    let magazine = be.u16(374).unwrap_or(9999).max(1);
    let burst_count = be.u8(337).unwrap_or(1).max(1);
    let burst_interval = be.u16(342).unwrap_or(0) as f32;
    let reload_time = be.u16(344).unwrap_or(60) as f32;
    Gun {
        remaining: magazine,
        magazine,
        reload_time,
        lock_time_ticks: be.u16(314).unwrap_or(0) as f32,
        cooldown: 0.0,
        init_speed: speed,
        kind,
        flight,
        missile_flight,
        blade: None,
        is_missile,
        weapon_lock_max: if is_missile { 1 } else { 0 },
        burst_count,
        burst_interval,
        damage_type: DamageType::from_raw(be.u8(392).unwrap_or(0)),
        damage_power: be.f32(396).unwrap_or(0.0),
        damage_recoil: be.f32(400).unwrap_or(0.0),
        en_drain: 0.0,
        decay: RangeDecay {
            no_decay_range: be.u16(380).unwrap_or(0) as f32,
            damp_range: be.u16(312).unwrap_or(0) as f32,
            guarantee_rate: be.u8(382).unwrap_or(100) as f32,
            min_power_rate: be.u8(383).unwrap_or(100) as f32,
        },
        fx,
        shoot: enemy_shoot_of_hit(hit_id),
        hit_se,
        stance: false,
        sniper: false,
        snipe_fov: 0.0,
        snipe_turn_scale: 1.0,
        reload_stab_factor: 1.0,
        fire_mode: FireMode::Auto,
        charge_time: 0.0,
        charge_ticks: 0.0,
        charge_se: 0,
        burst_wait: 0.0,
        burst_remaining: 0,
        burst_timer: 0.0,
        trigger_held: false,
    }
}

fn enemy_shoot_of_hit(hit_id: u32) -> i16 {
    find(PARAM_ENEMYWEAPONSOUNDPARAM_BIN, hit_id)
        .or_else(|| find(PARAM_ACWEAPONSOUNDPARAM_BIN, hit_id))
        .map(|r| r.data.shoot)
        .unwrap_or(DEFAULT_SHOOT)
}

fn enemy_fx_and_hit_se_for(hit_id: u32) -> (Fx, HitSe) {
    let Some(row) = find(PARAM_ENEMYWEAPONSFXPARAM_BIN, hit_id)
        .or_else(|| find(PARAM_ACWEAPONSFXPARAM_BIN, hit_id))
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

fn enemy_flight(id: u32, init_speed: f32) -> (Kind, Flight, Option<MissileFlight>, f32) {
    if let Some(row) = find(PARAM_ENEMY_BULLET_ENEMYBULLETMISSILE_BIN, id)
        .or_else(|| find(BULLET_BULLETMISSILE_BIN, id))
    {
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
            split: (d.close_act_id == 8 && d.shoot_bullet_id > 0 && d.once_shoot_count > 0).then_some(
                MissileSplit {
                    force_trace_frame: d.force_trace_frame as f32,
                    start_frame: d.close_act_start_frame as f32,
                    start_dist: d.close_act_start_dist,
                    arise_id: d.shoot_bullet_id,
                    count: d.once_shoot_count,
                    shoot_speed: d.shoot_speed as f32,
                },
            ),
        };
        let spd = if init_speed > 0.0 {
            init_speed
        } else if mf.min_speed > 0.0 {
            mf.min_speed
        } else {
            speed(init_speed, d.max_speed_km_h).min(120.0)
        };
        return (Kind::Missile, Flight::default(), Some(mf), spd);
    }
    if let Some(row) = find(PARAM_ENEMY_BULLET_ENEMYBULLETRIGID_BIN, id)
        .or_else(|| find(BULLET_BULLETRIGID_BIN, id))
    {
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
    if let Some(row) = find(PARAM_ENEMY_BULLET_ENEMYBULLETENERGY_BIN, id)
        .or_else(|| find(BULLET_BULLETENERGY_BIN, id))
    {
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

fn shoot_of_hit(hit_id: u32) -> i16 {
    find(PARAM_ACWEAPONSOUNDPARAM_BIN, hit_id)
        .map(|r| r.data.shoot)
        .unwrap_or(DEFAULT_SHOOT)
}

fn charge_of_hit(hit_id: u32) -> i16 {
    find(PARAM_ACWEAPONSOUNDPARAM_BIN, hit_id)
        .map(|r| r.data.charge)
        .filter(|&c| c > 0)
        .unwrap_or(0)
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
            // `0x8247f170`: physical blade / heat pile speed×weight scaling applies when
            // `damage_type != 1 (TE)`.
            physical: damage_type != DamageType::Te,
            infinite_ammo: magazine == 0,
        };
        return (Kind::Blade, Flight::default(), None, Some(spec), 0.0);
    }
    let (kind, fl, mf, spd) = flight(id, init_speed);
    (kind, fl, mf, None, spd)
}

fn flight(id: u32, init_speed: f32) -> (Kind, Flight, Option<MissileFlight>, f32) {
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
            split: (d.close_act_id == 8 && d.shoot_bullet_id > 0 && d.once_shoot_count > 0).then_some(
                MissileSplit {
                    force_trace_frame: d.force_trace_frame as f32,
                    start_frame: d.close_act_start_frame as f32,
                    start_dist: d.close_act_start_dist,
                    arise_id: d.shoot_bullet_id,
                    count: d.once_shoot_count,
                    shoot_speed: d.shoot_speed as f32,
                },
            ),
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

fn speed(init: f32, max_km_h: u16) -> f32 {
    if init > 0.0 {
        init
    } else {
        max_km_h as f32 / KMH_PER_MS
    }
}

/// `a00_000` stowed, `a00_001` deploy, `a00_002` fire, `a00_003` stow.
const ROLES: [&str; 4] = ["a00_000.ani", "a00_001.ani", "a00_002.ani", "a00_003.ani"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Stowed,
    Deploy,
    Out,
    Fire,
    Stow,
}

/// Which clip is showing. `frames[role]` is 0 when that clip is absent.
struct Playback {
    ready: bool,
    frames: [f32; 4],
    phase: Phase,
    role: usize,
    frame: f32,
    playing: bool,
}

impl Playback {
    fn new(ready: bool, frames: [u32; 4]) -> Self {
        let frames = frames.map(|n| if n == 0 { 0.0 } else { n as f32 });
        let mut play = Self {
            ready,
            frames,
            phase: Phase::Stowed,
            role: 0,
            frame: 0.0,
            playing: false,
        };
        if play.has(0) {
            play.hold(0, 0.0);
        }
        play
    }

    fn has(&self, role: usize) -> bool {
        self.frames[role] > 0.0
    }

    /// Ready-position weapons that actually have a deploy clip wait out that clip before a shot.
    fn deploys(&self) -> bool {
        self.ready && self.has(1)
    }

    fn hold(&mut self, role: usize, frame: f32) {
        (self.role, self.frame, self.playing) = (role, frame, false);
    }

    fn play(&mut self, role: usize) {
        (self.role, self.frame, self.playing) = (role, 0.0, true);
    }

    fn advance(&mut self, dt_frames: f32) {
        if !self.playing {
            return;
        }
        let end = (self.frames[self.role] - 1.0).max(0.0);
        self.frame = (self.frame + dt_frames).min(end);
        if self.frame >= end {
            self.playing = false;
        }
    }

    /// Advances the clip. Returns whether a round leaves the weapon this tick.
    fn tick(&mut self, want: bool, can_shoot: bool, dt_frames: f32) -> bool {
        if !self.deploys() {
            self.advance(dt_frames);
            if self.role == 2 && !self.playing && self.has(0) {
                self.hold(0, 0.0);
            }
            if want && can_shoot {
                if self.has(2) {
                    self.play(2);
                }
                return true;
            }
            return false;
        }

        self.advance(dt_frames);
        if self.phase == Phase::Deploy && !self.playing {
            self.phase = Phase::Out;
            self.hold(1, (self.frames[1] - 1.0).max(0.0));
        }
        if self.phase == Phase::Fire && !self.playing {
            self.phase = Phase::Out;
            self.hold(1, (self.frames[1] - 1.0).max(0.0));
        }
        if self.phase == Phase::Stow && !self.playing {
            self.phase = Phase::Stowed;
            if self.has(0) {
                self.hold(0, 0.0);
            }
        }
        if !want && matches!(self.phase, Phase::Deploy | Phase::Out | Phase::Fire) {
            if self.has(3) {
                self.phase = Phase::Stow;
                self.play(3);
            } else {
                self.phase = Phase::Stowed;
                if self.has(0) {
                    self.hold(0, 0.0);
                }
            }
            return false;
        }
        if want && matches!(self.phase, Phase::Stowed | Phase::Stow) {
            self.phase = Phase::Deploy;
            self.play(1);
            return false;
        }
        if want && can_shoot && matches!(self.phase, Phase::Out | Phase::Fire) {
            if self.has(2) {
                self.phase = Phase::Fire;
                self.play(2);
            }
            return true;
        }
        false
    }
}

struct WeaponRig {
    hand: Hand,
    /// On the rack until a bay shift brings it to the hand.
    bay: bool,
    /// The weapon was purged: it stays off the hand and off the rack.
    retired: bool,
    playback: Playback,
    /// Pose over the locomotion clip. Weapon meshes leave body-driven joints alone.
    body: bool,
    /// `(joint, bone index in the weapon skeleton)`.
    joints: Vec<(Entity, usize)>,
    skeleton: ani::Anim,
    clips: [Option<ani::Anim>; 4],
}

/// Base clip ID inside a weapon arm's `am*_a.bnd.dcx` for `leg_type` (360 `0x82882d28`):
/// biped (`0..=2`) -> `0`, reverse joint (`3..=4`) -> `10`, quad (`5`) -> `20`, tank (`6`) -> `30`.
pub(crate) fn weapon_arm_clip_base(leg_type: u8) -> u32 {
    match leg_type {
        0..=2 => 0,
        3..=4 => 10,
        5 => 20,
        6 => 30,
        _ => 0,
    }
}

/// Playback phase of a weapon arm's `am*_a.bnd.dcx` rig (360 `WeaponArmCtrl` `0x820930e8`,
/// `0x8284cecc`, `0x82847fc8`, `0x8289d890`, `0x8289d908`, `0x8289da68`, `0x8289dab8`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WeaponArmPhase {
    /// Folded / stowed (`base + 0`: `a00_000` / `010` / `020` / `030`). Garage & Scan Mode.
    Stowed,
    /// Transforming from folded to combat-ready (`base + 4`: `a00_004` / `014` / `024` / `034`).
    Deploying,
    /// Combat-ready idle (`base + 1`: `a00_001` / `011` / `021` / `031`).
    Deployed,
    /// Holding trigger / aiming (`base + 3`: `a00_003` / `013` / `023` / `033`).
    Ready,
    /// Firing / recoil (`base + 2`: `a00_002` / `012` / `022` / `032`, optional).
    Firing,
    /// Transforming from combat-ready back to folded (`base + 5`: `a00_005` / `015` / `025` / `035`).
    Stowing,
}

/// Dedicated animation rig for weapon arms (`acvparts` cat-2 `+0x13c == 1`, 360 `0x824793c8`).
/// Unlike standard arms (whose `*_arm01..*_armhand` follow the body locomotion clip), weapon arms
/// drive ALL non-root bones (`*_armwepunit_*`, `*_armroot`, `*_arm01..*_armhand`, `*_armhandu*`)
/// directly from `am*_a.bnd.dcx`, leaving only the shoulder roots (`l_kata`, `r_kata`) on the core.
struct WeaponArmRig {
    joints: Vec<(Entity, usize)>,
    skeleton: ani::Anim,
    stowed: ani::Anim,
    deploy: Option<ani::Anim>,
    stow: Option<ani::Anim>,
    deployed: Option<ani::Anim>,
    fire: Option<ani::Anim>,
    ready: Option<ani::Anim>,
    phase: WeaponArmPhase,
    frame: f32,
}

impl WeaponArmRig {
    fn load(
        disc: &Disc,
        placement: &Placement,
        rig: &Rig,
        joints: &[Entity],
        leg_type: u8,
    ) -> Option<Self> {
        let binder = anim_binder(placement.model.path)?;
        let assets: Vec<_> = acvd_data::generated::assets::ALL
            .iter()
            .flat_map(|g| g.iter())
            .filter(|a| a.binder == binder && a.ext == ".ani")
            .collect();
        let skel_asset = assets.iter().find(|a| a.entry == "a00_000.ani")?;
        let skeleton = ani::read(&vfs::open(disc, &skel_asset.path()).ok()?).ok()?;
        if skeleton.bones.iter().all(|b| b.rest.is_none()) {
            return None;
        }
        let load_clip = |id: u32, fallback: Option<u32>| -> Option<ani::Anim> {
            let try_id = |cid: u32| -> Option<ani::Anim> {
                let entry = format!("a00_{cid:03}.ani");
                let asset = assets.iter().find(|a| a.entry == entry)?;
                let clip = ani::read(&vfs::open(disc, &asset.path()).ok()?).ok()?;
                (clip.bones.len() == skeleton.bones.len() && clip.frames > 0).then_some(clip)
            };
            try_id(id).or_else(|| fallback.and_then(try_id))
        };
        let base = weapon_arm_clip_base(leg_type);
        let stowed = load_clip(base, Some(0)).unwrap_or_else(|| skeleton.clone());
        let deployed = load_clip(base + 1, Some(1));
        let fire = load_clip(base + 2, Some(2));
        let ready = load_clip(base + 3, Some(3));
        let deploy = load_clip(base + 4, Some(4));
        let stow = load_clip(base + 5, Some(5));

        // Bind every non-root FLVER bone (`l_kata` and `r_kata` stay mounted on `core`).
        let mut pairs = Vec::new();
        for (i, bone) in skeleton.bones.iter().enumerate() {
            let Some(name) = bone.rest.as_ref().map(|r| r.name.as_str()) else {
                continue;
            };
            let Some(b) = rig.bones.iter().position(|x| x.name == name) else {
                continue;
            };
            if rig.bones[b].parent.is_none() {
                continue;
            }
            if let Some(&entity) = joints.get(b) {
                pairs.push((entity, i));
            }
        }
        if pairs.is_empty() {
            return None;
        }
        Some(Self {
            joints: pairs,
            skeleton,
            stowed,
            deploy,
            stow,
            deployed,
            fire,
            ready,
            phase: WeaponArmPhase::Stowed,
            frame: 0.0,
        })
    }

    fn active_clip(&self) -> &ani::Anim {
        match self.phase {
            WeaponArmPhase::Stowed => &self.stowed,
            WeaponArmPhase::Deploying => self.deploy.as_ref().unwrap_or(&self.stowed),
            WeaponArmPhase::Deployed => self.deployed.as_ref().unwrap_or(&self.stowed),
            WeaponArmPhase::Ready => self
                .ready
                .as_ref()
                .or(self.deployed.as_ref())
                .unwrap_or(&self.stowed),
            WeaponArmPhase::Firing => self
                .fire
                .as_ref()
                .or(self.ready.as_ref())
                .or(self.deployed.as_ref())
                .unwrap_or(&self.stowed),
            WeaponArmPhase::Stowing => self.stow.as_ref().unwrap_or(&self.stowed),
        }
    }

    /// Advances the weapon arm state machine by `frames` 60 Hz ticks.
    fn tick(&mut self, combat_mode: bool, want_fire: bool, fired: bool, frames: f32) {
        if !combat_mode {
            match self.phase {
                WeaponArmPhase::Stowed => {
                    let len = self.stowed.frames.max(1) as f32;
                    self.frame = (self.frame + frames) % len;
                }
                WeaponArmPhase::Stowing => {
                    let len = self.stow.as_ref().map_or(1, |c| c.frames.max(1)) as f32;
                    self.frame += frames * 2.0;
                    if self.frame >= len - 1.0 {
                        self.phase = WeaponArmPhase::Stowed;
                        self.frame = 0.0;
                    }
                }
                _ => {
                    if self.stow.is_some() {
                        self.phase = WeaponArmPhase::Stowing;
                        self.frame = 0.0;
                    } else {
                        self.phase = WeaponArmPhase::Stowed;
                        self.frame = 0.0;
                    }
                }
            }
            return;
        }

        // Combat mode: unfold via `base+4`, then hold `base+1`, ready `base+3`, fire `base+2`.
        if fired && !matches!(self.phase, WeaponArmPhase::Deploying) {
            if self.fire.is_some() {
                self.phase = WeaponArmPhase::Firing;
                self.frame = 0.0;
            } else if self.ready.is_some() {
                self.phase = WeaponArmPhase::Ready;
                self.frame = 0.0;
            }
        }
        match self.phase {
            WeaponArmPhase::Stowed | WeaponArmPhase::Stowing => {
                if self.deploy.is_some() {
                    self.phase = WeaponArmPhase::Deploying;
                    self.frame = 0.0;
                } else {
                    self.phase = WeaponArmPhase::Deployed;
                    self.frame = 0.0;
                }
            }
            WeaponArmPhase::Deploying => {
                let len = self.deploy.as_ref().map_or(1, |c| c.frames.max(1)) as f32;
                let speed = if want_fire || fired { 3.0 } else { 2.0 };
                self.frame += frames * speed;
                if self.frame >= len - 1.0 {
                    self.phase = WeaponArmPhase::Deployed;
                    self.frame = 0.0;
                }
            }
            WeaponArmPhase::Firing => {
                let len = self.active_clip().frames.max(1) as f32;
                self.frame += frames;
                if self.frame >= len - 1.0 {
                    self.phase = if want_fire && self.ready.is_some() {
                        WeaponArmPhase::Ready
                    } else {
                        WeaponArmPhase::Deployed
                    };
                    self.frame = 0.0;
                }
            }
            WeaponArmPhase::Ready => {
                let len = self.active_clip().frames.max(1) as f32;
                self.frame = (self.frame + frames).min(len - 1.0);
                if !want_fire {
                    self.phase = WeaponArmPhase::Deployed;
                    self.frame = 0.0;
                }
            }
            WeaponArmPhase::Deployed => {
                if want_fire && self.ready.is_some() {
                    self.phase = WeaponArmPhase::Ready;
                    self.frame = 0.0;
                } else {
                    let len = self.active_clip().frames.max(1) as f32;
                    self.frame = (self.frame + frames) % len;
                }
            }
        }
    }

    fn sample_joint(&self, clip: &ani::Anim, bone: usize) -> Transform {
        let rest = self.skeleton.bones.get(bone).and_then(|b| b.rest.as_ref());
        let track = clip.bones.get(bone).map(|b| &b.track);
        let rotation = track
            .and_then(|t| t.rotation(self.frame))
            .or_else(|| rest.map(|r| ani::euler_quat(r.euler)))
            .unwrap_or([0.0, 0.0, 0.0, 1.0]);
        let translation = track
            .and_then(|t| t.translation(self.frame))
            .unwrap_or(rest.map(|r| r.translation).unwrap_or([0.0; 3]));
        let scale = track
            .and_then(|t| t.scale(self.frame))
            .unwrap_or(rest.map(|r| r.scale).unwrap_or([1.0; 3]));
        pose::to_transform(
            Vec3::from(translation),
            Quat::from_array(rotation),
            Vec3::from(scale),
        )
    }

    fn sample(&self) -> Vec<(Entity, Transform)> {
        let clip = self.active_clip();
        self.joints
            .iter()
            .map(|&(entity, bone)| (entity, self.sample_joint(clip, bone)))
            .collect()
    }

    fn apply(&self, posed: &mut Query<(&mut Transform, Option<&Driven>), With<ClipJoint>>) {
        let clip = self.active_clip();
        for &(entity, bone) in &self.joints {
            if let Ok((mut current, _)) = posed.get_mut(entity) {
                *current = self.sample_joint(clip, bone);
            }
        }
    }
}

impl SniperStance {
    fn update(&mut self, want: Option<Hand>, motion: &mut Motion, disc: &Disc, pilot: &mut Pilot) {
        match want {
            Some(hand) => {
                let restart = self.phase == SniperPhase::Off
                    || self.phase == SniperPhase::Release
                    || self.hand != hand;
                if restart {
                    self.hand = hand;
                    match play_stance(hand, false, motion, disc, pilot) {
                        StancePlay::Upper => {
                            self.phase = SniperPhase::Engage;
                            pilot.body_held(false);
                        }
                        StancePlay::Full => {
                            self.phase = SniperPhase::Engage;
                            pilot.body_held(true);
                        }
                        StancePlay::None => {
                            self.phase = SniperPhase::Hold;
                            pilot.body_held(false);
                        }
                    }
                } else if self.phase == SniperPhase::Engage
                    && (motion.upper_finished() && (motion.upper.is_some() || motion.finished()))
                {
                    self.phase = SniperPhase::Hold;
                }
            }
            None => match self.phase {
                SniperPhase::Off => {}
                SniperPhase::Release
                    if motion.upper_finished() && (motion.upper.is_some() || motion.finished()) =>
                {
                    self.phase = SniperPhase::Off;
                    motion.clear_upper(0.15);
                    pilot.body_held(false);
                }
                SniperPhase::Release => {}
                SniperPhase::Engage | SniperPhase::Hold => {
                    match play_stance(self.hand, true, motion, disc, pilot) {
                        StancePlay::Upper => {
                            self.phase = SniperPhase::Release;
                            pilot.body_held(false);
                        }
                        StancePlay::Full => {
                            self.phase = SniperPhase::Release;
                            pilot.body_held(true);
                        }
                        StancePlay::None => {
                            self.phase = SniperPhase::Off;
                            motion.clear_upper(0.15);
                            pilot.body_held(false);
                        }
                    }
                }
            },
        }
    }

    /// The engage clip has reached its last frame, so a round may leave.
    fn allows(&self, hand: Hand) -> bool {
        self.phase == SniperPhase::Hold && self.hand == hand
    }
}

enum StancePlay {
    None,
    Upper,
    Full,
}

/// `sniper_ready_*` / `sniper_stow_*` in `sheets/ac_states.csv`.
/// Upper-body-only rows (`b_apply_lower == 0`) play on the upper-body layer so leg locomotion
/// continues uninterrupted.
fn play_stance(
    hand: Hand,
    release: bool,
    motion: &mut Motion,
    disc: &Disc,
    pilot: &Pilot,
) -> StancePlay {
    let state = match (hand, release) {
        (Hand::Right, false) => "sniper_ready_r",
        (Hand::Right, true) => "sniper_stow_r",
        (Hand::Left, false) => "sniper_ready_l",
        (Hand::Left, true) => "sniper_stow_l",
    };
    let Some(row) = ac_state(state, None).and_then(|s| find(PARAM_ACMOTION_BIN, s.row)) else {
        return StancePlay::None;
    };
    if row.data.b_apply_lower == 0 {
        if motion
            .play_upper(
                disc,
                row.data.anim_id,
                row.data.b_loop != 0,
                1.0,
                pilot.fade(row.data.interpolate_id),
                row.data.upper_priority.max(1),
            )
            .is_ok()
        {
            return StancePlay::Upper;
        }
    } else if motion
        .play(
            disc,
            row.data.anim_id,
            row.data.b_loop != 0,
            1.0,
            pilot.fade(row.data.interpolate_id),
        )
        .is_ok()
    {
        return StancePlay::Full;
    }
    StancePlay::None
}

impl WeaponAnims {
    /// The weapon that was in the hand and the one on the bay trade places. A purge retires
    /// the hand weapon instead of parking it on the rack.
    pub(crate) fn transfer(&mut self, hand: Hand, purge: bool) {
        for rig in &mut self.rigs {
            if rig.hand != hand || rig.retired {
                continue;
            }
            if purge && !rig.bay {
                rig.retired = true;
            } else {
                rig.bay = !rig.bay;
            }
        }
    }

    /// Loads hand and bay `a00` clips, the arm deploy clips on that side, and the rifle kick bones.
    pub fn load(
        &mut self,
        disc: &Disc,
        placement: &Placement,
        rig: &Rig,
        joints: &[Entity],
        leg_type: u8,
    ) {
        let arm = placement.column == "arms";
        if arm && part_field(placement.part as i64, 2, "is_weapon_arm") != 0.0 {
            self.weapon_arm = WeaponArmRig::load(disc, placement, rig, joints, leg_type);
            return;
        }
        let placed = placement_place(placement.column);
        let hands: &[Hand] = if arm {
            &[Hand::Right, Hand::Left]
        } else if let Some((hand, _)) = placed {
            match hand {
                Hand::Right => &[Hand::Right],
                Hand::Left => &[Hand::Left],
            }
        } else {
            return;
        };
        let bay = placed.is_some_and(|(_, bay)| bay);
        // `arm_l` and `arm_r` share one placement (assembly_slots.csv column `arms`).
        if arm {
            let controls = vfs::open(disc, "param/jcondata.bin")
                .and_then(|d| jcon::read(&d))
                .unwrap_or_default();
            for &hand in hands {
                self.note_recoil(hand, &controls, rig, joints);
            }
        }
        let Some(binder) = anim_binder(placement.model.path) else {
            return;
        };
        let assets: Vec<_> = acvd_data::generated::assets::ALL
            .iter()
            .flat_map(|g| g.iter())
            .filter(|a| a.binder == binder && a.ext == ".ani")
            .collect();
        let Some(skeleton_asset) = assets.iter().find(|a| a.entry == ROLES[0]) else {
            return;
        };
        let Ok(bytes) = vfs::open(disc, &skeleton_asset.path()) else {
            return;
        };
        let Ok(skeleton) = ani::read(&bytes) else {
            return;
        };
        if skeleton.bones.iter().all(|b| b.rest.is_none()) {
            return;
        }
        let mut clips: [Option<ani::Anim>; 4] = [None, None, None, None];
        let mut frames = [0u32; 4];
        for (role, entry) in ROLES.iter().enumerate() {
            let Some(asset) = assets.iter().find(|a| a.entry == *entry) else {
                continue;
            };
            let Ok(bytes) = vfs::open(disc, &asset.path()) else {
                continue;
            };
            let Ok(clip) = ani::read(&bytes) else {
                continue;
            };
            if clip.bones.len() != skeleton.bones.len() || clip.frames == 0 {
                continue;
            }
            frames[role] = clip.frames;
            clips[role] = Some(clip);
        }
        if frames.iter().all(|&n| n == 0) {
            return;
        }
        for &hand in hands {
            let side = match hand {
                Hand::Right => "r_",
                Hand::Left => "l_",
            };
            let mut pairs = Vec::new();
            for (i, bone) in skeleton.bones.iter().enumerate() {
                let Some(name) = bone.rest.as_ref().map(|r| r.name.as_str()) else {
                    continue;
                };
                if arm && !name.starts_with(side) {
                    continue;
                }
                if let Some(b) = rig.bones.iter().position(|x| x.name == name) {
                    // The part root's local is the socket mount. A weapon clip's root sits at
                    // the origin, and writing it would pull the gun off the dummy.
                    if !arm && rig.bones[b].parent.is_none() {
                        continue;
                    }
                    if let Some(&entity) = joints.get(b) {
                        pairs.push((entity, i));
                    }
                }
            }
            if pairs.is_empty() && !arm && skeleton.bones.len() == rig.bones.len() {
                pairs = (0..rig.bones.len())
                    .filter(|i| rig.bones[*i].parent.is_some())
                    .filter_map(|i| joints.get(i).copied().map(|entity| (entity, i)))
                    .collect();
            }
            if pairs.is_empty() || (arm && frames[1] == 0) {
                continue;
            }
            let ready = arm
                || part_field(placement.part as i64, placement.category, "ready_position") != 0.0;
            let built = WeaponRig {
                hand,
                bay,
                retired: false,
                playback: Playback::new(ready, frames),
                body: arm,
                joints: pairs,
                skeleton: skeleton.clone(),
                clips: clips.clone(),
            };
            if arm {
                self.arms.push(built);
            } else {
                self.rigs.push(built);
            }
        }
    }

    /// Kicking bones of this arm's `gun_$(LR)` and `sniper_$(LR)` controls. The kick axis is
    /// not a field; LockMinX/LockMaxX is the wide limit on `*_arm01`, so it is a local X rotation.
    fn note_recoil(
        &mut self,
        hand: Hand,
        controls: &[jcon::Control],
        rig: &Rig,
        joints: &[Entity],
    ) {
        let side = match hand {
            Hand::Right => "R",
            Hand::Left => "L",
        };
        for (sniper, prefix) in [(false, "gun_"), (true, "sniper_")] {
            let name = format!("{prefix}{side}");
            let Some(control) = controls.iter().find(|c| c.name == name) else {
                continue;
            };
            if !sniper {
                if let Some(object) = control.objects.iter().find(|o| o.bone.ends_with("_arm01")) {
                    let aim = &mut self.aim[hand as usize];
                    aim.shoulder = rig
                        .bones
                        .iter()
                        .position(|b| b.name == object.bone)
                        .and_then(|i| joints.get(i).copied());
                    aim.limit = object.lock[0][0].abs().max(object.lock[0][1].abs());
                }
            }
            for object in control
                .objects
                .iter()
                .filter(|o| o.react_ang != 0.0 && o.react_time > 0.0)
            {
                let Some(entity) = rig
                    .bones
                    .iter()
                    .position(|b| b.name == object.bone)
                    .and_then(|i| joints.get(i).copied())
                else {
                    continue;
                };
                self.recoil_bones.push(RecoilBone {
                    entity,
                    hand,
                    sniper,
                    kick: Kick::new(object.react_ang, object.react_delay, object.react_time),
                    base: Quat::IDENTITY,
                    written: None,
                });
            }
        }
    }

    fn kick(&mut self, hand: Hand, sniper: bool) {
        for bone in &mut self.recoil_bones {
            if bone.hand == hand && bone.sniper == sniper {
                bone.kick.trigger();
            }
        }
    }

    fn apply_recoil(
        &mut self,
        frames: f32,
        posed: &mut Query<(&mut Transform, Option<&Driven>), With<ClipJoint>>,
    ) {
        for bone in &mut self.recoil_bones {
            let angle = bone.kick.step(frames);
            let Ok((mut transform, _)) = posed.get_mut(bone.entity) else {
                continue;
            };
            bone.rebase(&transform);
            let aim = &self.aim[bone.hand as usize];
            let turn = if aim.shoulder == Some(bone.entity) && aim.sniper == bone.sniper {
                aim.turn(bone.base)
            } else {
                Quat::IDENTITY
            };
            bone.write(&mut transform, angle, turn);
        }
    }

    pub fn joints(&self) -> impl Iterator<Item = Entity> + '_ {
        self.rigs
            .iter()
            .chain(&self.arms)
            .flat_map(|r| r.joints.iter().map(|(entity, _)| *entity))
            .chain(
                self.weapon_arm
                    .iter()
                    .flat_map(|r| r.joints.iter().map(|(entity, _)| *entity)),
            )
            .chain(self.recoil_bones.iter().map(|b| b.entity))
    }

    /// Stowed pose, applied once at spawn so the first frame is not the bind pose.
    pub fn rest_pose(&self) -> Vec<(Entity, Transform)> {
        self.rigs
            .iter()
            .flat_map(WeaponRig::sample)
            .chain(self.weapon_arm.iter().flat_map(WeaponArmRig::sample))
            .collect()
    }
}

/// True when any key's rotation or translation differs from the first key.
fn track_moves(track: &ani::Track) -> bool {
    let Some(first) = track.keys.first() else {
        return false;
    };
    track.keys.iter().skip(1).any(|key| {
        key.rotation
            .iter()
            .zip(first.rotation)
            .any(|(a, b)| (a - b).abs() > 1.0e-4)
            || match (key.translation, first.translation) {
                (Some(a), Some(b)) => a[0].iter().zip(b[0]).any(|(x, y)| (x - y).abs() > 1.0e-4),
                _ => false,
            }
    })
}

impl RecoilBone {
    /// Writes the pose's own rotation turned by `angle` degrees about local X.
    #[cfg(test)]
    fn rest_on(&mut self, transform: &mut Transform, angle: f32) {
        self.rebase(transform);
        self.write(transform, angle, Quat::IDENTITY);
    }

    /// Takes the clip's rotation as the base unless the joint still holds our last write.
    fn rebase(&mut self, transform: &Transform) {
        if self.written != Some(transform.rotation) {
            self.base = transform.rotation;
        }
    }

    /// `turn` is a parent-space rotation applied before the base (the arm aim).
    fn write(&mut self, transform: &mut Transform, angle: f32, turn: Quat) {
        let idle = angle == 0.0 && turn == Quat::IDENTITY;
        if idle && self.written.is_none() {
            return;
        }
        transform.rotation = turn * self.base * Quat::from_rotation_x(angle.to_radians());
        self.written = (!idle).then_some(transform.rotation);
    }
}

impl ArmAim {
    /// Advances `frames` 60 Hz frames; `want` is fire held on this side this frame.
    fn step(&mut self, want: bool, frames: f32) {
        self.hold = if want {
            AIM_HOLD
        } else {
            (self.hold - frames).max(0.0)
        };
        self.weight = if self.hold > 0.0 {
            (self.weight + frames / AIM_RAISE).min(1.0)
        } else {
            (self.weight - frames / AIM_LOWER).max(0.0)
        };
    }

    /// Parent-space turn of the shoulder whose unaimed rotation is `base`, bringing the barrel
    /// toward `aim`, scaled by the weight and clamped to the joint's LockMin/MaxX.
    fn turn(&self, base: Quat) -> Quat {
        let Some(frame) = self.frame.filter(|_| self.weight > 0.0) else {
            return Quat::IDENTITY;
        };
        let barrel = (frame.parent * base * frame.barrel).normalize_or_zero();
        if barrel == Vec3::ZERO {
            return Quat::IDENTITY;
        }
        let full = Quat::from_rotation_arc(barrel, frame.aim);
        let limit = self.limit.to_radians();
        let full = if full.angle_between(Quat::IDENTITY) > limit {
            Quat::IDENTITY.slerp(full, limit / full.angle_between(Quat::IDENTITY))
        } else {
            full
        };
        let world = Quat::IDENTITY.slerp(full, self.weight);
        frame.parent.inverse() * world * frame.parent
    }
}

impl Kick {
    fn new(angle: f32, delay: f32, time: f32) -> Self {
        Self {
            angle,
            delay,
            time,
            clock: None,
            ramping: false,
            shown: 0.0,
            from: 0.0,
            blend: KICK_RETURN,
            progress: KICK_RETURN,
        }
    }

    fn trigger(&mut self) {
        self.clock = Some(0.0);
        self.ramping = false;
    }

    fn ease(&mut self, frames: f32) {
        (self.from, self.blend, self.progress) = (self.shown, frames, 0.0);
    }

    /// Advances `frames` 60 Hz frames; returns the degrees the joint is turned now.
    fn step(&mut self, frames: f32) -> f32 {
        let mut target = 0.0;
        if let Some(clock) = self.clock {
            if clock >= self.delay {
                if !self.ramping {
                    self.ramping = true;
                    self.ease(self.time);
                }
                target = self.angle * ((clock - self.delay) / self.time).min(1.0);
            }
        }
        let t = if self.blend > 0.0 {
            (self.progress / self.blend).min(1.0)
        } else {
            1.0
        };
        self.shown = self.from + (target - self.from) * t;
        self.progress += frames;
        if let Some(clock) = self.clock.map(|c| c + frames) {
            self.clock = Some(clock);
            if clock > self.delay + self.time {
                self.clock = None;
                self.ramping = false;
                self.ease(KICK_RETURN);
            }
        }
        self.shown
    }
}

impl WeaponRig {
    fn apply(&self, posed: &mut Query<(&mut Transform, Option<&Driven>), With<ClipJoint>>) {
        if self.body && self.playback.phase == Phase::Stowed {
            return;
        }
        let Some(clip) = self.clips[self.playback.role].as_ref() else {
            return;
        };
        let frame = self.playback.frame;
        for &(entity, bone) in &self.joints {
            let Ok((mut current, driven)) = posed.get_mut(entity) else {
                continue;
            };
            if driven.is_some() && !self.body {
                continue;
            }
            let rest = self.skeleton.bones.get(bone).and_then(|b| b.rest.as_ref());
            let track = clip.bones.get(bone).map(|b| &b.track);
            // Arm binders key every bone, but `r_arm*` / `l_arm*` are two identical identity
            // keys. Writing those plants the limb on the bind and erases the body stance
            // (acmotion 78 moves `r_arm01`). Only a track that changes, the shoulder pads, applies.
            if self.body && track.is_none_or(|t| !track_moves(t)) {
                continue;
            }
            let rotation = track
                .and_then(|t| t.rotation(frame))
                .or_else(|| rest.map(|r| ani::euler_quat(r.euler)))
                .unwrap_or([0.0, 0.0, 0.0, 1.0]);
            let raw = track
                .and_then(|t| t.translation(frame))
                .unwrap_or(rest.map(|r| r.translation).unwrap_or([0.0; 3]));
            let translation = match driven.filter(|_| self.body) {
                Some(d) => {
                    let rest_t = rest.map(|r| Vec3::from(r.translation)).unwrap_or(d.bind);
                    d.bind + (Vec3::from(raw) - rest_t)
                }
                None => Vec3::from(raw),
            };
            let scale = track
                .and_then(|t| t.scale(frame))
                .unwrap_or(rest.map(|r| r.scale).unwrap_or([1.0; 3]));
            *current =
                pose::to_transform(translation, Quat::from_array(rotation), Vec3::from(scale));
        }
    }

    fn sample(&self) -> Vec<(Entity, Transform)> {
        let Some(clip) = self.clips[self.playback.role].as_ref() else {
            return Vec::new();
        };
        let frame = self.playback.frame;
        self.joints
            .iter()
            .map(|&(entity, bone)| {
                let rest = self.skeleton.bones.get(bone).and_then(|b| b.rest.as_ref());
                let track = clip.bones.get(bone).map(|b| &b.track);
                let rotation = track
                    .and_then(|t| t.rotation(frame))
                    .or_else(|| rest.map(|r| ani::euler_quat(r.euler)))
                    .unwrap_or([0.0, 0.0, 0.0, 1.0]);
                let translation = track
                    .and_then(|t| t.translation(frame))
                    .unwrap_or(rest.map(|r| r.translation).unwrap_or([0.0; 3]));
                let scale = track
                    .and_then(|t| t.scale(frame))
                    .unwrap_or(rest.map(|r| r.scale).unwrap_or([1.0; 3]));
                (
                    entity,
                    pose::to_transform(
                        Vec3::from(translation),
                        Quat::from_array(rotation),
                        Vec3::from(scale),
                    ),
                )
            })
            .collect()
    }
}

/// Which hands a placement carries. The two arm slots share the `arms` column.
/// `(hand, on the bay rack)`.
fn placement_place(column: &str) -> Option<(Hand, bool)> {
    match column {
        "armwep_r" => Some((Hand::Right, false)),
        "armwep_l" => Some((Hand::Left, false)),
        "hanger_r" => Some((Hand::Right, true)),
        "hanger_l" => Some((Hand::Left, true)),
        _ => None,
    }
}

/// `hr2230_m.bnd.dcx` -> `hr2230_a.bnd.dcx`.
fn anim_binder(model: &str) -> Option<String> {
    let binder = model.split('|').next()?;
    let (stem, rest) = binder.rsplit_once("_m.bnd")?;
    Some(format!("{stem}_a.bnd{rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_rifle_is_rigid() {
        let g = gun(1720);
        assert_eq!(
            g.kind,
            Kind::Rigid,
            "part 1720 bullet_id should be rigid 10002"
        );
        assert_eq!(g.magazine, 350);
        assert_eq!(g.remaining, 350);
        assert_eq!(g.init_speed, 391.0);
        assert_eq!(g.reload_time, 30.0);
        assert_eq!(g.damage_power, 1158.0);
        assert_eq!(g.damage_recoil, 628.0);
        assert_eq!(
            g.decay,
            RangeDecay {
                no_decay_range: 153.0,
                damp_range: 525.0,
                guarantee_rate: 50.0,
                min_power_rate: 50.0,
            }
        );
        assert!(
            !g.sniper && !g.stance,
            "the starter rifle uses the gun kick, not the ready stance"
        );
    }

    #[test]
    fn range_decay_matches_two_segment_curve() {
        // Starter rifle 1720: 100% up to 153 m, linear to 50% at 525 m, 50% to 2000 m.
        let rifle = gun(1720);
        assert_eq!(rifle.decay.multiplier(0.0), 1.0);
        assert_eq!(rifle.decay.multiplier(153.0), 1.0);
        assert!((rifle.decay.multiplier(339.0) - 0.75).abs() < 1e-6);
        assert!((rifle.decay.multiplier(525.0) - 0.50).abs() < 1e-6);
        assert!((rifle.decay.multiplier(2000.0) - 0.50).abs() < 1e-6);
        assert_eq!(rifle.damage_at(153.0), (1158.0, 628.0));
        assert_eq!(rifle.damage_at(525.0), (579.0, 314.0));

        // Pulse machine gun 920: 100% up to 81 m, 70% at 225 m, linear to 10% at 2000 m.
        let pmg = gun(920);
        assert_eq!(pmg.decay.multiplier(81.0), 1.0);
        assert!((pmg.decay.multiplier(153.0) - 0.85).abs() < 1e-6);
        assert!((pmg.decay.multiplier(225.0) - 0.70).abs() < 1e-6);
        assert!((pmg.decay.multiplier(1112.5) - 0.40).abs() < 1e-6);
        assert!((pmg.decay.multiplier(2000.0) - 0.10).abs() < 1e-6);
        assert!((pmg.decay.multiplier(2500.0) - 0.10).abs() < 1e-6);
    }

    #[test]
    fn projectile_units_and_stall() {
        let dt = 1.0 / TICK_RATE;
        // Rigid bullet (1720 -> row 10002): init_speed = 391 m/s, gravity = 1.0 m/s per tick.
        let rifle = gun(1720);
        let mut shot = Projectile::from_gun(&rifle, Vec3::NEG_Z);
        assert_eq!(shot.velocity, Vec3::new(0.0, 0.0, -391.0));
        assert_eq!(shot.effective_damage(), (1158.0, 628.0));
        let first = shot.step(dt).expect("in flight");
        // After 1 tick: v_y = -1.0 m/s, v_z = -391.0 m/s (air_cp = 0 before damp_range).
        assert!((shot.velocity.y - (-1.0)).abs() < 1e-5);
        assert!((shot.velocity.z - (-391.0)).abs() < 1e-5);
        assert!((first.z - (-391.0 / 60.0)).abs() < 1e-5);

        // Energy bullet (920 -> row 10006): init_speed = 422 m/s, damp_range = 225 m,
        // stall_to_vanish_frame = 5.
        let pmg = gun(920);
        let mut en = Projectile::from_gun(&pmg, Vec3::NEG_Z);
        assert_eq!(en.velocity, Vec3::new(0.0, 0.0, -422.0));
        let mut ticks = 0usize;
        while en.step(dt).is_some() {
            ticks += 1;
        }
        // 225 m at 422 m/s (7.033 m/tick) crosses damp_range on tick 32 (225.07 m), then
        // ticks 33..=37 accumulate stall_frames 1..=5 and tick 38 (stall_frames = 6 > 5) vanishes.
        assert_eq!(ticks, 37, "energy round vanishes 5 ticks after damp_range");
    }

    #[test]
    fn lightweight_sniper_rifle_does_not_take_a_stance() {
        let g = gun(2410);
        assert!(
            !g.stance && g.sniper,
            "a sniper rifle fires without the ready stance and kicks with sniper_$(LR)"
        );
        assert_eq!(part_field(2410, 10, "weapon_kind"), 5.0);
        assert_eq!(part_field(2410, 10, "ready_position"), 0.0);
    }

    #[test]
    fn ready_weapons_use_the_body_stance() {
        for id in [2010, 2230, 2610] {
            assert!(gun(id).stance, "part {id} is a ready weapon");
            assert_eq!(part_field(id, 10, "ready_position"), 1.0);
        }
        assert!(!gun(2410).stance, "a sniper rifle is not a ready weapon");
        assert_eq!(ac_state("sniper_ready_r", None).map(|s| s.row), Some(93));
        assert_eq!(ac_state("sniper_stow_l", None).map(|s| s.row), Some(96));
    }

    #[test]
    fn gun_kick_waits_ramps_and_settles() {
        // gun_R r_arm01: ReactAng -10, ReactDelay 5, ReactTime 6.
        let mut kick = Kick::new(-10.0, 5.0, 6.0);
        assert_eq!(kick.step(1.0), 0.0, "idle joint is untouched");
        kick.trigger();
        let frames: Vec<f32> = (0..60).map(|_| kick.step(1.0)).collect();
        assert!(
            frames[..5].iter().all(|&a| a == 0.0),
            "nothing before ReactDelay"
        );
        let (peak_at, peak) = frames
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.total_cmp(b.1))
            .unwrap();
        assert!(
            (peak + 10.0).abs() < 1e-4 && (10..=12).contains(&peak_at),
            "{frames:?}"
        );
        assert!(
            frames[peak_at + 15] > -6.0 && frames[peak_at + 15] < -4.0,
            "halfway back"
        );
        assert_eq!(
            frames[59], 0.0,
            "settled {KICK_RETURN} frames after the kick"
        );
    }

    #[test]
    fn spammed_kick_on_an_unkeyed_joint_does_not_stack() {
        let mut bone = RecoilBone {
            entity: Entity::PLACEHOLDER,
            hand: Hand::Right,
            sniper: false,
            kick: Kick::new(-10.0, 5.0, 6.0),
            base: Quat::IDENTITY,
            written: None,
        };
        let pose = Quat::from_rotation_y(0.3);
        let mut transform = Transform::from_rotation(pose);
        for frame in 0..300 {
            if frame % 3 == 0 {
                bone.kick.trigger();
            }
            let angle = bone.kick.step(1.0);
            bone.rest_on(&mut transform, angle);
            let off = pose.inverse() * transform.rotation;
            assert!(
                off.angle_between(Quat::IDENTITY).to_degrees() <= 10.01,
                "frame {frame}"
            );
        }
        for _ in 0..60 {
            let angle = bone.kick.step(1.0);
            bone.rest_on(&mut transform, angle);
        }
        assert!(
            transform.rotation.angle_between(pose) < 1e-4,
            "returns to the pose"
        );
    }

    #[test]
    fn arm_raises_on_fire_holds_then_lowers() {
        let mut arm = ArmAim::default();
        arm.step(true, 5.0);
        assert!(
            (arm.weight - 0.5).abs() < 1e-4,
            "half raised after 5 of {AIM_RAISE} frames"
        );
        arm.step(true, 5.0);
        assert_eq!(arm.weight, 1.0);
        for _ in 1..AIM_HOLD as usize {
            arm.step(false, 1.0);
        }
        assert_eq!(
            arm.weight, 1.0,
            "held {AIM_HOLD} frames after fire is let go"
        );
        arm.step(false, 1.0);
        arm.step(false, 29.0);
        assert!(
            (arm.weight - 0.5).abs() < 1e-4,
            "half lowered after 30 of {AIM_LOWER}"
        );
        arm.step(false, 30.0);
        assert_eq!(arm.weight, 0.0);
    }

    #[test]
    fn arm_turn_points_the_barrel_within_the_limit() {
        let mut arm = ArmAim {
            limit: 85.0,
            weight: 1.0,
            frame: Some(AimFrame {
                parent: Quat::from_rotation_y(0.4),
                barrel: Vec3::new(0.0, -1.0, -1.0).normalize(),
                aim: Vec3::NEG_Z,
            }),
            ..default()
        };
        let base = Quat::from_rotation_z(0.2);
        let frame = arm.frame.unwrap();
        let barrel = |turn: Quat| frame.parent * turn * base * frame.barrel;
        let full = arm.turn(base);
        let off = barrel(Quat::IDENTITY).angle_between(frame.aim).to_degrees();
        assert!(off < 85.0, "test setup: start {off} deg off");
        assert!(
            barrel(full).angle_between(frame.aim) < 1e-3,
            "points along the aim"
        );
        arm.weight = 0.5;
        let half = barrel(arm.turn(base)).angle_between(frame.aim).to_degrees();
        assert!((half - off / 2.0).abs() < 0.1, "{half} vs {off}");
        arm.frame = Some(AimFrame {
            aim: Vec3::Y,
            ..frame
        });
        arm.weight = 1.0;
        let turned = barrel(arm.turn(base)).angle_between(barrel(Quat::IDENTITY));
        assert!(
            (turned.to_degrees() - 85.0).abs() < 0.1,
            "clamped to LockMaxX"
        );
    }

    #[test]
    fn a_new_shot_restarts_the_kick_from_the_current_angle() {
        let mut kick = Kick::new(-5.0, 0.0, 6.0);
        kick.trigger();
        for _ in 0..10 {
            kick.step(1.0);
        }
        let mid = kick.shown;
        kick.trigger();
        let first = kick.step(1.0);
        assert!((first - mid).abs() < 1e-4, "eases from {mid}, got {first}");
    }

    #[test]
    fn arms_slot_covers_both_hands() {
        assert_eq!(placement_place("armwep_r"), Some((Hand::Right, false)));
        assert_eq!(placement_place("hanger_l"), Some((Hand::Left, true)));
        assert_eq!(placement_place("core"), None);
    }

    #[test]
    fn arm_deploy_holds_without_firing() {
        let mut play = Playback::new(true, [1, 20, 0, 20]);
        assert!(!play.tick(true, false, 19.0));
        assert_eq!(play.phase, Phase::Deploy);
        assert!(!play.tick(true, false, 19.0));
        assert_eq!(play.phase, Phase::Out);
    }

    #[test]
    fn cannon_engages_ready_position() {
        assert_eq!(part_field(2010, 10, "weapon_kind"), 16.0);
        assert_eq!(part_field(2010, 10, "ready_position"), 1.0);
        assert_eq!(part_field(1210, 10, "ready_position"), 1.0);
        assert_eq!(part_field(2230, 10, "ready_position"), 1.0);
        assert_eq!(
            part_field(1720, 10, "ready_position"),
            0.0,
            "rifles have no ready position"
        );
        assert_eq!(part_field(410, 10, "ready_position"), 0.0);
    }

    #[test]
    fn ready_weapon_deploys_before_it_fires() {
        let mut play = Playback::new(true, [1, 30, 18, 30]);
        assert!(!play.tick(true, true, 10.0), "the press starts the deploy");
        assert_eq!(play.phase, Phase::Deploy);
        assert!(!play.tick(true, true, 10.0));
        assert!(
            !play.tick(true, true, 9.0),
            "frame 19 of 30 is still deploying"
        );
        assert!(
            play.tick(true, true, 10.0),
            "reaching the last frame lets the shot out"
        );
        assert_eq!(play.phase, Phase::Fire);
    }

    #[test]
    fn releasing_fire_plays_the_stow() {
        let mut play = Playback::new(true, [1, 30, 0, 30]);
        assert!(!play.tick(true, true, 29.0));
        assert!(
            play.tick(true, true, 29.0),
            "deployed, no fire clip, shot still leaves"
        );
        assert_eq!(play.phase, Phase::Out);
        assert!(!play.tick(false, true, 1.0));
        assert_eq!(play.phase, Phase::Stow);
        assert_eq!(play.role, 3);
    }

    #[test]
    fn rifle_and_howitzer_without_a_deploy_clip_fire_immediately() {
        let mut rifle = Playback::new(false, [1, 0, 10, 0]);
        assert!(rifle.tick(true, true, 1.0));
        assert_eq!(rifle.role, 2);
        let mut howitzer = Playback::new(true, [1, 0, 60, 0]);
        assert!(
            howitzer.tick(true, true, 1.0),
            "ready_position with no a00_001 does not block the shot"
        );
        assert_eq!(howitzer.role, 2);
    }

    #[test]
    fn starter_rifle_uses_its_sound_row() {
        let g = gun(1720);
        let hit = part_field(1720, 10, "hit_id") as u32;
        let row = find(PARAM_ACWEAPONSOUNDPARAM_BIN, hit)
            .expect("part 1720 hit_id has an acweaponsoundparam row");
        assert_eq!(g.shoot, row.data.shoot);
        assert!(g.shoot > 0, "rifle shoot cue");
        assert_eq!(g.en_drain, 0.0, "kinetic rifle has zero EN drain");
    }

    #[test]
    fn energy_weapon_reads_en_drain() {
        let g = gun(810);
        let expected = part_field(810, 10, "en_drain");
        assert!(expected > 0.0, "laser weapon 810 has positive EN drain");
        assert_eq!(g.en_drain, expected);
        assert_eq!(g.damage_type, DamageType::Te);
        assert_eq!(g.hit_se.low, 9028);
        assert_eq!(g.hit_se.hi, 9225);
        assert_eq!(gun(1720).damage_type, DamageType::Ke);
        assert_eq!(gun(1720).hit_se.low, 9027);
        assert_eq!(gun(1720).hit_se.hi, 9224);
        assert_eq!(gun(1720).fx.hit_low, 199);
        assert_eq!(gun(1720).fx.hit_hi, 411);
        assert_eq!(gun(2230).damage_type, DamageType::Ce);
    }

    #[test]
    fn shoulder_missile_520_reads_cat12_fields_and_homing_flight() {
        // Design 5001 shoulder unit 520 (sh0520, Small missile).
        let sh = shoulder_gun(520, 1);
        assert_eq!(sh.kind, Kind::Missile);
        assert!(sh.is_missile);
        assert_eq!(sh.magazine, 72);
        assert_eq!(sh.reload_time, 112.0);
        assert_eq!(sh.lock_time_ticks, 18.0);
        assert_eq!(sh.weapon_lock_max, 6);
        assert_eq!(sh.burst_count, 1);
        assert_eq!(sh.burst_interval, 10.0);
        assert_eq!(sh.init_speed, 60.0);
        assert_eq!(sh.damage_type, DamageType::Ce);
        assert_eq!(sh.damage_power, 1292.0);
        assert_eq!(sh.damage_recoil, 884.0);
        assert_eq!(sh.shoot, 70);
        assert_eq!(sh.fx.bullet, 4800);
        assert_eq!(sh.fx.after_accel, 4801);

        // Twin-shoulder arms (am0120, float 4 = 2.0) double burst_count (`0x82858824..0x82858838`).
        let twin = shoulder_gun(520, 2);
        assert_eq!(twin.burst_count, 2);

        // Homing missile launched upward & right ignites at frame 30 and curves to hit a target ahead.
        let mut m = Missile::from_gun(&sh, Vec3::new(0.4, 0.5, -0.7).normalize(), None);
        let mut pos = Vec3::ZERO;
        let target = Vec3::new(0.0, 2.0, -120.0);
        let dt = 1.0 / TICK_RATE;
        let mut ignited_at = None;
        let mut min_dist = f32::MAX;
        for tick in 1..=240 {
            let (delta, just_ignited) = m.step(pos, Some(target), dt).expect("missile alive");
            if just_ignited {
                ignited_at = Some(tick);
            }
            pos += delta;
            let d = (pos - target).length();
            min_dist = min_dist.min(d);
            if d < 5.0 {
                break;
            }
        }
        assert_eq!(ignited_at, Some(30), "booster ignites at accel_start_frame = 30");
        assert!(
            min_dist < 5.0,
            "homing missile should intercept target (min_dist = {min_dist})"
        );
    }

    #[test]
    fn melee_blades_and_heat_piles_load_from_bulletblade_bin() {
        // Laser blade 350 (design 5011 hanger_l): TE, infinite ammo, EN drain, FFX 4104, hit_radius 25.
        let lb350 = gun(350);
        assert_eq!(lb350.kind, Kind::Blade);
        assert!(!lb350.is_missile);
        assert_eq!(lb350.damage_type, DamageType::Te);
        assert_eq!(lb350.magazine, 0);
        assert!(lb350.en_drain > 2000.0);
        assert!(lb350.damage_power > 8000.0);
        assert_eq!(lb350.fx.bullet, 4104);
        assert_eq!(lb350.shoot, 6);
        let spec350 = lb350.blade.expect("laser blade spec");
        assert!(!spec350.physical, "TE laser blade does not use speedxweight scaling (0x8247f170)");
        assert!(spec350.infinite_ammo);
        assert_eq!(spec350.hit_radius, 25.0);
        assert_eq!(spec350.blade_radius, 2.0);
        assert_eq!(spec350.max_hits, 3);

        // Laser blade 360 (starter design 5001 hanger_l): TE, infinite ammo, bullet 10063.
        let lb360 = gun(360);
        assert_eq!(lb360.kind, Kind::Blade);
        assert_eq!(lb360.blade.unwrap().hit_radius, 22.0);

        // Heat Pile 150 (design 6006 armwep_r): CE, finite ammo (12), 0 EN drain, physical scaling.
        let pile150 = gun(150);
        assert_eq!(pile150.kind, Kind::Blade);
        assert_eq!(pile150.damage_type, DamageType::Ce);
        assert_eq!(pile150.magazine, 12);
        assert_eq!(pile150.en_drain, 0.0);
        assert!(pile150.damage_power > 18000.0);
        assert!(pile150.damage_recoil > 2000.0);
        assert_eq!(pile150.shoot, 3);
        let spec150 = pile150.blade.expect("heat pile spec");
        assert!(spec150.physical, "CE heat pile uses physical speedxweight scaling (0x8247f170)");
        assert!(!spec150.infinite_ammo);
        assert_eq!(spec150.hit_radius, 10.0);
        assert_eq!(spec150.blade_radius, 3.0);

        // Physical Blade 2810 (design 6304 hanger): KE, infinite ammo, 0 EN drain, physical scaling.
        let phys2810 = gun(2810);
        assert_eq!(phys2810.kind, Kind::Blade);
        assert_eq!(phys2810.damage_type, DamageType::Ke);
        assert_eq!(phys2810.magazine, 0);
        assert_eq!(phys2810.en_drain, 0.0);
        assert_eq!(phys2810.shoot, 80);
        let spec2810 = phys2810.blade.expect("physical blade spec");
        assert!(spec2810.physical);
        assert!(spec2810.infinite_ammo);
        assert_eq!(spec2810.hit_radius, 10.0);
        assert_eq!(spec2810.blade_radius, 4.0);

        // Kick hit VFX/SE row 99000 and hit sphere row 4100 resolve cleanly from disc tables.
        let (kfx, kse) = fx_and_hit_se_for(99000);
        assert_eq!(kfx.hit_hi, 135);
        assert_eq!(kse.hi, 9202);
        assert_eq!(kse.low, 9027);
        assert_eq!(find(BULLET_BULLETEXPLOSION_BIN, 4100).unwrap().data.init_radius, 7.0);
    }

    #[test]
    fn kick_and_blade_hits_stagger_and_destroy_targets() {
        // Test Boost Charge Kick against tank e1020 (AP 1200) and DEF-MT e0210 (AP 3600).
        let design5001 = acvd_data::find(
            acvd_data::generated::ac_unit::PARAM_ACASSEMBLYDRAWING_BIN,
            5001,
        )
        .unwrap()
        .data;
        let ctrl = acvd_data::generated::ctrl::AcCtrlParam::calculate(&design5001);
        let (k_pwr, k_rec) =
            SpeedWeightScale::kick().scale(ctrl.kick_power, ctrl.kick_recoil, 120.0, ctrl.total_weight);

        let mut tank = TargetHealth::from_enemy("e1020");
        let (res, lethal) = tank.apply_hit(DamageType::Ke, k_pwr, k_rec);
        assert!(res.effective && lethal && tank.destroyed, "120 km/h kick must one-shot tank");

        let mut mt = TargetHealth::from_enemy("e0210");
        let (res_mt, _) = mt.apply_hit(DamageType::Ke, k_pwr, k_rec);
        assert!(res_mt.effective && mt.is_staggered(), "kick recoil must stagger DEF-MT");

        // Test Heat Pile 150 at 130 km/h against DEF-MT e0210 -> one-shot destruction.
        let pile = gun(150);
        let (p_pwr, p_rec) = SpeedWeightScale::physical_blade().scale(
            pile.damage_power,
            pile.damage_recoil,
            130.0,
            ctrl.total_weight,
        );
        let mut mt2 = TargetHealth::from_enemy("e0210");
        let (res_p, lethal_p) = mt2.apply_hit(pile.damage_type, p_pwr, p_rec);
        assert!(res_p.effective && lethal_p && mt2.destroyed, "Heat Pile must destroy DEF-MT");

        // Test Laser Blade 350 fixed TE damage against tank e1020 -> one-shot destruction.
        let lb = gun(350);
        let mut tank2 = TargetHealth::from_enemy("e1020");
        let (res_l, lethal_l) = tank2.apply_hit(lb.damage_type, lb.damage_power, lb.damage_recoil);
        assert!(res_l.effective && lethal_l && tank2.destroyed, "Laser Blade must destroy tank");
    }

    #[test]
    fn projectile_raycast_impacts_vertical_wall() {
        use crate::collision::Hit;
        // Vertical concrete wall at z = -30 m, 20 m tall.
        let wall = Collision::with_hits(
            Vec::new(),
            vec![
                Hit::solid(
                    Vec3::new(-20.0, 0.0, -30.0),
                    Vec3::new(20.0, 0.0, -30.0),
                    Vec3::new(-20.0, 20.0, -30.0),
                ),
                Hit::solid(
                    Vec3::new(20.0, 0.0, -30.0),
                    Vec3::new(20.0, 20.0, -30.0),
                    Vec3::new(-20.0, 20.0, -30.0),
                ),
            ],
        );
        let rifle = gun(1720);
        let mut shot = Projectile::from_gun(&rifle, Vec3::NEG_Z);
        let mut pos = Vec3::new(0.0, 5.0, 0.0);
        let mut hit = None;
        for _ in 0..120 {
            let Some(delta) = shot.step(1.0 / 60.0) else {
                break;
            };
            let next = pos + delta;
            if let Some((_, at, normal)) = wall.raycast(pos, next, RayFilter::Bullet) {
                hit = Some((at, normal));
                break;
            }
            pos = next;
        }
        let (at, normal) = hit.expect("projectile must impact vertical wall instead of clipping");
        assert!((at.z - (-30.0)).abs() < 1e-3, "impact at {at}");
        assert!((normal - Vec3::Z).length() < 1e-3, "wall normal {normal}");
    }

    #[test]
    fn arm_firing_stability_and_attack_balance_scale_reload() {
        let base = acvd_data::find(
            acvd_data::generated::ac_unit::PARAM_ACASSEMBLYDRAWING_BIN,
            5001,
        )
        .unwrap()
        .data;
        let ctrl_base = AcCtrlParam::calculate(&base);
        let arms_base = Armament::from_design(&base);

        // High firing-stability arms (5520: shoot_stability = 199 -> arm_shoot_stability = 0.67)
        // vs low firing-stability arms (120: shoot_stability = 122 -> arm_shoot_stability = 0.927).
        let mut high_stab = base;
        high_stab.arms = 5520;
        let mut low_stab = base;
        low_stab.arms = 120;
        let ctrl_hi = AcCtrlParam::calculate(&high_stab);
        let ctrl_lo = AcCtrlParam::calculate(&low_stab);
        assert!(
            (ctrl_hi.arm_shoot_stability - (1.0 - (199.0 - 100.0) / 300.0)).abs() < 1e-4,
            "arm_shoot_stability = {}",
            ctrl_hi.arm_shoot_stability
        );
        assert!(ctrl_hi.arm_shoot_stability < ctrl_lo.arm_shoot_stability);

        let arm_hi = Armament::from_design(&high_stab);
        let arm_lo = Armament::from_design(&low_stab);
        let raw_r = gun(base.armwep_r as i64);
        let expected_base =
            raw_r.reload_time * reload_multiplier(&ctrl_base, raw_r.reload_stab_factor, raw_r.stance);
        assert!(
            (arms_base.hands[0].reload_time - expected_base).abs() < 1e-4,
            "expected {expected_base}, got {}",
            arms_base.hands[0].reload_time
        );
        assert!(
            arm_hi.hands[0].reload_time < arm_lo.hands[0].reload_time,
            "higher firing stability must reload faster: hi={} lo={}",
            arm_hi.hands[0].reload_time,
            arm_lo.hands[0].reload_time
        );

        // Ready-position cannon (2010: ready_position = 1) uses snipe_attack_balance without
        // arm_shoot_stability (`0x8286adf8`), so quad legs (1110) reload much faster than biped.
        let mut quad = base;
        quad.legs = 1110;
        quad.armwep_r = 2010;
        let mut biped = base;
        biped.armwep_r = 2010;
        let ctrl_quad = AcCtrlParam::calculate(&quad);
        let ctrl_biped = AcCtrlParam::calculate(&biped);
        assert!(ctrl_quad.snipe_attack_balance < ctrl_biped.snipe_attack_balance * 0.5);
        let arm_quad = Armament::from_design(&quad);
        let arm_biped = Armament::from_design(&biped);
        assert!(
            arm_quad.hands[0].reload_time < arm_biped.hands[0].reload_time,
            "quad ready-position cannon reload {} must be faster than biped {}",
            arm_quad.hands[0].reload_time,
            arm_biped.hands[0].reload_time
        );
    }

    #[test]
    fn split_missiles_trigger_and_spawn_sub_munitions() {
        // Player Spread Missile sh1010 (bullet 51010 -> BulletAriseSt 51010 -> 3x child 61010).
        let sh1010 = shoulder_gun(1010, 1);
        assert_eq!(sh1010.kind, Kind::Missile);
        let mf = sh1010.missile_flight.expect("missile flight");
        let split = mf.split.expect("51010 must have MissileSplit (close_act_id == 8)");
        assert_eq!(split.arise_id, 51010);
        assert_eq!(split.count, 3);
        assert_eq!(split.start_frame, 45.0);
        assert_eq!(split.force_trace_frame, 25.0);
        assert_eq!(split.shoot_speed, 100.0);

        let mut parent = Missile::from_gun(&sh1010, Vec3::NEG_Z, None);
        let mut pos = Vec3::new(0.0, 10.0, 0.0);
        let target = Vec3::new(0.0, 10.0, -350.0);
        let dt = 1.0 / TICK_RATE;
        let mut split_tick = None;
        for tick in 1..=90 {
            let (delta, _) = parent.step(pos, Some(target), dt).expect("parent alive");
            pos += delta;
            if parent.should_split(pos, Some(target)).is_some() {
                split_tick = Some(tick);
                break;
            }
        }
        assert_eq!(split_tick, Some(45), "Spread missile 51010 splits at frame 45");

        let burst = parent.build_split_burst(pos, Some(target), split);
        assert_eq!(burst.children.len(), 3, "must spawn 3 child missiles");
        let mut dirs = Vec::new();
        for (cpos, child) in burst.children {
            let SplitChild::Missile(mut cm) = child else {
                panic!("expected child Missile for 61010");
            };
            assert_eq!(cm.damage_power, 652.0);
            assert_eq!(cm.damage_recoil, 1523.0);
            assert_eq!(cm.split_depth, 1);
            assert!(cm.flight.split.is_none(), "child 61010 does not split again");
            dirs.push(cm.velocity.normalize());
            // Confirm each child homes toward target.
            let mut p = cpos;
            let mut min_d = f32::MAX;
            for _ in 0..360 {
                let Some((d, _)) = cm.step(p, Some(target), dt) else {
                    break;
                };
                p += d;
                min_d = min_d.min(p.distance(target));
            }
            assert!(min_d < 8.0, "child missile should home to target (min_d = {min_d})");
        }
        // Children fan out in distinct directions.
        assert!(dirs[0].dot(dirs[1]) < 0.99);
        assert!(dirs[1].dot(dirs[2]) < 0.99);

        // Distance-triggered split (enemy cluster missile 3521 -> BulletAriseSt 5122 -> 4x 3522).
        let (ekind, _, emf, _) = enemy_flight(3521, 60.0);
        assert_eq!(ekind, Kind::Missile);
        let esplit = emf.unwrap().split.expect("3521 split spec");
        assert_eq!(esplit.arise_id, 5122);
        assert_eq!(esplit.count, 4);
        assert_eq!(esplit.start_dist, 120.0);
        let em = Missile::from_gun_owned(
            &Gun {
                missile_flight: emf,
                kind: Kind::Missile,
                init_speed: 60.0,
                ..silent()
            },
            Vec3::NEG_Z,
            None,
            Owner::Enemy,
        );
        assert!(em.should_split(Vec3::ZERO, Some(Vec3::new(0.0, 0.0, -200.0))).is_none());
        assert!(em.should_split(Vec3::ZERO, Some(Vec3::new(0.0, 0.0, -100.0))).is_some());
        let eburst = em.build_split_burst(Vec3::ZERO, Some(Vec3::new(0.0, 0.0, -100.0)), esplit);
        assert_eq!(eburst.children.len(), 4);
    }

    #[test]
    fn weapon_arms_resolve_embedded_weapons_and_stats() {
        let get = |id: u32| {
            acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES
                .iter()
                .flat_map(|(_, r)| r.iter())
                .find(|r| r.id == id)
                .unwrap()
                .data
        };

        // 6001: am7020 dual blade weapon arms -> wep10 9020 (KE physical blade, infinite ammo).
        let a6001 = Armament::from_design(&get(6001));
        assert!(a6001.weapon_arm);
        assert_eq!(a6001.hands[0].kind, Kind::Blade);
        assert_eq!(a6001.hands[0].damage_type, DamageType::Ke);
        assert_eq!(a6001.hands[0].damage_power, 7735.0);
        assert!(a6001.hands[0].blade.unwrap().infinite_ammo);
        assert_eq!(a6001.hands[1].kind, Kind::Skip);
        assert_eq!(a6001.shoulder.kind, Kind::Skip);

        // 6009: am7420 scatter cannon arms -> wep10 9420 (CE rigid, burst 8, mag 192; ignores stale hand slots).
        let a6009 = Armament::from_design(&get(6009));
        assert!(a6009.weapon_arm);
        assert_eq!(a6009.hands[0].kind, Kind::Rigid);
        assert_eq!(a6009.hands[0].damage_type, DamageType::Ce);
        assert_eq!(a6009.hands[0].burst_count, 8);
        assert_eq!(a6009.hands[0].magazine, 192);
        assert_eq!(a6009.hands[1].kind, Kind::Skip);

        // 6120: am7230 rocket weapon arms -> wep10 9230 (Missile, mag 480, burst 2).
        let a6120 = Armament::from_design(&get(6120));
        assert!(a6120.weapon_arm);
        assert_eq!(a6120.hands[0].kind, Kind::Missile);
        assert_eq!(a6120.hands[0].magazine, 480);
        assert_eq!(a6120.hands[0].burst_count, 2);

        // 6260: am7110 sniper cannon weapon arms -> wep10 9110 (Rigid, 1036 m/s, 9865 dmg).
        let a6260 = Armament::from_design(&get(6260));
        assert!(a6260.weapon_arm);
        assert_eq!(a6260.hands[0].kind, Kind::Rigid);
        assert_eq!(a6260.hands[0].init_speed, 1036.0);
        assert_eq!(a6260.hands[0].damage_power, 9865.0);

        // 6302: am7510 laser cannon weapon arms -> wep10 9510 (Energy TE, EN drain 25369, mag 40).
        let a6302 = Armament::from_design(&get(6302));
        assert!(a6302.weapon_arm);
        assert_eq!(a6302.hands[0].kind, Kind::Energy);
        assert_eq!(a6302.hands[0].damage_type, DamageType::Te);
        assert_eq!(a6302.hands[0].en_drain, 25369.0);
        assert_eq!(a6302.hands[0].magazine, 40);

        // Homing missile weapon arm am7210 -> wep10 9210 (is_missile = true, multi-lock).
        let mut d7210 = get(6001);
        d7210.arms = 7210;
        let a7210 = Armament::from_design(&d7210);
        assert!(a7210.weapon_arm);
        assert_eq!(a7210.hands[0].kind, Kind::Missile);
        assert!(a7210.hands[0].is_missile);
        assert_eq!(a7210.hands[0].magazine, 160);
    }

    #[test]
    fn ac_ctrl_param_ow_formulas_match_lua() {
        let get = |id: u32| {
            acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES
                .iter()
                .flat_map(|(_, r)| r.iter())
                .find(|r| r.id == id)
                .unwrap()
                .data
        };
        for id in [9002_u32, 9003, 9999] {
            let d = get(id);
            let c = AcCtrlParam::calculate(&d);
            // AcCtrlParamCalc.lua lines 253-283:
            assert!((c.ow_cooltime_lv2 - c.ow_mode_time * 0.5).abs() < 1.0e-4);
            assert!((c.ow_cooltime_lv1 - c.ow_cooltime_lv2 * 0.6).abs() < 1.0e-4);
            assert!(c.ow_overload_dmg > 0.0, "design {id} ow_overload_dmg");
            assert!(c.ow_dmgtime_min > 0.0 && c.ow_dmgtime_max > c.ow_dmgtime_min);
            assert!(c.ow_charge_en > 100_000.0, "design {id} charge_en");
            assert!(c.ow_en_output > 10_000.0, "design {id} ow_en_output");
            assert_eq!(c.ow_reload, 3.0);
            assert_eq!(c.ow_calorific_base, 5.0);
            assert_eq!(c.ow_charge_heat_base, 500.0);
            assert!(c.ow_attack_accel_tick > 0.0 && c.ow_attack_max_tick > 0.0);
        }
        // Grind Blade (9003, back_r=110) has attack_speed_scale = 1.8 vs Huge Cannon (9002, back_r=610) = 1.0.
        let mut grind = get(9002);
        let c_cannon = AcCtrlParam::calculate(&grind);
        grind.back_r = 110;
        let c_grind = AcCtrlParam::calculate(&grind);
        assert!(
            (c_grind.ow_attack_max_tick / c_cannon.ow_attack_max_tick - 1.8).abs() < 0.05,
            "Grind Blade 1.8x attack speed scale (0x82477400)"
        );
    }

    #[test]
    fn all_six_overed_weapons_resolve_specs_and_unmitigated_damage() {
        let mut base = acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, r)| r.iter())
            .find(|r| r.id == 9002)
            .unwrap()
            .data;
        let cases = [
            (110_u16, 1_u32, Kind::Blade, true, false),
            (310, 3, Kind::Blade, true, false),
            (610, 6, Kind::Rigid, false, true),
            (810, 8, Kind::Energy, false, true),
            (1010, 10, Kind::Missile, false, true),
            (6010, 60, Kind::Blade, true, false),
        ];
        for (part_id, ow_type, kind, melee, has_blast) in cases {
            base.back_r = part_id as i16;
            let arm = Armament::from_design(&base);
            let ow = arm.overed.expect("equipped OW");
            assert_eq!(ow.spec.part_id, part_id);
            assert_eq!(ow.spec.ow_type, ow_type);
            assert_eq!(ow.spec.gun.kind, kind);
            assert_eq!(ow.spec.is_melee(), melee);
            assert_eq!(ow.spec.gun.damage_type, DamageType::Unmitigated);
            assert!(ow.spec.gun.damage_power >= 60_000.0);
            assert_eq!(ow.spec.blast.is_some(), has_blast, "OW {part_id} blast");
        }
    }

    #[test]
    fn overed_weapon_state_machine_lifecycle() {
        let d = acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, r)| r.iter())
            .find(|r| r.id == 9002)
            .unwrap()
            .data;
        let mut ow = Armament::from_design(&d).overed.expect("design 9002 OW");
        assert_eq!(ow.phase, OwPhase::Disarmed);

        // 1. Short tap does not arm OW; continuous hold for >= activate_hold_secs arms it.
        ow.step(0.3, true, false);
        assert_eq!(ow.phase, OwPhase::Disarmed);
        ow.step(0.1, false, false);
        assert_eq!(ow.activate_hold, 0.0);
        for _ in 0..40 {
            ow.step(0.05, true, false);
        }
        assert_eq!(ow.phase, OwPhase::Armed);
        assert!(ow.is_active());
        assert!(ow.limit_remaining < ow.spec.limit_capacity);

        // 2. Early trigger release decays charge back to 0 without firing.
        ow.step(0.5, false, true);
        assert_eq!(ow.phase, OwPhase::Charging);
        assert!(ow.charge_en_cur > 0.0);
        ow.step(0.05, false, false);
        assert_eq!(ow.phase, OwPhase::Armed);
        for _ in 0..40 {
            ow.step(0.05, false, false);
        }
        assert_eq!(ow.charge_en_cur, 0.0);

        // 3. Full charge + overload self-damage ticks + release-to-fire -> Attacking -> Purged -> Lockout -> Spent.
        let mut saw_overload = false;
        let mut saw_charged = false;
        for _ in 0..400 {
            let r = ow.step(0.1, false, true);
            if r.overload_damage > 0.0 {
                saw_overload = true;
            }
            if r.event == Some(OwEvent::ChargeComplete) || ow.phase == OwPhase::Charged {
                saw_charged = true;
                break;
            }
        }
        assert!(saw_charged, "OW must reach Charged phase");
        // Release trigger to fire Huge Cannon!
        let fire_res = ow.step(0.05, false, false);
        assert_eq!(fire_res.event, Some(OwEvent::Fired));
        assert_eq!(ow.phase, OwPhase::Attacking);
        assert_eq!(ow.attacks_fired, 1);

        // Let attack finish -> Huge Cannon auto-purges -> enters Lockout -> eventually Spent.
        let mut saw_purge = false;
        for _ in 0..40 {
            let r = ow.step(0.05, false, false);
            if r.overload_damage > 0.0 {
                saw_overload = true;
            }
            if r.event == Some(OwEvent::Purged) {
                saw_purge = true;
                break;
            }
        }
        assert!(saw_purge, "Huge Cannon must auto-purge after firing");
        assert_eq!(ow.phase, OwPhase::Lockout);
        assert!(!ow.is_active());

        // Advance past lockout_secs -> Spent.
        let mut ended = false;
        for _ in 0..100 {
            if ow.step(0.5, false, false).event == Some(OwEvent::LockoutEnded) {
                ended = true;
                break;
            }
        }
        assert!(ended && ow.phase == OwPhase::Spent);
        // Verify overload self-damage fires on a fresh armed OW over 20 s.
        if !saw_overload {
            let mut ow2 = Armament::from_design(&d).overed.unwrap();
            for _ in 0..40 {
                ow2.step(0.05, true, false);
            }
            let mut dmg_total = 0.0;
            for _ in 0..200 {
                dmg_total += ow2.step(0.1, false, false).overload_damage;
            }
            assert!(dmg_total > 0.0, "armed OW must deal periodic overload AP self-damage");
        }
    }

    #[test]
    fn hand_weapon_firing_modes_cover_auto_pellet_burst_and_charge() {
        // Auto: rifle 1611.
        let mut rifle = gun(1611);
        assert_eq!(rifle.fire_mode, FireMode::Auto);
        assert_eq!(
            rifle.step_trigger(true, true, true, 1.0),
            TriggerAction::Fire {
                power_scale: 1.0,
                pellets: 1,
            }
        );

        // PelletSpread: shotgun 1011 (12 simultaneous pellets).
        let mut shotgun = gun(1011);
        assert_eq!(shotgun.fire_mode, FireMode::PelletSpread);
        assert_eq!(shotgun.burst_count, 12);
        assert_eq!(
            shotgun.step_trigger(true, true, true, 1.0),
            TriggerAction::Fire {
                power_scale: 1.0,
                pellets: 12,
            }
        );

        // Burst: burst rifle 710 (5 sequential rounds spaced by burst_wait = 5 ticks).
        let mut burst = gun(710);
        assert_eq!(burst.fire_mode, FireMode::Burst);
        assert_eq!(burst.burst_count, 5);
        assert_eq!(burst.burst_wait, 5.0);
        let mut fired = 0;
        if matches!(
            burst.step_trigger(true, true, true, 1.0),
            TriggerAction::Fire { pellets: 1, .. }
        ) {
            fired += 1;
        }
        // Release trigger immediately; remaining 4 burst rounds still fire on schedule.
        for _ in 0..25 {
            if matches!(
                burst.step_trigger(false, false, true, 1.0),
                TriggerAction::Fire { pellets: 1, .. }
            ) {
                fired += 1;
            }
        }
        assert_eq!(fired, 5, "burst rifle must fire all 5 sequential rounds");

        // Charge: laser rifle 1411 (charge_time = 109 ticks), rail cannon 2510 (63), laser cannon 2110 (246).
        let mut laser = gun(1411);
        assert_eq!(laser.fire_mode, FireMode::Charge);
        assert_eq!(laser.charge_time, 109.0);
        assert_eq!(gun(2510).fire_mode, FireMode::Charge);
        assert_eq!(gun(2110).fire_mode, FireMode::Charge);

        // Hold to start charge, charge halfway, release -> partial power (0.5..1.0).
        assert_eq!(laser.step_trigger(true, true, true, 1.0), TriggerAction::StartCharge);
        assert_eq!(laser.step_trigger(true, true, true, 53.5), TriggerAction::None);
        match laser.step_trigger(false, true, true, 1.0) {
            TriggerAction::Fire { power_scale, pellets } => {
                assert_eq!(pellets, 1);
                assert!(power_scale > 0.7 && power_scale < 0.8, "half charge scale {power_scale}");
            }
            other => panic!("expected partial-charge fire on release, got {other:?}"),
        }

        // Full charge -> release fires at full 1.0 power.
        assert_eq!(laser.step_trigger(true, true, true, 1.0), TriggerAction::StartCharge);
        assert_eq!(laser.step_trigger(true, true, true, 120.0), TriggerAction::None);
        assert_eq!(laser.charge_fraction(), 1.0);
        assert_eq!(
            laser.step_trigger(false, true, true, 1.0),
            TriggerAction::Fire {
                power_scale: 1.0,
                pellets: 1,
            }
        );
    }

    #[test]
    fn muzzle_scale_and_cartridge_sfx_from_weaponsfxparam() {
        // 360 0x82891f08 -> 0x82b88a78 spawns muzzle_sfx_id at SfxRequest.scale = 1.0
        // (private/xenia/fx_polish.jsonl / fx_polish2.jsonl).
        assert_eq!(MUZZLE_SCALE, 1.0);
        assert_eq!(CARTRIDGE_POINT, 111);

        // Rifle hit_id on design 5001 starter AC has muzzle and cartridge SFX.
        let design = &find(
            acvd_data::generated::ac_unit::PARAM_ACASSEMBLYDRAWING_BIN,
            5001,
        )
        .expect("design 5001")
        .data;
        let arms = Armament::from_design(design);
        let rifle = &arms.hands[0];
        assert!(rifle.fx.muzzle > 0, "rifle must have muzzle_sfx_id");
        assert!(
            rifle.fx.cartridge > 0,
            "ballistic rifle must populate cartridge_sfx_id (got {})",
            rifle.fx.cartridge
        );
    }

    #[test]
    fn weapon_arm_rig_loads_all_nonroot_bones_and_transitions_phases() {
        use crate::assemble;
        use acvd_data::generated::ac_unit::PARAM_ACASSEMBLYDRAWING_BIN;
        use acvd_data::generated::assembly::AC_ASSEMBLY_DESIGN_ST_SLOTS;

        assert_eq!(weapon_arm_clip_base(0), 0);
        assert_eq!(weapon_arm_clip_base(2), 0);
        assert_eq!(weapon_arm_clip_base(3), 10);
        assert_eq!(weapon_arm_clip_base(4), 10);
        assert_eq!(weapon_arm_clip_base(5), 20);
        assert_eq!(weapon_arm_clip_base(6), 30);

        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let Ok(disc) = Disc::open(&vfs::default_disc(&root)) else {
            return;
        };

        // Design 6009 uses weapon arms 7420 (`am7420.flv`, 54 bones: 2 roots `l_kata`/`r_kata`
        // + 52 child bones, with full 25-clip `am7420_a.bnd.dcx` including fire clip `a00_002`).
        let design = &find(PARAM_ACASSEMBLYDRAWING_BIN, 6009)
            .expect("design 6009")
            .data;
        let built = assemble::assemble(AC_ASSEMBLY_DESIGN_ST_SLOTS, design);
        let arms_pl = built
            .placements
            .iter()
            .find(|p| p.column == "arms")
            .expect("arms placement");
        let (_, rig) =
            acvd_render::rigged_model(&disc, arms_pl.model.path, &|_| assemble::place_of(&[], None))
                .expect("rigged_model");
        let mut world = World::new();
        let joints: Vec<Entity> = (0..rig.bones.len()).map(|_| world.spawn_empty().id()).collect();
        let leg_type = part_field(design.legs as i64, 3, "leg_type") as u8;

        let mut anims = WeaponAnims::default();
        anims.load(&disc, arms_pl, &rig, &joints, leg_type);
        assert!(anims.arms.is_empty(), "weapon arm must not load normal-arm rigs");
        assert!(anims.recoil_bones.is_empty(), "weapon arm must not hook normal-arm recoil");
        let wa = anims.weapon_arm.as_ref().expect("weapon_arm rig loaded");
        assert_eq!(wa.phase, WeaponArmPhase::Stowed);
        let joint_count = wa.joints.len();
        assert_eq!(
            joint_count,
            rig.bones.iter().filter(|b| b.parent.is_some()).count(),
            "every non-root weapon arm bone must be driven"
        );
        // `rest_pose()` must pose the weapon arm into its folded stowed clip for the garage.
        let rest = anims.rest_pose();
        assert_eq!(rest.len(), joint_count);

        // Combat mode: Stowed -> Deploying -> Deployed -> Firing -> Deployed -> Stowing -> Stowed.
        let wa = anims.weapon_arm.as_mut().unwrap();
        wa.tick(true, false, false, 1.0);
        assert_eq!(wa.phase, WeaponArmPhase::Deploying);
        for _ in 0..200 {
            wa.tick(true, false, false, 1.0);
        }
        assert_eq!(wa.phase, WeaponArmPhase::Deployed);
        wa.tick(true, true, true, 1.0);
        assert_eq!(wa.phase, WeaponArmPhase::Firing);
        for _ in 0..70 {
            wa.tick(true, false, false, 1.0);
        }
        assert_eq!(wa.phase, WeaponArmPhase::Deployed);
        // Scan mode: Deployed -> Stowing -> Stowed.
        wa.tick(false, false, false, 1.0);
        assert_eq!(wa.phase, WeaponArmPhase::Stowing);
        for _ in 0..200 {
            wa.tick(false, false, false, 1.0);
        }
        assert_eq!(wa.phase, WeaponArmPhase::Stowed);
    }
}
