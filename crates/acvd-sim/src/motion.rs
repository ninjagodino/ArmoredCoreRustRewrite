//! Pure 60 Hz AC movement state machine, moved from `acvd-game/src/control.rs`.
//!
//! This is the movement subset of the old `AcMotion`: velocity, facing, EN, boost/glide, high
//! boost, wall jump, kick, blade motion, and landings. The follow camera, animation clip
//! selection, shader blur, camera shakes, and ECS resources stay in `acvd-game`.
//!
//! Absolute aim (`aim_yaw_off`/`aim_pitch`) replaces `FollowCam::yaw_off`/`pitch`; the game
//! mirrors its camera into these before each step.

use acvd_data::generated::ctrl::AcCtrlParam;
use acvd_data::generated::tuning::NEW_AC_BEHAVIOR;
use glam::{Quat, Vec2, Vec3};

use crate::collision::{Collision, Layer, RAY_HALF};
use crate::input::InputFrame;
use crate::map::AreaZone;

/// Movement ticks per second; `AcCtrlParam`'s `_tick` values are per tick.
const TICK_RATE: f32 = 60.0;
/// Metres per tick to km/h.
const TICK_TO_KMH: f32 = TICK_RATE * 3.6;
/// Normalized Boost Charge hold threshold that auto-fires `HighBoostState` (360 `0x8237c1b8`,
/// float `0x8200d3f0`).
const QB_AUTO_FIRE: f32 = 0.9999;
/// Directional wall-kick vertical acceleration multiplier (`360 0x82824560` at `0x82824688`,
/// float `0x8371398c` = 0.5).
const WALL_JUMP_DIR_V_ACC_SCALE: f32 = 0.5;
/// Directional wall-kick vertical rise cap multiplier (`360 0x82824560` at `0x828246e0`,
/// float `0x83713988` = 0.8).
const WALL_JUMP_DIR_V_CAP_SCALE: f32 = 0.8;

/// Active `WallJumpState` (360 vtable `0x82092a20`: ctor `0x828410a0`, enter `0x828436e0`,
/// update `0x82844648`).
#[derive(Clone, Copy, Debug)]
pub enum WallJumpPhase {
    /// Phase 0 (`0x828438c0`): wind-up on the wall (`trijump_prep_frames`, playing
    /// `wall_jump_prep` rows 231/233/235/237). At expiry computes the launch direction via
    /// `0x828abac0`, zeroes `velocity.y` (`0x8281f100`), and enters `Accel`.
    Prep {
        timer: f32,
        wall_normal: Vec3,
        clip_dir: u8,
    },
    /// Phase 1 (`0x82841190`): launch acceleration (`trijump_accel_frames`), calling
    /// `AcMove_wallJump` (`0x82824560`) and playing `wall_jump` rows 232/234/236/238.
    Accel { timer: f32, dir: Vec3, clip_dir: u8 },
    /// Phase 2 (`0x82843b88`): post-launch follow-through (`trijump_after_frames`), allowing
    /// turns at `trijump_turn_rate` (`0x8281fcd0`) until timer expiry or touchdown.
    After { timer: f32, clip_dir: u8 },
}

/// One tick's Wall Jump thrust command (`AcMove_wallJump` `0x82824560`, movement slot `+0x44`).
#[derive(Clone, Copy, Debug)]
struct WallJumpThrust {
    dir: Vec3,
    scale: f32,
}

/// Active `HighBoostState` (360 vtable `0x8200d378`: ctor `0x8237bec8`, update `0x8237c5a0`).
#[derive(Clone, Copy, Debug)]
pub enum QbPhase {
    /// Phase 0 (`0x8237c1b8`): Boost Charge wind-up while the button stays held, up to
    /// `qb_charge_max_frames` ticks (`+0x24`).
    Charge { held_ticks: f32 },
    /// Phase 1 (`0x8237ba68`): initial burst (`+0x10` = `qb_phase1_frames * w1`), adding the
    /// grounded vertical hop (`r7 = 1` in `0x82822980`) and triggering the directional clip at
    /// the Phase 1 -> Phase 2 boundary (`0x8284c3a0`).
    Phase1 {
        timer: f32,
        p1_ticks: f32,
        p2_ticks: f32,
        weight: f32,
        dir: Vec3,
        eighth: u8,
    },
    /// Phase 2 (`0x8237bf28`): main burst (`+0x14` = `qb_phase2_frames * w2`), ending when the
    /// timer expires or the AC touches down (`0x8283fc10`).
    Phase2 {
        timer: f32,
        p2_ticks: f32,
        weight: f32,
        dir: Vec3,
    },
}

/// One tick's High Boost thrust command (`0x82822980`, movement slot `+0x68` + `+0xd4` mode 4).
#[derive(Clone, Copy, Debug)]
struct QbThrust {
    weight: f32,
    scale: f32,
    dir: Vec3,
    hop: bool,
}

/// Active Boost Charge Kick state (`KickState` vtable `0x820928d0`, ctor `0x8283f8a8`, update
/// `0x82844570` â†’ `0x8283e1f0` / `0x828433a0`, `AcMove_boostChargeKick` `0x82823740`).
#[derive(Clone, Copy, Debug)]
pub struct KickState {
    /// Elapsed ticks since entering Phase 1 (`0..=kick_accel_frames + kick_after_frames`).
    pub timer: f32,
    /// Horizontal forward unit direction at kick launch.
    pub dir: Vec3,
    /// True once the single kick hit (`BulletExplosionSt` 4100 `damage_hit_count = 1`) has landed.
    pub hit_applied: bool,
}

/// Active melee blade phase (`BladeState`, phase table `0x83714024`:
/// `[0x828ae9e0, 0x828af000, 0x828af1c8]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BladePhase {
    /// Phase 0 (`0x828ae9e0`): wind-up charge (`blade_charge_frames` / `phys_blade_charge_frames` = 5 ticks).
    Charge,
    /// Phase 1 (`0x828af000`): homing lunge toward FCS-locked target (`AcMove_bladeDash` `0x828248f8`).
    Dash,
    /// Phase 2 (`0x828af1c8` first `blade_swing_frames` = 12 ticks): active swing & hit window.
    Swing,
    /// Phase 2 tail (`0x828af30c..0x828af350`, up to `blade_recovery_frames` = 30 ticks): braking recovery.
    Recovery,
}

/// Parameters passed to `AcMotion::start_blade` when triggering a melee blade weapon.
#[derive(Clone, Copy, Debug)]
pub struct BladeLaunch {
    /// `0` = right arm, `1` = left arm.
    pub hand_idx: usize,
    /// `0x8247f170`: true for KE physical blades & CE heat piles; false for TE laser blades.
    pub physical: bool,
    /// FCS-locked target entity & world position (if any).
    pub target: Option<(u64, Vec3)>,
    /// Weapon hit reach (`BulletBladeSt.hit_radius`, m).
    pub hit_radius: f32,
    /// Weapon blade capsule thickness (`BulletBladeSt.blade_radius`, m).
    pub blade_radius: f32,
    /// Max targets damaged per swing (`BulletBladeSt.damage_hit_count_chara`).
    pub max_hits: u8,
    /// Multi-hit power decay (`BulletBladeSt.decay_power`).
    pub decay_power: f32,
    /// Multi-hit recoil decay (`BulletBladeSt.decay_recoil`).
    pub decay_recoil: f32,
    /// Weapon damage attribute (`0` KE, `1` TE, `6` CE).
    pub damage_type: crate::damage::DamageType,
    /// Base weapon power (`acvparts.bin +0x18c`).
    pub damage_power: f32,
    /// Base weapon impact recoil (`acvparts.bin +0x190`).
    pub damage_recoil: f32,
    /// Blade trail FFX id (`acweaponsfxparam.bullet_sfx_id`, e.g. `4104` for laser blades).
    pub trail_fx: i32,
    /// Hit FFX ids (`bullethitsfxparam.dry_ac_low_damage` / `ac_hi_damage`).
    pub hit_low_fx: i32,
    pub hit_hi_fx: i32,
    /// Hit sound cue ids (`bullethitseparam.dry_ac_low_damage` / `ac_hi_damage`).
    pub hit_low_se: i16,
    pub hit_hi_se: i16,
}

/// Live melee blade attack state on `AcMotion`.
#[derive(Clone, Debug)]
pub struct BladeAction {
    pub launch: BladeLaunch,
    pub phase: BladePhase,
    /// Ticks elapsed in the current phase (`Charge`, `Dash`, or `Swing+Recovery`).
    pub timer: f32,
    /// Maximum ticks for `BladePhase::Dash` (`0x828ad510`).
    pub max_dash_ticks: f32,
    /// Entry horizontal speed in m/tick (`0x828ad328`).
    pub entry_speed_tick: f32,
    /// Live target position (updated each frame while target lives).
    pub target_pos: Option<Vec3>,
    /// Whether the attack started airborne (picks air vs ground `acmotion` rows).
    pub air_start: bool,
    /// Whether the attack uses the airborne downward-thrust clip (`NewAcBehavior +0x2e2` = 35Â°).
    pub downward: bool,
    /// True once the blade trail FFX has been spawned on entering `Swing`.
    pub trail_spawned: bool,
    /// Targets already struck during this swing (caps per-target hits at 1, total at `max_hits`).
    pub hit_entities: Vec<u64>,
}
/// Where an unturned AC faces: the assembled models' fronts point down -Z in Bevy axes.
const FORWARD: Vec3 = Vec3::NEG_Z;

/// Glide Boost phase (`GlideState` vtable `0x820926c8`, update `0x82841b38`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GlidePhase {
    /// Phase 0: the ground glide (movement vtable slot `+0x34` = `0x8281f540`, state 4).
    #[default]
    Ground,
    /// Phase 1: the air glide (slot `+0x38` = `0x82823618` -> `0x8281f8c0`, state 5).
    Air,
}

/// Movement state of the AC root. Velocity is in metres per tick, Bevy axes; `yaw` turns
/// about +Y (positive turns left), 0 facing `FORWARD`.
#[derive(Clone, Debug)]
pub struct AcMotion {
    pub ctrl: AcCtrlParam,
    /// Current generator energy (`AC + 0x214` `+0x04`, `0x82830910` / `0x828307c8`).
    pub en: f32,
    /// Generator energy capacity (`AC + 0x214` `+0x08`, `0x8247a390` -> `0x82858408`).
    pub en_max: f32,
    pub velocity: Vec3,
    pub yaw: f32,
    /// Camera-relative aim yaw (the old `FollowCam::yaw_off`), in radians.
    pub aim_yaw_off: f32,
    /// Aim pitch (the old `FollowCam::pitch`), in radians.
    pub aim_pitch: f32,
    /// Mouselook horizontal follow-rate lead (`FollowCam::hud_yaw_lead`).
    pub hud_yaw_lead: f32,
    pub boost: bool,
    pub glide: bool,
    /// Remaining height of the Glide Boost start ascent (m); set to `glide_lift_height` at
    /// activation and spent at `glide_rise_tick` per tick.
    pub glide_lift_left: f32,
    /// True while the fixed Glide Boost start ascent is applying.
    pub glide_lifting: bool,
    /// True once the current glide captured its start height and began the ascent.
    pub glide_lift_started: bool,
    /// Glide Boost phase: the 360 GlideState update `0x82841b38` runs the ground glide for
    /// `glide_ground_phase` seconds and then the air glide until the glide ends.
    pub glide_phase: GlidePhase,
    /// Seconds spent in the current glide (360 `GlideState +0x24`).
    pub glide_phase_time: f32,
    /// World feet height captured at glide activation (360 `GlideState +0xc`, copied to
    /// `AcMove +0x2b4` and `+0x318` by `0x8281f8c0`); the 5.0 m lift caps at this plus
    /// `glide_lift_height` (`0x8281dd28`).
    pub glide_activation_y: f32,
    /// True while a High Boost was started from an active Glide Boost (the glide quick boost).
    /// The 360 keeps the glide top-level action (`0x82092318`, update `0x8283f240`) alive while
    /// `HighBoostState` replaces only the movement substate, so a tapped Hi-Boost during a glide
    /// is a quick boost that keeps (and then resumes) the glide. A *held* Hi-Boost still becomes
    /// the Boost Charge Kick (`QbPhase::Phase2` -> `0x8283fdb0`).
    pub glide_qb: bool,
    pub airborne: bool,
    /// True while Scan Mode is active (`AC + 0xdb0 != 0`, `0x828b41a0`). Switches passive EN
    /// drain to `scan_passive_en_drain_tick` (`0x8286a558` / `0x828bef18`) and locks out firing.
    pub scan_mode: bool,
    /// True while an Overed Weapon is armed/charging/attacking (`0x82885c40 != 0`): switches
    /// generator EN output to `ow_en_output` (`0x82868798` -> `0x8287ef68`).
    pub ow_active: bool,
    /// True during post-OW purge lockout (`0x828861d0` state 2, `manual.fmg` 20814): drains EN
    /// and disables FCS lock-on for `lockout_secs`.
    pub ow_lockout: bool,
    /// Remaining 60 Hz ticks of an OW attack movement lunge (`NewAcBehavior` field 235 = 30 ticks,
    /// `0x82869d40` -> `AcMove +0x124/+0x128`).
    pub ow_dash_ticks: f32,
    pub ow_dash_dir: Vec3,
    /// Current map area band (`POINT_PARAM_ST` kinds `100`–`102`, `manual.fmg` 20500).
    pub area_zone: AreaZone,
    /// Move stick this frame, x right and y forward in the facing the integrator uses.
    /// The booster effect dispatcher (`0x828932a0`) reads this vector.
    pub command: Vec2,
    /// High boosts so far, whether the last one hit the Second-Stage sweet spot, and its world
    /// direction.
    pub quick_boosts: u32,
    pub second_stage_qb: bool,
    pub quick_dir: Vec3,
    /// `HighBoostState` Phase 0 charge entries (`0x8237ba10` -> `0x8283c8d0` TAE `0xC1D`/`0xC1E`).
    pub qb_charge_starts: u32,
    /// Quick-boost attempts rejected for EN shortage (plays `ac_qb_shortage` `304`, `0x82891ce8`).
    pub qb_shortages: u32,
    /// Whether the most recent High Boost / charge started airborne (selects ground vs air TAE
    /// `0xC1D/0xC1F/0xC21` vs `0xC1E/0xC20/0xC22`).
    pub qb_airborne: bool,
    /// Body-relative eighth (`0..=7`) of the most recent High Boost release.
    pub qb_eighth: u8,
    /// Most recent `HighBoostState` TAE event ID fired (`0xC1D..=0xC22`, `0x8283c8d0`).
    pub last_qb_tae: u32,
    /// Surface material directly beneath the AC from `.hmd` triangles (`0x829ee1d0`).
    pub ground_surface: crate::collision::GroundSurface,
    /// Monotonic counter of ground touchdowns (so `sfx` observes every landing cleanly).
    pub landings: u32,
    /// World point where the AC entered water before `check_water_return` teleported it.
    pub water_splash_at: Option<Vec3>,
    /// Wall kicks launched so far (`WallJumpState` Phase 0 -> Phase 1, `0x828438c0`).
    pub wall_jumps: u32,
    /// Boost Charge Kicks started and landed (`KickState` `0x820928d0`).
    pub kicks_started: u32,
    pub kicks_landed: u32,
    /// Melee blade swings started and target hits landed (`BladeState` `0x83714024`).
    pub blade_swings: u32,
    pub blade_hits: u32,
    /// Active High Boost / Boost Charge state (`0x8200d378`), and re-fire cooldown ticks
    /// (`action_ctrl +0x110`, `0x8283ed98`).
    pub qb: Option<QbPhase>,
    pub qb_cooldown: f32,
    /// Active Wall Jump state (`0x82092a20`), and wall-kick lockout ticks (`action_ctrl +0x114`:
    /// set to `wall_jump_post_jump_lockout` on normal jump `0x8283d4b8` and
    /// `trijump_lockout_frames` on wall kick `0x82841338`).
    pub wj: Option<WallJumpPhase>,
    pub wj_lockout: f32,
    pub wall_jump_queued: bool,
    /// Active Boost Charge Kick state (`KickState` `0x820928d0`, `0x8283fdb0`).
    pub kick: Option<KickState>,
    /// Active melee blade state (`BladeState` `0x83714024`).
    pub blade: Option<BladeAction>,
    /// World position before the last movement frame (for swept melee collision).
    pub prev_position: Vec3,
    /// The body-relative eighth of a high boost whose clip starts on the next state pick.
    pub quick_pending: Option<u8>,
    /// Ticks of take-off acceleration left.
    pub takeoff: f32,
    pub jump_queued: bool,
    /// Touched down during the last steps, and downward impact speed at touchdown (km/h).
    pub landed: bool,
    pub landing_impact_kmh: f32,
}
impl AcMotion {
    /// Whether a High Boost / Boost Charge (`HighBoostState`, `0x8200d378`) is active.
    pub fn in_quick_boost(&self) -> bool {
        self.qb.is_some()
    }

    /// Whether `WallJumpState` (`0x82092a20`) is in its Phase 1 acceleration burst.
    pub fn in_wall_jump_accel(&self) -> bool {
        matches!(self.wj, Some(WallJumpPhase::Accel { .. }))
    }

    /// Whether a Boost Charge Kick (`KickState`, `0x820928d0`) is active.
    pub fn in_kick(&self) -> bool {
        self.kick.is_some()
    }

    /// Whether a melee blade attack (`BladeState`, `0x83714024`) is active.
    pub fn in_blade(&self) -> bool {
        self.blade.is_some()
    }

    /// Whether a melee blade attack is actively lunging or swinging (lights boosters).
    pub fn in_blade_dash(&self) -> bool {
        matches!(
            self.blade.as_ref().map(|b| b.phase),
            Some(BladePhase::Dash | BladePhase::Swing)
        )
    }

    /// Current horizontal speed in km/h (used by `SpeedWeightScale` `0x828a2e50`).
    pub fn horiz_speed_kmh(&self) -> f32 {
        Vec2::new(self.velocity.x, self.velocity.z).length() * TICK_TO_KMH
    }

    /// Starts Boost Charge Kick (`AcActCtrl_startKick` `0x8283fdb0`).
    pub fn start_kick(&mut self) -> bool {
        if self.kick.is_some() || self.blade.is_some() || self.wj.is_some() {
            return false;
        }
        if !self.try_drain_en(self.ctrl.qb_en_drain) {
            return false;
        }
        let dir = Quat::from_rotation_y(self.yaw + self.aim_yaw_off) * FORWARD;
        let dir = Vec3::new(dir.x, 0.0, dir.z).normalize_or(FORWARD);
        self.qb = None;
        self.glide = false;
        self.glide_qb = false;
        self.kicks_started += 1;
        let horiz = Vec2::new(self.velocity.x, self.velocity.z);
        let speed = horiz.length().max(self.ctrl.kick_max_tick);
        self.velocity.x = dir.x * speed;
        self.velocity.z = dir.z * speed;
        self.kick = Some(KickState {
            timer: 0.0,
            dir,
            hit_applied: false,
        });
        true
    }

    /// Pure kick rebound (`0x828433b8` -> `0x8283cef8`); camera shake stays in the client.
    pub fn on_kick_contact(&mut self) {
        if let Some(kick) = self.kick.take() {
            self.kicks_landed += 1;
            self.velocity.x = -kick.dir.x * (self.ctrl.walk_max_tick * 0.5);
            self.velocity.z = -kick.dir.z * (self.ctrl.walk_max_tick * 0.5);
        }
    }

    /// Starts a melee blade attack (`BladeState` `0x83714024`, `0x8283ec20`).
    pub fn start_blade(&mut self, launch: BladeLaunch) -> bool {
        if self.blade.is_some() || self.kick.is_some() || self.wj.is_some() {
            return false;
        }
        self.qb = None;
        self.glide = false;
        let entry_speed_tick = Vec2::new(self.velocity.x, self.velocity.z).length();
        let speed_bonus_ticks = NEW_AC_BEHAVIOR.blade_homing_time_speed_coef
            * (entry_speed_tick * TICK_TO_KMH)
            * 0.01
            * TICK_RATE;
        let max_dash_ticks = if launch.target.is_some() {
            (NEW_AC_BEHAVIOR.blade_dash_max_frames + speed_bonus_ticks).clamp(18.0, 45.0)
        } else {
            NEW_AC_BEHAVIOR.blade_dash_max_frames
        };
        let target_pos = launch.target.map(|(_, pos)| pos);
        let downward = self.airborne
            && self.aim_pitch.to_degrees() <= -f32::from(NEW_AC_BEHAVIOR.blade_down_angle_deg);
        self.blade_swings += 1;
        self.blade = Some(BladeAction {
            launch,
            phase: BladePhase::Charge,
            timer: 0.0,
            max_dash_ticks,
            entry_speed_tick,
            target_pos,
            air_start: self.airborne,
            downward,
            trail_spawned: false,
            hit_entities: Vec::new(),
        });
        true
    }

    /// Launches an Overed Weapon attack dash burst (`0x82869d40`).
    pub fn start_ow_dash(&mut self, dir: Vec3) {
        let horiz = Vec3::new(dir.x, 0.0, dir.z).normalize_or_zero();
        let dir = if horiz != Vec3::ZERO {
            horiz
        } else {
            Quat::from_rotation_y(self.yaw) * FORWARD
        };
        let frames =
            (acvd_data::generated::tuning::NEW_AC_BEHAVIOR.ow_attack_dash_frames as f32).max(1.0);
        self.ow_dash_ticks = frames;
        self.ow_dash_dir = dir;
        self.qb = None;
        self.glide = false;
    }

    /// A standing AC with full EN.
    pub fn new(ctrl: AcCtrlParam) -> Self {
        let en_max = ctrl.en_capacity.max(0.0);
        Self {
            ctrl,
            en: en_max,
            en_max,
            velocity: Vec3::ZERO,
            yaw: 0.0,
            aim_yaw_off: 0.0,
            aim_pitch: 0.0,
            hud_yaw_lead: 0.0,
            boost: false,
            glide: false,
            glide_lift_left: 0.0,
            glide_lifting: false,
            glide_lift_started: false,
            glide_phase: GlidePhase::Ground,
            glide_phase_time: 0.0,
            glide_activation_y: 0.0,
            glide_qb: false,
            airborne: false,
            scan_mode: false,
            ow_active: false,
            ow_lockout: false,
            ow_dash_ticks: 0.0,
            ow_dash_dir: FORWARD,
            area_zone: AreaZone::Inside,
            command: Vec2::ZERO,
            quick_boosts: 0,
            second_stage_qb: false,
            quick_dir: FORWARD,
            qb_charge_starts: 0,
            qb_shortages: 0,
            qb_airborne: false,
            qb_eighth: 0,
            last_qb_tae: 0,
            ground_surface: crate::collision::GroundSurface::Default,
            landings: 0,
            water_splash_at: None,
            wall_jumps: 0,
            kicks_started: 0,
            kicks_landed: 0,
            blade_swings: 0,
            blade_hits: 0,
            qb: None,
            qb_cooldown: 0.0,
            wj: None,
            wj_lockout: 0.0,
            wall_jump_queued: false,
            kick: None,
            blade: None,
            prev_position: Vec3::ZERO,
            quick_pending: None,
            takeoff: 0.0,
            jump_queued: false,
            landed: false,
            landing_impact_kmh: 0.0,
        }
    }

    /// Attempts to spend `amount` EN (`360 0x82830910(..., is_passive = 0)`).
    pub fn try_drain_en(&mut self, amount: f32) -> bool {
        if amount <= 0.0 {
            return true;
        }
        if self.en < amount {
            return false;
        }
        self.en = (self.en - amount).max(0.0);
        true
    }

    /// Fire direction from the absolute aim.
    pub fn aim_direction(&self) -> Vec3 {
        Quat::from_rotation_y(self.yaw + self.aim_yaw_off)
            * Quat::from_rotation_x(self.aim_pitch)
            * FORWARD
    }

    /// Lock-Sight 3D projection direction (`0x829d2120`).
    pub fn sight_direction(&self) -> Vec3 {
        Quat::from_rotation_y(self.yaw + self.aim_yaw_off + self.hud_yaw_lead)
            * Quat::from_rotation_x(self.aim_pitch)
            * FORWARD
    }

    /// Current aim angles `(yaw, pitch)` in radians (`yaw + yaw_off`, `pitch`).
    pub fn aim_angles(&self) -> (f32, f32) {
        (self.yaw + self.aim_yaw_off, self.aim_pitch)
    }
}
/// `stick` (camera-relative) turned into the body's frame, for the directional clips.
pub fn body_stick(stick: Vec2, yaw_off: f32) -> Vec2 {
    let (sin, cos) = yaw_off.sin_cos();
    Vec2::new(stick.x * cos - stick.y * sin, stick.x * sin + stick.y * cos)
}

/// Piecewise-linear Boost Charge weight curve (`360 0x8237bbf8`, `0x8202081c` = 0.5):
/// holds `base` before `start`, ramps to `1.0` at `center - width * 0.5`, holds `1.0` across the
/// Second-Stage sweet spot `[center - width * 0.5, center + width * 0.5]`, and ramps to `over` at
/// `1.0` (`qb_charge_max_frames`).
fn charge_curve(x: f32, start: f32, base: f32, center: f32, width: f32, over: f32) -> f32 {
    if x < start {
        return base;
    }
    let half = width * 0.5;
    let lo = center - half;
    if x < lo {
        return if lo - start > 1e-6 {
            base + (x - start) / (lo - start) * (1.0 - base)
        } else {
            1.0
        };
    }
    let hi = center + half;
    if x <= hi {
        return 1.0;
    }
    if 1.0 - hi > 1e-6 {
        1.0 + (x - hi) / (1.0 - hi) * (over - 1.0)
    } else {
        over
    }
}

/// `HighBoostState` TAE event IDs (`360 0x8283c8d0`, verified in `private/xenia/fx_polish2.jsonl`):
/// - Charge enter (`0x8237ba10`): `0xC1D` (ground) / `0xC1E` (air)
/// - Normal release (`0x8237c1b8`): `0xC1F` (ground) / `0xC20` (air) -> FFX `0x2FE` (`766`, 1.5)
/// - Second-Stage release (`0x8237c1b8`, `w2 >= 1.0`): `0xC21` (ground) / `0xC22` (air)
///   -> FFX `0x79` (`121`, 1.5) + `0x2FE` (`766`, 1.5)
pub fn high_boost_tae_id(release: bool, second_stage: bool, airborne: bool) -> u32 {
    match (release, second_stage, airborne) {
        (false, _, false) => 0x0C1D,
        (false, _, true) => 0x0C1E,
        (true, false, false) => 0x0C1F,
        (true, false, true) => 0x0C20,
        (true, true, false) => 0x0C21,
        (true, true, true) => 0x0C22,
    }
}

/// Enters `HighBoostState` Phase 0 (`360 0x8283ed98` -> `0x82844ea8` -> `0x8237bec8`).
/// Drains `qb_en_drain` (`BST__HI_EN * 0.7`, Lua param 57) via `0x828b2648` -> `0x82830910` and
/// blocks launch when EN is insufficient (Xenia probe `private/xenia/en_drain2.jsonl`), recording
/// an EN-shortage QB attempt so `sfx` can fire `ac_qb_shortage` (`304`, `0x82891ce8`).
///
/// `from_glide` marks the glide quick boost: the 360 glide top-level action stays active while
/// `HighBoostState` runs, so the glide survives the burst and resumes when it ends (see
/// `AcMotion::glide_qb`). A normal High Boost still clears the glide.
pub fn start_quick_boost(p: &mut AcMotion, from_glide: bool) -> bool {
    if p.qb.is_some() || p.kick.is_some() || p.blade.is_some() || p.qb_cooldown >= 1.0 {
        return false;
    }
    if !p.try_drain_en(p.ctrl.qb_en_drain) {
        p.qb_shortages += 1;
        p.qb_airborne = p.airborne;
        return false;
    }
    p.qb_cooldown = p.ctrl.qb_reload_ticks;
    if from_glide && p.glide {
        p.glide_qb = true;
    } else {
        p.glide = false;
        p.glide_qb = false;
    }
    p.qb_charge_starts += 1;
    p.qb_airborne = p.airborne;
    p.last_qb_tae = high_boost_tae_id(false, false, p.airborne);
    p.qb = Some(QbPhase::Charge { held_ticks: 0.0 });
    true
}

/// Evaluates the Boost Charge curve at `hold_ratio` (`0..=1`) and enters `HighBoostState` Phase 1
/// (`360 0x8237c1b8`, `0x8237c2dc..0x8237c364`). Does not force boost mode on (`private/xenia/quick_boost.jsonl` Hit 1).
fn fire_quick_boost(p: &mut AcMotion, stick: Vec2, hold_ratio: f32) {
    let c = p.ctrl;
    let x = hold_ratio.clamp(0.0, 1.0);
    let (w1, w2) = if c.qb_charge_max_frames > 0.0 {
        let inv = 1.0 / c.qb_charge_max_frames;
        let start = c.qb_charge_start_frames * inv;
        let center = c.qb_sweet_frames * inv;
        let width = c.qb_sweet_width_frames * inv;
        let over = c.qb_over_rate * 0.01;
        (
            charge_curve(x, start, c.qb_base_rate1 * 0.01, center, width, over),
            charge_curve(x, start, c.qb_base_rate2 * 0.01, center, width, over),
        )
    } else {
        (c.qb_base_rate1 * 0.01, c.qb_base_rate2 * 0.01)
    };
    let stick = stick.try_normalize().unwrap_or(Vec2::Y);
    let forward = Quat::from_rotation_y(p.yaw + p.aim_yaw_off) * FORWARD;
    let world = forward.cross(Vec3::Y) * stick.x + forward * stick.y;
    let eighth = direction(body_stick(stick, p.aim_yaw_off));
    if !p.glide_qb {
        p.glide = false;
    }
    p.quick_dir = world;
    p.quick_boosts += 1;
    p.second_stage_qb = w2 >= 1.0;
    p.qb_airborne = p.airborne;
    p.qb_eighth = eighth;
    p.last_qb_tae = high_boost_tae_id(true, p.second_stage_qb, p.airborne);
    let p1_ticks = c.qb_phase1_frames * w1;
    let p2_ticks = c.qb_phase2_frames * w2;
    p.qb = Some(QbPhase::Phase1 {
        timer: 0.0,
        p1_ticks,
        p2_ticks,
        weight: w1,
        dir: world,
        eighth,
    });
    let _qb_max = c.quick_boost_min_tick + (c.quick_boost_max_tick - c.quick_boost_min_tick) * w1;
}

/// Steps `HighBoostState` (`360 0x8237c5a0`: Phase 0 `0x8237c1b8`, Phase 1 `0x8237ba68`,
/// Phase 2 `0x8237bf28`) by one 60 Hz tick.
fn step_qb(p: &mut AcMotion, input: &InputFrame, moving: bool) -> Option<QbThrust> {
    let phase = p.qb?;
    match phase {
        QbPhase::Charge { mut held_ticks } => {
            held_ticks += 1.0;
            let max_f = p.ctrl.qb_charge_max_frames;
            let x = if max_f > 0.0 { held_ticks / max_f } else { 1.0 };
            if input.qb_held && x < QB_AUTO_FIRE {
                p.qb = Some(QbPhase::Charge { held_ticks });
            } else {
                fire_quick_boost(p, input.stick, x);
            }
            Some(QbThrust {
                weight: 0.0,
                scale: 0.0,
                dir: p.quick_dir,
                hop: false,
            })
        }
        QbPhase::Phase1 {
            timer,
            p1_ticks,
            p2_ticks,
            weight,
            dir,
            eighth,
        } => {
            let next_t = timer + 1.0;
            let hop = !p.airborne;
            if next_t <= p1_ticks {
                p.qb = Some(QbPhase::Phase1 {
                    timer: next_t,
                    p1_ticks,
                    p2_ticks,
                    weight,
                    dir,
                    eighth,
                });
                Some(QbThrust {
                    weight,
                    scale: 1.0,
                    dir,
                    hop,
                })
            } else {
                let scale = (p1_ticks - timer).max(0.0);
                p.quick_pending = Some(eighth);
                p.qb = Some(QbPhase::Phase2 {
                    timer: 0.0,
                    p2_ticks,
                    weight,
                    dir,
                });
                Some(QbThrust {
                    weight,
                    scale,
                    dir,
                    hop,
                })
            }
        }
        QbPhase::Phase2 {
            timer,
            p2_ticks,
            weight,
            dir,
        } => {
            // `0x8237bf28` -> `0x8237bcb0` -> `0x8283fdb0`: holding High Boost (`â–¡ + HOLD`,
            // manual 20301/20401) into Phase 2 launches Boost Charge Kick (`KickState`).
            if input.qb_held && p.start_kick() {
                return None;
            }
            // Glide Boost cancels an active Phase 2 burst. A glide quick boost already has its
            // glide running, so it must not cancel the burst it started.
            if !p.glide_qb
                && input.glide
                && p.boost
                && moving
                && !p.airborne
                && p.en >= p.ctrl.glide_en_tick
            {
                p.qb = None;
                p.glide = true;
                return None;
            }
            let next_t = timer + 1.0;
            if next_t <= p2_ticks {
                // `0x8237c054..0x8237c074`: touching the ground ends Phase 2 after this step's thrust.
                if !p.airborne {
                    p.qb = None;
                } else {
                    p.qb = Some(QbPhase::Phase2 {
                        timer: next_t,
                        p2_ticks,
                        weight,
                        dir,
                    });
                }
                Some(QbThrust {
                    weight,
                    scale: 1.0,
                    dir,
                    hop: false,
                })
            } else {
                let scale = (p2_ticks - timer).max(0.0);
                p.qb = None;
                Some(QbThrust {
                    weight,
                    scale,
                    dir,
                    hop: false,
                })
            }
        }
    }
}

/// Eighth of the circle `stick` points at, 0 forward, clockwise.
pub fn direction(stick: Vec2) -> u8 {
    let angle = stick.x.atan2(stick.y).rem_euclid(std::f32::consts::TAU);
    ((angle / std::f32::consts::FRAC_PI_4).round() as u8) % 8
}

/// Travel eighth for the air-glide entry (`0x82825918`: 0 forward, clockwise). Velocity in the
/// body's frame, the stick when nearly still. `x` is right, `y` is forward, the same axes as
/// [`direction`].
pub fn travel_eighth(p: &AcMotion, stick: Vec2) -> u8 {
    let forward = Quat::from_rotation_y(p.yaw) * FORWARD;
    let right = forward.cross(Vec3::Y);
    let v = Vec2::new(p.velocity.dot(right), p.velocity.dot(forward));
    direction(if v.length() > 0.02 { v } else { stick })
}

/// Glide clip while `glide` is held. On the ground: the start, then the ground wheel. In the
/// air: one directional entry (`glide_air_in`), then the air-float wheel (`glide_air`). Landing
/// returns to the ground wheel without replaying the start.
pub fn glide_clip(
    current: (&'static str, Option<u8>),
    airborne: bool,
    clip_ok: bool,
    finished: bool,
    eighth: u8,
) -> (&'static str, Option<u8>) {
    if airborne {
        return match current.0 {
            "glide_air" => current,
            "glide_air_in" if clip_ok && !finished => current,
            "glide_air_in" => ("glide_air", None),
            _ => ("glide_air_in", Some(eighth)),
        };
    }
    match current.0 {
        "glide" => current,
        "glide_start" if clip_ok && !finished => current,
        "glide_start" | "glide_air" | "glide_air_in" => ("glide", None),
        _ => ("glide_start", None),
    }
}

fn approach(v: Vec2, target: Vec2, step: f32) -> Vec2 {
    let d = target - v;
    if d.length() <= step {
        target
    } else {
        v + d.normalize() * step
    }
}

/// Glide Boost velocity update (360 `0x8281e408`, called from the move integrator `0x82822ea0`
/// for states 4/5). It adds the per-tick acceleration along the stick, then:
/// - below `max`: the new length clamps to `max` (`normalize(v + a) * min(|v + a|, max)`);
/// - at or above `max`: the speed is **held** (`normalize(v + a) * |v|`), only the direction
///   changes. It does not brake: entering a glide above glide max (from High Boost) keeps that
///   speed for the whole glide. Xenia probe `private/xenia/glide_move.jsonl` measured the glide
///   holding `sqrt(vx^2 + vz^2) = 0.4630` m/tick (`100.0 km/h`) across states 4 and 5.
fn glide_thrust(v: Vec2, accel: Vec2, max: f32) -> Vec2 {
    let speed = v.length();
    let next = v + accel;
    if speed < max {
        next.clamp_length_max(max)
    } else {
        next.normalize_or(v / speed) * speed
    }
}

/// Glide heading gate (`0x8281ea30` for state 4, `0x8281e898` for state 5): both return 0 and
/// end the glide when `cos(stick heading - velocity heading) < -sin(block +0x20)`. The block
/// angle is a 10 degree allowance (`private/xenia/glide_gate.jsonl` reads 0.17453292 rad at both
/// entries), so the stick may sit up to 100 degrees off the current velocity. The comparison is
/// frame-invariant, so world directions work; at a standstill the 360 heading is local forward
/// (`atan2(0, 0) = 0`), i.e. the body forward here.
fn glide_heading_ok(wish: Vec2, horizontal: Vec2, forward: Vec3, cone: f32) -> bool {
    let v = if horizontal.length_squared() > 0.0 {
        horizontal.normalize_or_zero()
    } else {
        Vec2::new(forward.x, forward.z).normalize_or_zero()
    };
    wish.normalize_or_zero().dot(v) >= -cone.sin()
}

/// One tick of the move integrator (360 0x82822ea0) with a move action held: at or under `max`
/// the acceleration adds and the speed clamps to `max`; above it the acceleration only steers,
/// and the speed falls by `decel` per tick, not below `max`.
fn integrate(v: Vec2, accel: Vec2, max: f32, decel: f32) -> Vec2 {
    let speed = v.length();
    let next = v + accel;
    if speed <= max {
        return next.clamp_length_max(max);
    }
    next.normalize_or(v / speed) * (speed * (1.0 - decel)).max(max)
}

/// Computes the wall-kick launch direction (`360 0x828abac0`):
/// - Zero stick -> `(0, 1, 0)` (vertical wall kick).
/// - Stick away from the wall (`dot(wish, wall_normal) > 0`) -> `wish`.
/// - Stick straight into the wall (`dot < -cos(vertical_angle_rad)`) -> `(0, 1, 0)` (vertical).
/// - Stick obliquely into the wall -> horizontal `wall_normal` rotated by `Â±glance_rad` toward `wish`.
fn wall_jump_dir(
    stick: Vec2,
    facing: Quat,
    wall_normal: Vec3,
    vertical_angle_rad: f32,
    glance_rad: f32,
) -> Vec3 {
    if stick.length_squared() < 1e-6 {
        return Vec3::Y;
    }
    let forward = facing * FORWARD;
    let right = forward.cross(Vec3::Y);
    let wish = (right * stick.x + forward * stick.y).normalize_or_zero();
    if wish.length_squared() < 1e-6 {
        return Vec3::Y;
    }
    let n = wall_normal.normalize_or(Vec3::Z);
    let dot = wish.dot(n);
    if dot > 0.0 {
        return wish;
    }
    if dot < -vertical_angle_rad.cos() {
        return Vec3::Y;
    }
    let flat_n = Vec3::new(n.x, 0.0, n.z).normalize_or(Vec3::Z);
    let sign = if flat_n.cross(wish).y >= 0.0 {
        1.0
    } else {
        -1.0
    };
    Quat::from_rotation_y(sign * glance_rad) * flat_n
}

/// Body-local cardinal direction (0 front, 2 right, 4 back, 6 left) of the outward wall normal,
/// for `wall_jump_prep` (rows 237/235/231/233, `0x82884da8`) and `wall_jump` (rows 238/236/232/234,
/// `0x82884688`) via `WallJumpState::enter` (`0x828436e0`).
fn wall_jump_clip_dir(yaw: f32, wall_normal: Vec3) -> u8 {
    let forward = Quat::from_rotation_y(yaw) * FORWARD;
    let right = forward.cross(Vec3::Y);
    let local = Vec2::new(wall_normal.dot(right), wall_normal.dot(forward));
    if local.y.abs() >= local.x.abs() {
        if local.y >= 0.0 {
            0
        } else {
            4
        }
    } else if local.x >= 0.0 {
        2
    } else {
        6
    }
}

/// Steps `WallJumpState` (`360 0x82844648`: Phase 0 `0x828438c0`, Phase 1 `0x82841190`,
/// Phase 2 `0x82843b88`) by one 60 Hz tick.
fn step_wall_jump(p: &mut AcMotion, input: &InputFrame, facing: Quat) -> Option<WallJumpThrust> {
    let phase = p.wj?;
    let c = p.ctrl;
    match phase {
        WallJumpPhase::Prep {
            timer,
            wall_normal,
            clip_dir,
        } => {
            let next_t = timer + 1.0;
            if next_t >= c.trijump_prep_frames {
                let dir = wall_jump_dir(
                    input.stick,
                    facing,
                    wall_normal,
                    c.wall_jump_vertical_angle_rad,
                    c.trijump_glance_rad,
                );
                // `0x82843b24` -> `0x8281f100`: zero vertical velocity at launch.
                p.velocity.y = 0.0;
                p.wall_jumps += 1;
                p.wj = Some(WallJumpPhase::Accel {
                    timer: 0.0,
                    dir,
                    clip_dir,
                });
            } else {
                p.wj = Some(WallJumpPhase::Prep {
                    timer: next_t,
                    wall_normal,
                    clip_dir,
                });
            }
            None
        }
        WallJumpPhase::Accel {
            timer,
            dir,
            clip_dir,
        } => {
            let next_t = timer + 1.0;
            if next_t <= c.trijump_accel_frames {
                p.wj = Some(WallJumpPhase::Accel {
                    timer: next_t,
                    dir,
                    clip_dir,
                });
                Some(WallJumpThrust { dir, scale: 1.0 })
            } else {
                let scale = (c.trijump_accel_frames - timer).max(0.0);
                p.wj = Some(WallJumpPhase::After {
                    timer: 0.0,
                    clip_dir,
                });
                Some(WallJumpThrust { dir, scale })
            }
        }
        WallJumpPhase::After { timer, clip_dir } => {
            let next_t = timer + 1.0;
            if next_t > c.trijump_after_frames {
                p.wj = None;
            } else {
                p.wj = Some(WallJumpPhase::After {
                    timer: next_t,
                    clip_dir,
                });
            }
            None
        }
    }
}

/// Steps `KickState` (`360 0x820928d0`: `0x82844570` â†’ `0x828433a0`, `AcMove_boostChargeKick`
/// `0x82823740`) by one 60 Hz tick. Returns `(horizontal, skip_gravity)` when active.
fn step_kick(p: &mut AcMotion, horizontal: Vec2) -> Option<(Vec2, bool)> {
    let mut kick = p.kick?;
    let c = p.ctrl;
    let total = c.kick_accel_frames + c.kick_after_frames;
    kick.timer += 1.0;
    if kick.timer > total {
        p.kick = None;
        return None;
    }
    p.kick = Some(kick);

    let dir2 = Vec2::new(kick.dir.x, kick.dir.z).normalize_or(Vec2::NEG_Y);
    let next_h = if kick.timer <= c.kick_accel_frames {
        integrate(
            horizontal,
            dir2 * c.kick_accel_tick,
            c.kick_max_tick,
            c.over_max_decel_tick,
        )
    } else {
        // Carry kick momentum through the 35-tick follow-through without ground braking
        // (`+0x1c0 = 0` in `0x828435b4`), decaying gently if above `kick_max_tick`.
        let cur = horizontal.length();
        if cur > c.kick_max_tick {
            horizontal.normalize_or(dir2)
                * (cur * (1.0 - c.over_max_decel_tick)).max(c.kick_max_tick)
        } else {
            dir2 * cur.max(c.kick_max_tick * 0.85)
        }
    };

    // Vertical thrust (`0x82823788..0x82823964`): downward dive when pitched down
    // (`NewAcBehavior +0x538` = 3.0 m/s per tick / 60), else slight lift capped at `qb_hop_max_tick`.
    let pitch = p.aim_pitch;
    if pitch < 0.0 {
        let down_rad = NEW_AC_BEHAVIOR.kick_down_angle_deg.to_radians();
        let ignore_rad = NEW_AC_BEHAVIOR.kick_ignore_fall_angle_deg.to_radians();
        let denom = (-std::f32::consts::FRAC_PI_2 - ignore_rad).min(-0.1);
        let norm = (pitch.clamp(down_rad, 0.0) / denom).clamp(0.0, 1.0);
        p.velocity.y -= c.kick_down_accel_tick * norm;
    } else {
        p.velocity.y = (p.velocity.y + c.kick_accel_tick).min(c.qb_hop_max_tick);
    }
    Some((next_h, true))
}

/// Steps `BladeState` (`360 0x83714024`: Phase 0 `0x828ae9e0`, Phase 1 `0x828af000`,
/// Phase 2 `0x828af1c8`, `AcMove_bladeDash` `0x828248f8`) by one 60 Hz tick.
/// Returns `(horizontal, skip_gravity)` when active.
fn step_blade(
    p: &mut AcMotion,
    position: Vec3,
    horizontal: Vec2,
    brake: f32,
    drag: f32,
) -> Option<(Vec2, bool)> {
    let mut b = p.blade.take()?;
    let c = p.ctrl;
    b.timer += 1.0;

    // Steer AC facing toward the FCS-locked target during Charge, Dash, and Swing (`0x828ad710`).
    if !matches!(b.phase, BladePhase::Recovery) {
        if let Some(tpos) = b.target_pos {
            let to = Vec2::new(tpos.x - position.x, tpos.z - position.z);
            if to.length_squared() > 1e-4 {
                // FORWARD is -Z (`sin(-yaw) = x`, `-cos(-yaw) = z` -> `yaw = atan2(-dx, -dz)`).
                let target_yaw = (-to.x).atan2(-to.y);
                let mut diff = (target_yaw - p.yaw + std::f32::consts::PI)
                    .rem_euclid(std::f32::consts::TAU)
                    - std::f32::consts::PI;
                let max_step = (c.turn_rate * 2.5) / TICK_RATE;
                diff = diff.clamp(-max_step, max_step);
                p.yaw += diff;
            }
        }
    }

    let forward = Quat::from_rotation_y(p.yaw) * FORWARD;
    let forward_h = Vec2::new(forward.x, forward.z).normalize_or(Vec2::NEG_Y);
    let dist_to_target = b.target_pos.map(|t| (t - position).with_y(0.0).length());

    let (next_h, skip_gravity) = match b.phase {
        BladePhase::Charge => {
            let charge_ticks = if b.launch.physical {
                NEW_AC_BEHAVIOR.phys_blade_charge_frames
            } else {
                NEW_AC_BEHAVIOR.blade_charge_frames
            };
            let h = if p.airborne {
                horizontal * (1.0 - drag).clamp(0.0, 1.0)
            } else {
                approach(horizontal, Vec2::ZERO, brake)
            };
            if b.timer >= charge_ticks {
                b.timer = 0.0;
                let instant_dist = NEW_AC_BEHAVIOR.blade_instant_swing_dist;
                if b.max_dash_ticks > 0.0 && dist_to_target.is_some_and(|d| d > instant_dist) {
                    b.phase = BladePhase::Dash;
                } else {
                    b.phase = BladePhase::Swing;
                }
            }
            (h, false)
        }
        BladePhase::Dash | BladePhase::Swing => {
            let dir_h = b
                .target_pos
                .map(|t| Vec2::new(t.x - position.x, t.z - position.z).normalize_or(forward_h))
                .unwrap_or(forward_h);
            // `0x828ad328`: entry speed ratio between `blade_min_speed_kmh` (20) and `blade_max_speed_kmh` (150),
            // combined with `0x828aeb68` distance scale (`min(1, dist / 50 m)`).
            let entry_kmh = b.entry_speed_tick * TICK_TO_KMH;
            let min_s = f32::from(NEW_AC_BEHAVIOR.blade_min_speed_kmh);
            let max_s = f32::from(NEW_AC_BEHAVIOR.blade_max_speed_kmh).max(min_s + 1.0);
            let spd_rate = ((entry_kmh - min_s) / (max_s - min_s))
                .clamp(NEW_AC_BEHAVIOR.blade_min_accel_rate, 1.0)
                .max(0.4);
            let dist_rate = dist_to_target
                .map(|d| (d / NEW_AC_BEHAVIOR.blade_homing_full_dist.max(1.0)).clamp(0.4, 1.0))
                .unwrap_or(1.0);
            let accel = dir_h * (c.blade_accel_tick * spd_rate * dist_rate);
            let h = integrate(horizontal, accel, c.blade_max_tick, c.over_max_decel_tick);

            // Vertical homing toward locked target (`AcMove_bladeDash` `0x82824a04..0x82824a24`).
            let mut hover = false;
            if let Some(tpos) = b.target_pos {
                let dy = tpos.y - (position.y + 4.0);
                if dy > 1.0 {
                    p.airborne = true;
                    p.velocity.y =
                        (p.velocity.y + c.quick_boost_v_acc_tick).min(c.blade_rise_max_tick);
                    hover = true;
                } else if dy < -1.0 && p.airborne {
                    p.velocity.y =
                        (p.velocity.y - c.quick_boost_v_acc_tick).max(-c.blade_rise_max_tick);
                    hover = true;
                } else if p.airborne {
                    p.velocity.y *= 0.85;
                    hover = true;
                }
            }

            if b.phase == BladePhase::Dash {
                let instant_dist = NEW_AC_BEHAVIOR.blade_instant_swing_dist;
                if b.timer >= NEW_AC_BEHAVIOR.blade_dash_min_frames
                    && (dist_to_target.is_none_or(|d| d <= instant_dist)
                        || b.timer >= b.max_dash_ticks)
                {
                    b.phase = BladePhase::Swing;
                    b.timer = 0.0;
                }
            } else {
                let swing_ticks = f32::from(NEW_AC_BEHAVIOR.blade_swing_frames).max(1.0);
                if b.timer >= swing_ticks {
                    b.phase = BladePhase::Recovery;
                }
            }
            (h, hover)
        }
        BladePhase::Recovery => {
            // `0x828af30c..0x828af350`: brake until `blade_recovery_frames` (30 ticks) elapse.
            let h = if p.airborne {
                horizontal * (1.0 - drag).clamp(0.0, 1.0)
            } else {
                approach(horizontal, Vec2::ZERO, brake)
            };
            let total_ticks = f32::from(NEW_AC_BEHAVIOR.blade_recovery_frames)
                .max(f32::from(NEW_AC_BEHAVIOR.blade_swing_frames));
            if b.timer >= total_ticks {
                return Some((h, false));
            }
            (h, false)
        }
    };

    p.blade = Some(b);
    Some((next_h, skip_gravity))
}

/// One 60 Hz tick of generator output minus passive part EN drain (`360 0x828bef18` at
/// `0x828bf6cc..0x828bf74c`: `0x8287edd8` -> `0x8287eef0` -> `0x82830910(passive=1)` ->
/// `0x8287ef40` -> `0x828307c8`; Xenia probes `private/xenia/en_drain{,2}.jsonl`).
/// In Scan Mode (`AC + 0xdb0 != 0`, `0x828bf6d4..0x828bf6ec`) uses `0x82868268`
/// (`scan_passive_en_drain_tick`, `0x8286a558` + `NewAcBehavior` fields 319-331) for faster EN
/// recharge. Runs every tick with no recovery delay and the same rate on ground and in air.
fn step_en(p: &mut AcMotion) {
    if p.ow_lockout {
        // Post-OW purge lockout (`0x828861d0` state 2, `manual.fmg` 20814): EN stays depleted.
        p.en = 0.0;
        return;
    }
    let drain = if p.scan_mode {
        p.ctrl.scan_passive_en_drain_tick
    } else {
        p.ctrl.passive_en_drain_tick
    };
    // While OW is active (`0x82885c40 != 0`), generator output uses `gene.ow_en_output`
    // (`0x82868798` -> `0x82855ad8` -> `0x8287ef68`).
    let output_tick = if p.ow_active && p.ctrl.ow_en_output > 0.0 {
        p.ctrl.ow_en_output / TICK_RATE
    } else {
        p.ctrl.en_output_tick
    };
    let net = output_tick - drain;
    p.en = (p.en + net).clamp(0.0, p.en_max);
}

/// AC-vs-MapObject collision damage before `DestroyApSt` scaling (`0x82864008`, `0x828b2b50`,
/// `0x82865470`): `ac_rigidness * impact_speed_kmh * DAMAGE.obj_collision_damage_coef`.
fn ac_prop_collision_raw_damage(total_weight: f32, velocity: Vec3, normal: Vec3) -> f32 {
    let v_kmh = velocity * TICK_TO_KMH;
    let speed_kmh = v_kmh.length();
    if speed_kmh < 0.5 {
        return 0.0;
    }
    let dmg = &acvd_data::generated::tuning::DAMAGE;
    let v_weighted = Vec3::new(v_kmh.x, v_kmh.y * dmg.vertical_collision_coef, v_kmh.z);
    let d = normal.dot(v_weighted.normalize_or_zero());
    // `0x82864008` uses `-dot(normal, v_weighted.norm)` when moving into the surface (`d < 0`).
    // When walking/sliding across the top of a low ground prop (`normal.y > 0.5`), apply a
    // grazing floor (`0.25`) so low crates/fences/debris are crushed underfoot as on 360.
    let alignment = if d < 0.0 {
        -d
    } else if normal.y > 0.5 && Vec2::new(v_kmh.x, v_kmh.z).length() >= 1.0 {
        0.25
    } else {
        return 0.0;
    };
    let impact_speed_kmh = speed_kmh * alignment;
    let ac_rigidness = (total_weight.max(1000.0) / 1000.0) * dmg.weight_to_rigidness;
    ac_rigidness * impact_speed_kmh * dmg.obj_collision_damage_coef
}

/// Applies AC body collision damage to any active destructible props touching the AC right before
/// `push_out`, and immediately disables broken props in `collision` so the AC crashes straight
/// through without losing momentum (`0x82865890` + `0x82865470`).
fn crush_contacted_props(
    p: &AcMotion,
    feet: Vec3,
    collision: &mut Collision,
    mut apply_to_prop: impl FnMut(u32, f32) -> bool,
) {
    if p.velocity.length_squared() < 1e-8 {
        return;
    }
    let contacts = collision.body_prop_contacts(feet);
    for (prop_id, normal) in contacts {
        let raw = ac_prop_collision_raw_damage(p.ctrl.total_weight, p.velocity, normal);
        if raw > 0.0 && apply_to_prop(prop_id, raw) {
            collision.break_prop(prop_id);
        }
    }
}

/// One 60 Hz tick of movement over the highest ground under the AC.
#[cfg_attr(not(test), allow(dead_code))]
fn step(p: &mut AcMotion, input: &InputFrame, position: &mut Vec3, collision: &Collision) {
    step_inner(p, input, position, collision, |_| {});
}

/// One 60 Hz tick of movement that also applies AC body collision damage to destructible map
/// objects (`0x82865470`) before `push_out`, letting the AC crash straight through broken props.
fn step_with_props(
    p: &mut AcMotion,
    input: &InputFrame,
    position: &mut Vec3,
    collision: &mut Collision,
    mut apply_to_prop: impl FnMut(u32, f32) -> bool,
) {
    // Run the movement integration up to `*position += p.velocity`, then crush any contacted
    // destructibles and disable their `_h.hmd` triangles before `push_out` runs.
    let mut crush = |pilot: &AcMotion, feet: Vec3, col: &mut Collision| {
        crush_contacted_props(pilot, feet, col, &mut apply_to_prop);
    };
    step_core(p, input, position, collision, Some(&mut crush));
}

/// The turn rate the current movement state actually uses, in radians per second.
///
/// `WallJumpState` Phases 0 and 2 steer at `trijump_turn_rate`, Phase 1 does not steer, and the
/// normal movers use `turn_rate` (see `step_move_integrate`).
pub fn effective_turn_rate(p: &AcMotion) -> f32 {
    match p.wj {
        Some(WallJumpPhase::Prep { .. } | WallJumpPhase::After { .. }) => p.ctrl.trijump_turn_rate,
        Some(WallJumpPhase::Accel { .. }) => 0.0,
        None => p.ctrl.turn_rate,
    }
}

/// Applies the controller edges the game normally handles before stepping: boost toggle, Glide
/// Boost start, jump/wall-kick queue, and High Boost start.
///
/// Network prediction and the server both call this, so an input frame has one meaning on both
/// sides. The server's starvation repeat clears the one-shot fields (see
/// `InputFrame::repeat_safe`), so a repeated frame cannot toggle boost or fire a burst again.
pub fn apply_input_edges(p: &mut AcMotion, input: &InputFrame) {
    if input.boost_toggle {
        p.boost = !p.boost;
    }
    if p.qb.is_none() && p.wj.is_none() && p.kick.is_none() && p.blade.is_none() {
        let starting_glide = input.glide && p.boost && !p.airborne && p.en >= p.ctrl.glide_en_tick;
        if starting_glide && !p.glide {
            p.glide_phase = GlidePhase::Ground;
            p.glide_phase_time = 0.0;
            p.glide_lift_started = false;
        }
        p.glide |= starting_glide;
        p.jump_queued |= input.jump && !p.airborne;
        p.wall_jump_queued |= input.jump && p.airborne;
    }
    if input.quick_boost {
        start_quick_boost(p, p.glide);
    }
    if input.kick {
        p.start_kick();
    }
}

/// Rewrites a client frame into the server-validated form.
///
/// Aim is absolute: the per-tick turn is derived from `aim_yaw - yaw` and clamped to one tick of
/// the AC's effective turn rate, so a lost or malicious packet cannot turn the AC faster or leave
/// the aim permanently ahead of the body. `look` is client camera-spring input only and has no
/// server meaning.
pub fn validated_input(p: &AcMotion, frame: &InputFrame) -> InputFrame {
    let mut input = *frame;
    input.stick = input.stick.clamp_length_max(1.0);
    input.look = 0.0;
    let per_tick = (effective_turn_rate(p) / TICK_RATE).max(1e-4);
    let target = crate::input::wrap_angle(input.aim_yaw - p.yaw);
    input.turn = (target / per_tick).clamp(-1.0, 1.0);
    input.aim_pitch = if input.aim_pitch.is_finite() {
        input.aim_pitch
    } else {
        p.aim_pitch
    };
    input
}

/// Steps one client-authoritative input frame with the exact validation the server applies.
///
/// `aim_yaw_off` is rebuilt from the absolute aim after the body turn, so movement and weapons use
/// the same camera-relative facing the game's mouselook produces.
pub fn step_validated_with_props(
    p: &mut AcMotion,
    frame: &InputFrame,
    position: &mut Vec3,
    collision: &mut Collision,
    apply_to_prop: impl FnMut(u32, f32) -> bool,
) {
    let input = validated_input(p, frame);
    p.aim_pitch = input.aim_pitch;
    p.aim_yaw_off = crate::input::wrap_angle(frame.aim_yaw - p.yaw);
    apply_input_edges(p, &input);
    step_tick_with_props(p, &input, position, collision, apply_to_prop);
}

/// Read-only variant of [`step_validated_with_props`] for prediction tests and collision-free
/// replays.
pub fn step_validated(
    p: &mut AcMotion,
    frame: &InputFrame,
    position: &mut Vec3,
    collision: &Collision,
) {
    let input = validated_input(p, frame);
    p.aim_pitch = input.aim_pitch;
    p.aim_yaw_off = crate::input::wrap_angle(frame.aim_yaw - p.yaw);
    apply_input_edges(p, &input);
    step_tick(p, &input, position, collision);
}

/// One fixed 60 Hz movement tick (the public simulation entry point).
pub fn step_tick(p: &mut AcMotion, input: &InputFrame, position: &mut Vec3, collision: &Collision) {
    step(p, input, position, collision);
}

/// One fixed 60 Hz movement tick that also applies AC body collision damage to destructible
/// props through `apply_to_prop` (`prop_id`, raw damage -> broke now).
pub fn step_tick_with_props(
    p: &mut AcMotion,
    input: &InputFrame,
    position: &mut Vec3,
    collision: &mut Collision,
    apply_to_prop: impl FnMut(u32, f32) -> bool,
) {
    step_with_props(p, input, position, collision, apply_to_prop);
}

fn step_inner(
    p: &mut AcMotion,
    input: &InputFrame,
    position: &mut Vec3,
    collision: &Collision,
    _unused: impl FnMut(Vec3),
) {
    // Non-mutating path for unit tests that hold `&Collision`.
    let mut dummy = None::<&mut dyn FnMut(&AcMotion, Vec3, &mut Collision)>;
    step_core_ref(p, input, position, collision, &mut dummy);
}

fn step_core(
    p: &mut AcMotion,
    input: &InputFrame,
    position: &mut Vec3,
    collision: &mut Collision,
    mut on_move: Option<&mut dyn FnMut(&AcMotion, Vec3, &mut Collision)>,
) {
    step_move_integrate(p, input, position, collision);
    if let Some(f) = on_move.as_deref_mut() {
        f(p, *position, collision);
    }
    step_resolve_world(p, position, collision);
}

fn step_core_ref(
    p: &mut AcMotion,
    input: &InputFrame,
    position: &mut Vec3,
    collision: &Collision,
    _on_move: &mut Option<&mut dyn FnMut(&AcMotion, Vec3, &mut Collision)>,
) {
    step_move_integrate(p, input, position, collision);
    step_resolve_world(p, position, collision);
}

fn step_move_integrate(
    p: &mut AcMotion,
    input: &InputFrame,
    position: &mut Vec3,
    collision: &Collision,
) {
    let c = p.ctrl;
    // Glide Boost begins with a fixed ascent (`0x8281dd28` clamps the rise to block+0x0 above
    // the activation height, `object+0x2b4`): 5.0 m at ~6 m/s, so the AC lifts off even on flat
    // ground. `GlideState` (`0x82841b38`) runs the ground move (vtable slot `+0x34` -> movement
    // state 4) for `glide_ground_phase` = 0.5 s (`0x83713a94`) and then the air move
    // (`+0x38` -> state 5); the lift continues across both phases and caps at
    // `activation height + glide_lift_height`. Xenia probe `private/xenia/glide_cap.jsonl`:
    // state 4 for 12 samples (~0.5 s), then state 5 rising 25.97 -> 28.23 m.
    if !p.glide {
        p.glide_lifting = false;
        p.glide_lift_started = false;
        p.glide_lift_left = 0.0;
        p.glide_phase = GlidePhase::Ground;
        p.glide_phase_time = 0.0;
        p.glide_activation_y = 0.0;
    } else if !p.glide_lift_started {
        p.glide_lift_started = true;
        p.glide_lifting = true;
        p.glide_lift_left = c.glide_lift_height;
        p.glide_phase = GlidePhase::Ground;
        p.glide_phase_time = 0.0;
        p.glide_activation_y = position.y;
    }
    if p.glide {
        p.glide_phase_time += 1.0 / TICK_RATE;
        // Strictly greater in the 360 (`0x83713a94 < GlideState +0x24`); at 60 Hz the sample
        // lands on 0.5 s, so allow the tick rounding.
        if p.glide_phase == GlidePhase::Ground && p.glide_phase_time + 1e-4 >= c.glide_ground_phase
        {
            p.glide_phase = GlidePhase::Air;
            // The air move is movement state 5; the AC is now in the airborne mover even while
            // the start ascent is still spending its remaining metres.
            p.airborne = true;
        }
    }
    if p.qb_cooldown >= 1.0 {
        p.qb_cooldown = (p.qb_cooldown - 1.0).max(0.0);
    }
    if p.wj_lockout > 0.0 {
        p.wj_lockout = (p.wj_lockout - 1.0).max(0.0);
    }
    // `WallJumpState` Phases 0 and 2 turn via movement slot `+0xb4` (`0x8281fcd0`, `trijump_turn_rate`);
    // Phase 1 (`0x82841190`) does not turn.
    let turn_rate = match p.wj {
        Some(WallJumpPhase::Prep { .. } | WallJumpPhase::After { .. }) => c.trijump_turn_rate,
        Some(WallJumpPhase::Accel { .. }) => 0.0,
        None => c.turn_rate,
    };
    p.yaw += input.turn * turn_rate / TICK_RATE;
    // Mouselook: move relative to where the camera looks, not where the body faces.
    let facing = Quat::from_rotation_y(p.yaw + p.aim_yaw_off);
    let forward = facing * FORWARD;
    let right = forward.cross(Vec3::Y);
    let wish3 = right * input.stick.x + forward * input.stick.y;
    let wish = Vec2::new(wish3.x, wish3.z);
    let mut horizontal = Vec2::new(p.velocity.x, p.velocity.z);
    let speed = horizontal.length();
    let moving = wish != Vec2::ZERO;
    // A glide starts on the ground (`pilot`) but carries on over a drop. A glide quick boost
    // keeps the glide alive while `HighBoostState` runs, so the phase timer and start ascent
    // survive the burst and the glide resumes when it ends.
    if p.qb.is_none() || !p.glide {
        p.glide_qb = false;
    }
    p.glide &= p.boost
        && moving
        && (p.qb.is_none() || p.glide_qb)
        && p.wj.is_none()
        && p.kick.is_none()
        && p.blade.is_none();
    let brake = if speed >= c.brake_switch_tick {
        c.hispeed_brake_tick
    } else {
        c.lospeed_brake_tick
    };
    let drag = if p.boost {
        c.air_drag_boost_tick
    } else {
        c.air_drag_tick
    };

    // Boost Charge Kick (`0x82840ee0` -> `0x8283fdb0`) is the *held* Hi-Boost path (manual
    // 20301/20401 "â–¡ + HOLD"). A tapped Hi-Boost during a glide is the glide quick boost and
    // keeps the glide; holding it through `QbPhase::Phase2` starts the kick in `step_qb`.
    if input.kick {
        p.start_kick();
    }

    // Wall kick trigger (`0x82841520` -> `0x82841338`): airborne, not in jump take-off (state 3),
    // lockout (`action_ctrl +0x114`) < 1 tick, and `WallJumpHitChecker` (`0x828aa500`) hits a wall.
    if p.wall_jump_queued {
        p.wall_jump_queued = false;
        if p.airborne
            && p.wj.is_none()
            && p.qb.is_none()
            && p.kick.is_none()
            && p.blade.is_none()
            && p.takeoff <= 0.0
            && p.wj_lockout < 1.0
        {
            if let Some(wall_normal) = collision.wall_kick_contact(
                *position,
                c.wall_jump_sphere_radius,
                c.wall_jump_sphere_height,
                c.wall_jump_surface_angle_rad,
            ) {
                p.wj_lockout = c.trijump_lockout_frames;
                p.glide = false;
                let clip_dir = wall_jump_clip_dir(p.yaw, wall_normal);
                p.wj = Some(WallJumpPhase::Prep {
                    timer: 0.0,
                    wall_normal,
                    clip_dir,
                });
            }
        }
    }

    let qb_thrust = if p.kick.is_none() && p.blade.is_none() {
        step_qb(p, input, moving)
    } else {
        None
    };
    let kick_step = if qb_thrust.is_none() && p.blade.is_none() {
        step_kick(p, horizontal)
    } else {
        None
    };
    let blade_step = if qb_thrust.is_none() && kick_step.is_none() {
        step_blade(p, *position, horizontal, brake, drag)
    } else {
        None
    };
    let wj_thrust = if qb_thrust.is_none() && kick_step.is_none() && blade_step.is_none() {
        step_wall_jump(p, input, facing)
    } else {
        None
    };
    let mut qb_hopped = false;
    let mut wj_accelerated = false;
    let mut melee_skip_gravity = false;

    if let Some((kh, skip_g)) = kick_step {
        horizontal = kh;
        melee_skip_gravity = skip_g;
    } else if let Some((bh, skip_g)) = blade_step {
        horizontal = bh;
        melee_skip_gravity = skip_g;
    } else if let Some(qb) = qb_thrust {
        // `0x82824078` with `+0x1c0 == 1` (no normal move action): ground brake or air drag runs
        // before `0x82822ea0` adds the High Boost acceleration (`+0x118 * scale`) and clamps to
        // `0x82822af0` case 4 (`lerp(quick_boost_min_tick, quick_boost_max_tick, weight)`).
        if p.airborne {
            horizontal *= (1.0 - drag).clamp(0.0, 1.0);
        } else {
            horizontal = approach(horizontal, Vec2::ZERO, brake);
        }
        if qb.scale > 0.0 {
            let qb_max = c.quick_boost_min_tick
                + (c.quick_boost_max_tick - c.quick_boost_min_tick) * qb.weight;
            let accel = Vec2::new(qb.dir.x, qb.dir.z) * (c.quick_boost_acc_tick * qb.scale);
            let cur = horizontal.length();
            let next = horizontal + accel;
            horizontal = if cur <= qb_max {
                next.clamp_length_max(qb_max)
            } else {
                next.normalize_or(horizontal / cur) * cur
            };
        }
        if qb.hop && qb.scale > 0.0 {
            p.airborne = true;
            p.velocity.y =
                (p.velocity.y + c.quick_boost_v_acc_tick * qb.scale).min(c.qb_hop_max_tick);
            qb_hopped = true;
        }
    } else if let Some(wj) = wj_thrust {
        // `AcMove_wallJump` (`0x82824560`, movement slot `+0x44`):
        // - Drains `trijump_en_tick * wj.scale` (`+0x14c`/`+0x150` = `BST__HI_EN * 1.5`) via
        //   `0x828b2648` -> `0x82830910`; skips thrust when EN is insufficient.
        // - Vertical (`0x8281f408`: `dir.y >= 1.0`): no horizontal accel, `vy += (v_acc + h_acc) * scale`
        //   capped at `trijump_v_speed_tick`.
        // - Directional (`dir.y < 1.0`): horizontal accel `h_acc * scale` along `dir` capped at
        //   `trijump_h_speed_tick` (`0x82822af0` case 2), `vy += v_acc * 0.5 * scale` capped at
        //   `trijump_v_speed_tick * 0.8` (Xenia probe `private/xenia/wall_kick.jsonl`).
        if wj.scale > 0.0 && p.try_drain_en(c.trijump_en_tick * wj.scale) {
            if wj.dir.y >= 1.0 {
                horizontal *= (1.0 - drag).clamp(0.0, 1.0);
                p.velocity.y = (p.velocity.y
                    + (c.trijump_v_acc_tick + c.trijump_h_acc_tick) * wj.scale)
                    .min(c.trijump_v_speed_tick);
            } else {
                let accel = Vec2::new(wj.dir.x, wj.dir.z) * (c.trijump_h_acc_tick * wj.scale);
                horizontal = integrate(
                    horizontal,
                    accel,
                    c.trijump_h_speed_tick,
                    c.over_max_decel_tick,
                );
                p.velocity.y = (p.velocity.y
                    + c.trijump_v_acc_tick * WALL_JUMP_DIR_V_ACC_SCALE * wj.scale)
                    .min(c.trijump_v_speed_tick * WALL_JUMP_DIR_V_CAP_SCALE);
            }
            wj_accelerated = true;
        }
    } else if p.wj.is_some() {
        // `WallJumpState` Phase 0 (Prep) or Phase 2 (After): coast with air drag.
        horizontal *= (1.0 - drag).clamp(0.0, 1.0);
    } else if !p.airborne || p.glide {
        // The glide action returns 0 (and the glide ends) when the stick heading leaves the
        // turn cone around the current velocity, before the EN check (`0x8281ea30` /
        // `0x8281e898`).
        if p.glide && !glide_heading_ok(wish, horizontal, forward, c.glide_turn_cone) {
            p.glide = false;
        }
        // Glide Boost (`0x8281ea30` / `0x8281e9d4`) drains `glide_en_tick` (`BST__HI_EN / 60`)
        // each tick and ends immediately when `en < glide_en_tick` (`private/xenia/en_drain2.jsonl`).
        if p.glide && !p.try_drain_en(c.glide_en_tick) {
            p.glide = false;
        }
        if !p.glide && p.boost && moving {
            let _ = p.try_drain_en(c.boost_ep_tick);
        }
        // After a high boost ends, speed sits above the boost/walk max; `integrate` keeps steering
        // with the stick while speed decays by `over_max_decel_tick` per tick. A glide is the
        // special path `0x8281e408`: thrust along the stick, speed held above glide max.
        let (max, accel) = if p.boost {
            (c.boost_max_tick, c.boost_acc_tick)
        } else {
            (c.walk_max_tick, c.walk_acc_tick)
        };
        horizontal = if p.glide {
            glide_thrust(horizontal, wish * c.glide_acc_tick, c.glide_max_tick)
        } else if moving && !p.airborne {
            integrate(horizontal, wish * accel, max, c.over_max_decel_tick)
        } else if !p.airborne {
            approach(horizontal, Vec2::ZERO, brake)
        } else if p.boost && moving {
            integrate(
                horizontal,
                wish * c.boost_acc_tick,
                c.boost_max_tick,
                c.over_max_decel_tick,
            )
        } else {
            horizontal *= (1.0 - drag).clamp(0.0, 1.0);
            horizontal
        };
    } else if p.takeoff > 0.0 {
        let _ = p.try_drain_en(c.boost_ep_tick);
        horizontal += wish * c.jump_h_acc_tick;
    } else if p.boost && moving {
        let _ = p.try_drain_en(c.boost_ep_tick);
        horizontal = integrate(
            horizontal,
            wish * c.boost_acc_tick,
            c.boost_max_tick,
            c.over_max_decel_tick,
        );
    } else {
        horizontal *= (1.0 - drag).clamp(0.0, 1.0);
    }
    if p.jump_queued
        && !p.airborne
        && p.qb.is_none()
        && p.wj.is_none()
        && p.kick.is_none()
        && p.blade.is_none()
    {
        (p.airborne, p.takeoff, p.boost, p.glide) = (true, c.jump_frames, true, false);
        // `0x8283d4b8`: entering Jump (state 3) sets `action_ctrl +0x114` to
        // `NewAcBehavior +0x4ae` (10 ticks), preventing immediate wall kick off the ground.
        p.wj_lockout = c.wall_jump_post_jump_lockout;
    }
    p.jump_queued = false;

    // Glide Boost opens with the fixed start ascent of `glide_lift_height` (object+0x8c = 5.0 m,
    // clamped by 0x8281dd28 to `block+0x0 + block+0x2c` = activation height + 5.0 m) at
    // `glide_rise_tick`. It spans the ground and air phases: `GlideState` switches to the air
    // move (state 5) at 0.5 s while the ascent is still running, and `0x8281dd28` only stops it
    // at the activation-height ceiling. Xenia probe private/xenia/glide_cap.jsonl: 23.230 ->
    // 28.230 m, state 4 for the first 0.5 s then state 5 for the rest.
    if p.glide && p.glide_lifting {
        let ceiling = p.glide_activation_y + c.glide_lift_height;
        let left = (ceiling - position.y).max(0.0);
        let step = c.glide_rise_tick.min(left);
        p.glide_lift_left = (left - step).max(0.0);
        p.velocity.y = step;
        if left <= 1e-4 {
            p.glide_lifting = false;
            p.glide_lift_left = 0.0;
            p.velocity.y = 0.0;
        }
    } else if p.airborne && !qb_hopped && !wj_accelerated && !melee_skip_gravity {
        if p.takeoff > 0.0 {
            p.velocity.y += c.jump_accel_tick * p.takeoff.min(1.0);
            p.takeoff = (p.takeoff - 1.0).max(0.0);
        } else if p.glide {
            // The vertical update 0x82820168 skips the gravity loop (0x8281f960) in the glide
            // states 4/5 (0x8281f3d0 at 0x828201b4): after the start ascent the glide holds its
            // height (a glide that leaves a ledge does not rise).
            p.velocity.y = 0.0;
        } else {
            // Boost gravity (movement +0x284) only while boost mode is on and the AC falls; it
            // rises under full gravity (Xenia probe private/xenia/vertical.txt).
            p.velocity.y -= if p.boost && p.velocity.y < 0.0 {
                c.boost_gravity_tick
            } else {
                c.gravity_tick
            };
        }
        p.velocity.y = p.velocity.y.max(-c.fall_max_tick);
    }
    if p.ow_dash_ticks > 0.0 {
        p.ow_dash_ticks = (p.ow_dash_ticks - 1.0).max(0.0);
        let dir2 = Vec2::new(p.ow_dash_dir.x, p.ow_dash_dir.z).normalize_or_zero();
        let mult = acvd_data::generated::tuning::NEW_AC_BEHAVIOR
            .ow_attack_accel_mult
            .max(1.0);
        let accel = c.ow_attack_accel_tick.max(c.quick_boost_acc_tick) * mult;
        let max_spd = c.ow_attack_max_tick.max(c.quick_boost_max_tick);
        if dir2 != Vec2::ZERO && max_spd > 0.0 {
            horizontal = integrate(horizontal, dir2 * accel, max_spd, c.over_max_decel_tick);
        }
    }
    p.velocity = Vec3::new(horizontal.x, p.velocity.y, horizontal.y);
    *position += p.velocity;
}

fn step_resolve_world(p: &mut AcMotion, position: &mut Vec3, collision: &Collision) {
    let before_y = position.y - p.velocity.y;
    let horizontal = Vec2::new(p.velocity.x, p.velocity.z);
    // Walls: the body spheres push the AC out sideways and lose the velocity into the wall,
    // so it slides along it (AC TEST parapet, `private/xenia/wall_walk3.jsonl`).
    for n in collision.push_out(position) {
        let into = p.velocity.x * n.x + p.velocity.z * n.y;
        if into < 0.0 {
            p.velocity.x -= into * n.x;
            p.velocity.z -= into * n.y;
        }
    }
    // Snap puts y on the triangle, so a grounded ray has to start above it. The lift is this
    // tick's travel (slopes) or 5 cm when still â€” a hit farther down than that is a drop.
    // Airborne, the ray starts at last tick's height so a fast fall cannot step past a floor;
    // a walkable slope rising into the flight path is also above the feet, so the probe has to
    // clear `travel x tan(55)` there too or a glide boost clips through an incline.
    let lift = if p.airborne {
        (before_y - position.y).max(0.0)
    } else {
        horizontal.length().max(0.05)
    };
    let probe_up = if p.airborne {
        lift.max(horizontal.length() * crate::collision::WALKABLE_SLOPE_TAN)
    } else {
        (lift * crate::collision::WALKABLE_SLOPE_TAN).max(crate::collision::STEP_UP)
    };
    let ground = collision.ground_below_with_surface(Vec3::new(
        position.x,
        position.y + probe_up,
        position.z,
    ));
    if p.airborne {
        // Ground that rose through the feet is a hit even while still rising (a glide boost
        // does not rise after its start ascent, but the slope can).
        if let Some((g, surf)) = ground
            .filter(|&(g, _)| position.y <= g && (p.velocity.y <= 0.0 || position.y < g - 1e-3))
        {
            let impact_kmh = p.velocity.y.abs() * TICK_TO_KMH;
            p.landing_impact_kmh = p.landing_impact_kmh.max(impact_kmh);
            p.ground_surface = surf;
            p.landings += 1;
            (position.y, p.velocity.y, p.airborne, p.landed, p.wj) = (g, 0.0, false, true, None);
        }
    } else if p.glide_lifting {
        // Glide Boost start ascent (`glide_lift_height`): the ground snap must not undo the lift
        // while the movement state is still the ground glide; `GlideState` switches the mover to
        // the air phase (state 5) after 0.5 s while the ascent continues to its ceiling. Rising
        // terrain still stops it, or the ascent carries the AC through the slope.
        if let Some((g, surf)) = ground.filter(|&(g, _)| g > position.y + 1e-3) {
            (position.y, p.velocity.y, p.airborne, p.landed) = (g, 0.0, false, true);
            p.ground_surface = surf;
            p.glide_lifting = false;
            p.glide_lift_left = 0.0;
        }
    } else {
        match ground.filter(|&(g, _)| g >= position.y - lift) {
            Some((g, surf)) => {
                position.y = g;
                p.ground_surface = surf;
            }
            None => p.airborne = true,
        }
    }
    // Boosters on make the AC skim the water surface instead of dropping in. Water is a separate
    // collision class (the water probe `0x829e7be0` masks `0x200002`; the AC body sphere filters
    // `0x100022` / `0x4100022` at `0x82863ba4` / `0x82863bcc` do not include its `0x200000` class),
    // so an airborne boost has to clamp its own fall to the surface. The clamp also recovers a
    // tick that sank a little while moving (up to the wading depth), otherwise a solo crossing
    // check lets the AC walk itself under the surface over several ticks; deeper than that is a
    // real drop and is handled by the water return in `pilot`. Boosters off keeps falling through.
    if p.airborne && (p.boost || p.glide) && p.velocity.y <= 0.0 {
        if let Some(wy) = collision.ray_down(
            *position + Vec3::Y * RAY_HALF,
            position.y - RAY_HALF,
            Layer::Water,
        ) {
            let sank =
                position.y <= wy && (before_y >= wy || wy - position.y <= WATER_RETURN_DEPTH);
            if sank {
                position.y = wy;
                p.velocity.y = 0.0;
            }
        }
    }
    step_en(p);
}

/// Submersion (m) the AC tolerates before a water return: its feet may sit this far under the
/// surface while it wades, glides or hovers over water (`--plane --water 2` leaves the AC 2 m
/// under). Anchored to the lower body sphere's centre height above the feet
/// (`collision::BODY[0].height`, built from tuning record 0 `0x82330c38` `+0x234`; Xenia probe
/// `private/xenia/wall_walk4.txt`), i.e. the water returns the AC once it reaches its torso.
const WATER_RETURN_DEPTH: f32 = crate::collision::BODY[0].height;

/// Checks if the AC at `pos` has sunk deep into water (`env::Scene.water_height` from
/// `m####_env.msb` sub-block 0 `+0x08`, 360 `0x827ffa80`, or `--water <y>`). Feet within
/// [`WATER_RETURN_DEPTH`] of the surface are shallow water the AC can wade, glide and hover
/// over, and floating above the surface never returns. Deep water teleports the AC to the
/// nearest `POINT_WATER_RETURN` (`50`) point, zeroes velocity, and clears active dash/air/melee
/// states. Returns `true` if a water-return respawn occurred.
pub fn check_water_return(
    p: &mut AcMotion,
    pos: &mut Vec3,
    water_return: &crate::map::WaterReturnPoints,
    fallback: crate::map::Start,
    collision: &Collision,
) -> bool {
    let Some(wy) = water_return.water_height else {
        return false;
    };
    // Kind-50 points are the only water-return destinations. Maps whose env has a sea-level
    // water plane but no `POINT_WATER_RETURN` (m3750 Agria/M42: water_height 127, playable
    // interior at y -1) never return; the old height-only check respawned the AC every frame.
    if water_return.points.is_empty() {
        return false;
    }
    if pos.y + WATER_RETURN_DEPTH >= wy {
        return false;
    }
    p.water_splash_at = Some(Vec3::new(pos.x, wy, pos.z));
    let dest = water_return.nearest(*pos, fallback);
    let y = collision
        .ground_below(dest.position + Vec3::Y * 10.0)
        .unwrap_or(dest.position.y);
    *pos = Vec3::new(dest.position.x, y, dest.position.z);
    p.prev_position = *pos;
    p.yaw = dest.yaw;
    p.velocity = Vec3::ZERO;
    p.airborne = false;
    p.glide = false;
    p.qb = None;
    p.wj = None;
    p.kick = None;
    p.blade = None;
    p.aim_yaw_off = 0.0;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use acvd_data::find;

    /// A high boost is not a locked impulse: the stick keeps steering the velocity while the
    /// speed above the boost max falls back, in the air and on the ground.
    #[test]
    fn high_boost_steers_and_settles() {
        use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;
        let ctrl = AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .filter(|r| AcCtrlParam::buildable(&r.data))
            .map(|r| AcCtrlParam::calculate(&r.data))
            .find(|c| c.boost_acc_tick > 0.0 && c.over_max_decel_tick > 0.0)
            .expect("a preset with boost acceleration");
        for airborne in [false, true] {
            let mut p = AcMotion::new(ctrl);
            p.boost = true;
            p.airborne = airborne;
            let collision = if airborne {
                Collision::default()
            } else {
                Collision::with_hits(
                    vec![crate::collision::Plane {
                        y: 0.0,
                        layer: Layer::Ground,
                    }],
                    Vec::new(),
                )
            };
            let mut position = Vec3::new(0.0, if airborne { 500.0 } else { 0.0 }, 0.0);
            fire_quick_boost(&mut p, Vec2::Y, 0.0);
            let right = InputFrame {
                stick: Vec2::X,
                ..InputFrame::default()
            };
            let mut peak = 0.0f32;
            for _ in 0..30 {
                step(&mut p, &right, &mut position, &collision);
                peak = peak.max(Vec2::new(p.velocity.x, p.velocity.z).length());
            }
            assert!(
                peak > ctrl.boost_max_tick,
                "air={airborne}: burst peak {peak} should exceed boost max {}",
                ctrl.boost_max_tick
            );
            assert!(
                !p.in_quick_boost(),
                "air={airborne}: HighBoostState should finish well before 30 ticks"
            );
            let horizontal = Vec2::new(p.velocity.x, p.velocity.z);
            let sideways = horizontal.dot(Vec2::new(p.yaw.cos(), -p.yaw.sin())).abs();
            assert!(sideways > 0.2 * horizontal.length(), "air={airborne}: stuck on the QB heading {horizontal:?}, boost acc {} max {} decel {} qb {}", ctrl.boost_acc_tick, ctrl.boost_max_tick, ctrl.over_max_decel_tick, ctrl.quick_boost_max_tick);
            for _ in 0..400 {
                step(&mut p, &right, &mut position, &collision);
            }
            let settled = Vec2::new(p.velocity.x, p.velocity.z).length();
            assert!(
                (settled - ctrl.boost_max_tick).abs() < 1e-4,
                "air={airborne}: speed {settled} should settle at boost max {}",
                ctrl.boost_max_tick
            );
        }
    }

    #[test]
    fn high_boost_charge_sweet_spot_and_refire() {
        use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;
        let ctrl = AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .filter(|r| AcCtrlParam::buildable(&r.data))
            .map(|r| AcCtrlParam::calculate(&r.data))
            .find(|c| {
                c.tank == 0.0
                    && c.qb_charge_max_frames > 0.0
                    && c.qb_min_speed_rate > 0.0
                    && c.qb_min_speed_rate < 1.0
            })
            .expect("a non-tank preset with Boost Charge window");

        // 1. Uncharged tap on ground with boost mode OFF stays boost=false, hops, and settles to walk_max_tick.
        let ground = Collision::with_hits(
            vec![crate::collision::Plane {
                y: 0.0,
                layer: Layer::Ground,
            }],
            Vec::new(),
        );
        let mut p = AcMotion::new(ctrl);
        let mut pos = Vec3::ZERO;
        assert!(start_quick_boost(&mut p, false));
        assert!(!start_quick_boost(&mut p, false), "blocked while active");
        let tap_release = InputFrame {
            stick: Vec2::Y,
            qb_held: false,
            ..InputFrame::default()
        };
        step(&mut p, &tap_release, &mut pos, &ground);
        assert!(!p.second_stage_qb);
        let expected_tap_max = ctrl.quick_boost_min_tick
            + (ctrl.quick_boost_max_tick - ctrl.quick_boost_min_tick) * (ctrl.qb_base_rate1 * 0.01);
        let mut tap_peak = 0.0f32;
        let mut hopped = false;
        let mut burst_ticks = 0u32;
        while p.in_quick_boost() {
            step(&mut p, &tap_release, &mut pos, &ground);
            tap_peak = tap_peak.max(Vec2::new(p.velocity.x, p.velocity.z).length());
            hopped |= p.velocity.y > 0.0;
            burst_ticks += 1;
            assert!(burst_ticks < 30);
        }
        assert!(
            hopped,
            "grounded High Boost should add a small vertical hop"
        );
        assert!(
            (tap_peak - expected_tap_max).abs() < 1e-3,
            "tap peak {tap_peak} vs expected {expected_tap_max}"
        );
        assert!(!p.boost, "High Boost must not force boost mode on");
        // Immediately re-fireable once HighBoostState ends (well under 20 ticks).
        assert!(burst_ticks <= 16, "burst took {burst_ticks} ticks");
        assert!(
            start_quick_boost(&mut p, false),
            "re-fire unlocked right after burst"
        );
        p.qb = None;
        for _ in 0..300 {
            step(&mut p, &tap_release, &mut pos, &ground);
        }
        let walk_settled = Vec2::new(p.velocity.x, p.velocity.z).length();
        assert!(
            (walk_settled - ctrl.walk_max_tick).abs() < 1e-4,
            "boost-off QB settled at {walk_settled}, expected walk max {}",
            ctrl.walk_max_tick
        );

        // 2. Sweet-spot hold reaches Second-Stage High Boost (`w1 = w2 = 1.0`, full `quick_boost_max_tick`).
        let mut p2 = AcMotion::new(ctrl);
        p2.boost = true;
        p2.airborne = true;
        let mut pos2 = Vec3::new(0.0, 500.0, 0.0);
        assert!(start_quick_boost(&mut p2, false));
        let sweet_ticks = ctrl.qb_sweet_frames.round() as u32;
        let hold = InputFrame {
            stick: Vec2::Y,
            qb_held: true,
            ..InputFrame::default()
        };
        for _ in 0..sweet_ticks.saturating_sub(1) {
            step(&mut p2, &hold, &mut pos2, &Collision::default());
        }
        step(&mut p2, &tap_release, &mut pos2, &Collision::default());
        assert!(
            p2.second_stage_qb,
            "sweet-spot release must trigger Second-Stage QB"
        );
        let mut sweet_peak = 0.0f32;
        while p2.in_quick_boost() {
            step(&mut p2, &tap_release, &mut pos2, &Collision::default());
            sweet_peak = sweet_peak.max(Vec2::new(p2.velocity.x, p2.velocity.z).length());
        }
        assert!(
            (sweet_peak - ctrl.quick_boost_max_tick).abs() < 1e-4,
            "sweet-spot peak {sweet_peak} vs {}",
            ctrl.quick_boost_max_tick
        );
    }

    #[test]
    fn wall_jump_direction_vertical_away_and_oblique() {
        let facing = Quat::IDENTITY; // forward = -Z, right = +X
        let wall_n = Vec3::X; // wall on left, outward normal +X
        let vert_rad = 45.0_f32.to_radians();
        let glance_rad = 85.0_f32.to_radians();

        // Neutral stick -> vertical (0, 1, 0).
        assert_eq!(
            wall_jump_dir(Vec2::ZERO, facing, wall_n, vert_rad, glance_rad),
            Vec3::Y
        );
        // Straight into wall (-X, dot = -1 < -cos(45°)) -> vertical (0, 1, 0).
        assert_eq!(
            wall_jump_dir(Vec2::new(-1.0, 0.0), facing, wall_n, vert_rad, glance_rad),
            Vec3::Y
        );
        // Away from wall (+X, dot = +1 > 0) -> wish (+X).
        let away = wall_jump_dir(Vec2::new(1.0, 0.0), facing, wall_n, vert_rad, glance_rad);
        assert!((away - Vec3::X).length() < 1e-5, "{away}");
        // Oblique into wall (-0.3, +0.95): -cos(45°) <= dot <= 0 -> rotated wall normal.
        let oblique = wall_jump_dir(Vec2::new(-0.3, 0.95), facing, wall_n, vert_rad, glance_rad);
        assert!(
            oblique.y.abs() < 1e-5 && oblique.x > 0.0 && oblique.z < 0.0,
            "{oblique}"
        );
        assert!(
            (oblique.dot(wall_n) - glance_rad.cos()).abs() < 1e-4,
            "expected cos(85°) = {}, got {}",
            glance_rad.cos(),
            oblique.dot(wall_n)
        );
    }

    #[test]
    fn wall_jump_phases_lockout_and_velocity_caps() {
        use crate::collision::{Hit, Layer, Plane};
        let design = &acvd_data::find(
            acvd_data::generated::ac_unit::PARAM_ACASSEMBLYDRAWING_BIN,
            5001,
        )
        .expect("design 5001")
        .data;
        let ctrl = AcCtrlParam::calculate(design);
        // Wall at x = 0, floor at y = 0.
        let world = Collision::with_hits(
            vec![Plane {
                y: 0.0,
                layer: Layer::Ground,
            }],
            vec![
                Hit::solid(
                    Vec3::new(0.0, 0.0, -50.0),
                    Vec3::new(0.0, 40.0, -50.0),
                    Vec3::new(0.0, 0.0, 50.0),
                ),
                Hit::solid(
                    Vec3::new(0.0, 40.0, -50.0),
                    Vec3::new(0.0, 40.0, 50.0),
                    Vec3::new(0.0, 0.0, 50.0),
                ),
            ],
        );

        // 1. Post-jump 10-frame lockout blocks immediate wall kick, then directional kick fires.
        let mut p = AcMotion::new(ctrl);
        let mut pos = Vec3::new(5.5, 0.0, 0.0);
        p.jump_queued = true;
        step(&mut p, &InputFrame::default(), &mut pos, &world);
        assert!(p.airborne);
        assert_eq!(p.wj_lockout, ctrl.wall_jump_post_jump_lockout);
        p.wall_jump_queued = true;
        step(&mut p, &InputFrame::default(), &mut pos, &world);
        assert!(p.wj.is_none(), "post-jump lockout must block wall kick");
        for _ in 0..12 {
            step(&mut p, &InputFrame::default(), &mut pos, &world);
        }
        assert!(p.wj_lockout < 1.0);
        // Trigger directional wall kick away from wall (+X stick).
        p.velocity.y = -0.3;
        p.wall_jump_queued = true;
        let away = InputFrame {
            stick: Vec2::new(1.0, 0.0),
            ..InputFrame::default()
        };
        step(&mut p, &away, &mut pos, &world);
        assert!(matches!(p.wj, Some(WallJumpPhase::Prep { .. })));

        let mut peak_h = 0.0f32;
        let mut peak_v = 0.0f32;
        for _ in 0..40 {
            step(&mut p, &away, &mut pos, &world);
            peak_h = peak_h.max(Vec2::new(p.velocity.x, p.velocity.z).length());
            peak_v = peak_v.max(p.velocity.y);
            if p.wj.is_none() {
                break;
            }
        }
        assert_eq!(p.wall_jumps, 1);
        assert!(
            (peak_h - ctrl.trijump_h_speed_tick).abs() < 1e-3,
            "dir wall kick h peak {peak_h} vs {}",
            ctrl.trijump_h_speed_tick
        );
        assert!(
            (peak_v - ctrl.trijump_v_speed_tick * WALL_JUMP_DIR_V_CAP_SCALE).abs() < 1e-3,
            "dir wall kick v peak {peak_v} vs {}",
            ctrl.trijump_v_speed_tick * WALL_JUMP_DIR_V_CAP_SCALE
        );

        // 2. Vertical wall kick (neutral stick) reaches full `trijump_v_speed_tick` with no h accel.
        let mut p2 = AcMotion::new(ctrl);
        p2.airborne = true;
        p2.boost = true;
        let mut pos2 = Vec3::new(5.5, 5.0, 0.0);
        p2.wall_jump_queued = true;
        let neutral = InputFrame::default();
        step(&mut p2, &neutral, &mut pos2, &world);
        let mut vert_v = 0.0f32;
        let mut vert_h = 0.0f32;
        for _ in 0..40 {
            step(&mut p2, &neutral, &mut pos2, &world);
            vert_v = vert_v.max(p2.velocity.y);
            vert_h = vert_h.max(Vec2::new(p2.velocity.x, p2.velocity.z).length());
            if p2.wj.is_none() {
                break;
            }
        }
        assert_eq!(p2.wall_jumps, 1);
        assert!(
            (vert_v - ctrl.trijump_v_speed_tick).abs() < 1e-3,
            "vertical wall kick v peak {vert_v} vs {}",
            ctrl.trijump_v_speed_tick
        );
        assert!(
            vert_h < 1e-4,
            "vertical wall kick should add no horizontal speed, got {vert_h}"
        );
    }

    #[test]
    fn over_max_speed_decays_to_max() {
        let (walk, boost, accel, decel) = (52.0 / TICK_TO_KMH, 123.0 / TICK_TO_KMH, 0.02, 0.01);
        let mut v = Vec2::new(0.0, boost);
        let steered = integrate(v, Vec2::new(accel, 0.0), walk, decel);
        assert!((steered.length() - boost * (1.0 - decel)).abs() < 1e-6 && steered.x > 0.0);
        let mut ticks = 0;
        while v.length() > walk {
            v = integrate(v, Vec2::new(0.0, accel), walk, decel);
            ticks += 1;
        }
        assert_eq!(v.length(), walk);
        assert!((80..=90).contains(&ticks), "{ticks} ticks");
        assert_eq!(
            integrate(
                Vec2::new(0.0, walk * 0.5),
                Vec2::new(0.0, walk),
                walk,
                decel
            )
            .length(),
            walk
        );
    }

    #[test]
    fn travel_eighth_is_clockwise_from_forward() {
        use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;
        let ctrl = AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find_map(|r| AcCtrlParam::buildable(&r.data).then(|| AcCtrlParam::calculate(&r.data)))
            .expect("a buildable preset");
        let mut p = AcMotion::new(ctrl);
        p.velocity = FORWARD;
        assert_eq!(travel_eighth(&p, Vec2::ZERO), 0);
        p.velocity = Vec3::NEG_X;
        assert_eq!(travel_eighth(&p, Vec2::ZERO), 6);
        p.velocity = Vec3::X;
        assert_eq!(travel_eighth(&p, Vec2::ZERO), 2);
        p.velocity = Vec3::ZERO;
        assert_eq!(travel_eighth(&p, Vec2::Y), 0);
        assert_eq!(travel_eighth(&p, Vec2::new(1.0, 1.0)), 1);
    }

    #[test]
    fn en_high_boost_drains_and_blocks_when_low() {
        use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;
        let ctrl = AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 5001)
            .map(|r| AcCtrlParam::calculate(&r.data))
            .expect("design 5001");
        assert!(ctrl.en_capacity > 0.0 && ctrl.en_output > ctrl.passive_en_drain);
        assert!((ctrl.qb_en_drain - ctrl.hi_en * 0.7).abs() < 1e-3);

        let mut p = AcMotion::new(ctrl);
        assert_eq!(p.en, p.en_max);
        assert!(start_quick_boost(&mut p, false));
        assert!((p.en - (p.en_max - ctrl.qb_en_drain)).abs() < 1e-2);

        // Immediate recharge with no delay, same on ground and in air.
        let before_rec = p.en;
        let net_tick = ctrl.en_output_tick - ctrl.passive_en_drain_tick;
        step_en(&mut p);
        assert!((p.en - (before_rec + net_tick)).abs() < 1e-3);

        // Scan Mode (`AC + 0xdb0 != 0`, `0x828bef18` -> `0x82868268`) uses reduced passive EN drain
        // (`0x8286a558`), so EN recharges strictly faster per tick.
        p.scan_mode = true;
        let before_scan = p.en;
        let scan_net_tick = ctrl.en_output_tick - ctrl.scan_passive_en_drain_tick;
        assert!(
            scan_net_tick > net_tick,
            "Scan Mode recharges EN faster than Combat Mode"
        );
        step_en(&mut p);
        assert!((p.en - (before_scan + scan_net_tick)).abs() < 1e-3);
        p.scan_mode = false;

        // High Boost is blocked without draining when EN < qb_en_drain.
        p.qb = None;
        p.qb_cooldown = 0.0;
        p.en = ctrl.qb_en_drain - 10.0;
        let low_en = p.en;
        assert!(!start_quick_boost(&mut p, false));
        assert_eq!(p.en, low_en);
    }

    #[test]
    fn glide_boost_starts_with_a_fixed_ascent() {
        // 0x8281dd28 clamps the start rise to block+0x0 + block+0x2c, the activation height
        // (object+0x2b4) plus the 5.0 m lift (object+0x8c). Xenia probe
        // private/xenia/glide_cap.jsonl: 23.230 -> 28.230 over 374 ms (0.1 m/tick), then flat.
        let ctrl = acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 5001)
            .map(|r| AcCtrlParam::calculate(&r.data))
            .expect("design 5001");
        assert!((ctrl.glide_lift_height - 5.0).abs() < 1e-6);
        assert!((ctrl.glide_rise_tick - 0.1).abs() < 1e-6);

        let ground = Collision::with_hits(
            vec![crate::collision::Plane {
                y: 0.0,
                layer: Layer::Ground,
            }],
            Vec::new(),
        );
        let mut p = AcMotion::new(ctrl);
        p.boost = true;
        p.glide = true;
        p.velocity = Vec3::NEG_Z * 0.4;
        let mut pos = Vec3::ZERO;
        let input = InputFrame {
            stick: Vec2::Y,
            ..InputFrame::default()
        };
        // Activation lifts off the ground immediately, even on flat terrain, while the ground
        // glide clip still drives the pose.
        step(&mut p, &input, &mut pos, &ground);
        assert_eq!(p.glide_phase, GlidePhase::Ground);
        assert!(!p.airborne, "the lift starts in the ground glide phase");
        assert!(pos.y > 0.0, "the first tick of the glide already rises");
        // 5.0 m at 0.1 m/tick: about 50 ticks, then the fixed amount is spent.
        for _ in 0..80 {
            step(&mut p, &input, &mut pos, &ground);
        }
        assert!(
            p.airborne,
            "the ground phase hands over to the air mover at 0.5 s"
        );
        assert!(
            (pos.y - 5.0).abs() < 0.11,
            "the start ascent is a fixed 5.0 m, got {}",
            pos.y
        );
        assert_eq!(p.velocity.y, 0.0, "after the ascent the glide holds height");
        // Gliding off a ledge afterwards must not add any further ascent.
        let void = Collision::default();
        for _ in 0..40 {
            step(&mut p, &input, &mut pos, &void);
        }
        assert!(
            (pos.y - 5.0).abs() < 0.11,
            "stepping off a ledge must not ascend, got {}",
            pos.y
        );
        assert!(p.glide, "the glide itself keeps running");
    }

    #[test]
    fn glide_hands_over_to_the_air_phase_after_half_a_second() {
        // GlideState update 0x82841b38: the ground move (slot +0x34 -> state 4) runs while
        // the timer +0x24 is <= fRam83713a94 = 0.5 s, then the air move (slot +0x38 ->
        // 0x8281f8c0, state 5). The 5.0 m ascent continues into the air phase (0x8281dd28
        // only caps it at activation height + block+0x0).
        let ctrl = acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 5001)
            .map(|r| AcCtrlParam::calculate(&r.data))
            .expect("design 5001");
        assert!((ctrl.glide_ground_phase - 0.5).abs() < 1e-6);
        let ground = Collision::with_hits(
            vec![crate::collision::Plane {
                y: 0.0,
                layer: Layer::Ground,
            }],
            Vec::new(),
        );
        let mut p = AcMotion::new(ctrl);
        p.boost = true;
        p.glide = true;
        p.velocity = Vec3::NEG_Z * ctrl.glide_max_tick;
        p.en = p.en_max;
        let mut pos = Vec3::ZERO;
        let input = InputFrame {
            stick: Vec2::Y,
            ..InputFrame::default()
        };
        for _ in 0..29 {
            step(&mut p, &input, &mut pos, &ground);
        }
        assert!(p.glide);
        assert_eq!(p.glide_phase, GlidePhase::Ground);
        assert!(!p.airborne, "state 4 still drives the mover before 0.5 s");
        assert!(p.glide_lifting, "the start ascent is still running");
        step(&mut p, &input, &mut pos, &ground);
        assert_eq!(p.glide_phase, GlidePhase::Air);
        assert!(p.airborne, "state 5 is the airborne mover");
        assert!(
            p.glide_lifting,
            "the ascent continues after the phase switch"
        );
        assert!(
            pos.y < ctrl.glide_lift_height,
            "the phase switch happens before the 5.0 m are spent, got {}",
            pos.y
        );
    }

    #[test]
    fn glide_quick_boost_keeps_the_glide() {
        // A tapped Hi-Boost during a glide is the glide quick boost: the 360 glide top-level
        // action (vtable 0x82092318, update 0x8283f240) stays active while HighBoostState runs,
        // so the glide does not exit and resumes after the burst. Holding the button through
        // Phase 2 is the separate Boost Charge Kick path.
        let ctrl = acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 5001)
            .map(|r| AcCtrlParam::calculate(&r.data))
            .expect("design 5001");
        let ground = Collision::with_hits(
            vec![crate::collision::Plane {
                y: 0.0,
                layer: Layer::Ground,
            }],
            Vec::new(),
        );
        let mut p = AcMotion::new(ctrl);
        p.boost = true;
        p.glide = true;
        p.en = p.en_max;
        p.velocity = Vec3::NEG_Z * ctrl.glide_max_tick;
        let mut pos = Vec3::ZERO;
        let input = InputFrame {
            stick: Vec2::Y,
            ..InputFrame::default()
        };
        for _ in 0..10 {
            step(&mut p, &input, &mut pos, &ground);
        }
        assert!(p.glide);

        assert!(start_quick_boost(&mut p, true));
        assert!(p.glide, "the tap must not clear the glide");
        assert!(p.glide_qb);

        // Two held ticks then a release: the tap fires High Boost.
        let held = InputFrame {
            stick: Vec2::Y,
            qb_held: true,
            ..InputFrame::default()
        };
        step(&mut p, &held, &mut pos, &ground);
        step(&mut p, &held, &mut pos, &ground);
        step(&mut p, &input, &mut pos, &ground);
        assert!(p.qb.is_some(), "the tap fires High Boost Phase 1");
        assert!(p.glide);
        assert!(p.glide_qb);

        let mut saw_glide_qb = false;
        for _ in 0..300 {
            saw_glide_qb |= p.glide_qb;
            step(&mut p, &input, &mut pos, &ground);
            assert!(p.glide, "the glide survives the whole burst");
        }
        assert!(
            saw_glide_qb,
            "the glide quick boost flag held through the burst"
        );
        assert!(p.qb.is_none(), "the burst ends");
        assert!(
            !p.glide_qb,
            "the glide quick boost flag clears when the burst ends"
        );
        assert!(p.glide, "the glide resumes after the glide quick boost");

        // A normal High Boost still exits the glide.
        p.qb = None;
        p.qb_cooldown = 0.0;
        p.en = p.en_max;
        p.glide = true;
        p.glide_qb = false;
        assert!(start_quick_boost(&mut p, false));
        assert!(!p.glide, "a normal High Boost clears the glide");
        assert!(!p.glide_qb);
    }

    #[test]
    fn glide_holds_speed_above_max_and_accelerates_below() {
        // 0x8281e408: thrust adds below max; at/above max the speed is held and only steered.
        let max = 0.463;
        let below = glide_thrust(Vec2::new(0.0, 0.40), Vec2::new(0.0, 0.01), max);
        assert!(
            (below.length() - 0.41).abs() < 1e-4,
            "below max adds the thrust"
        );
        let at_cap = glide_thrust(Vec2::new(0.0, max), Vec2::new(0.01, 0.0), max);
        assert!(
            (at_cap.length() - max).abs() < 1e-4,
            "at max the speed stays capped"
        );
        assert!(at_cap.x > 0.0, "at max a perpendicular thrust steers");
        let above = glide_thrust(Vec2::new(0.0, 0.60), Vec2::new(0.01, 0.0), max);
        assert!(
            (above.length() - 0.60).abs() < 1e-4,
            "above glide max the entry speed is held, not braked"
        );
    }

    #[test]
    fn glide_heading_gate_allows_100_degrees_and_ends_beyond() {
        // 0x8281ea30 / 0x8281e898: pass while cos(stick - velocity) >= -sin(10 deg), i.e. the
        // stick may sit up to 100 deg off the current velocity (private/xenia/glide_gate.jsonl).
        let cone = 0.174532925_f32;
        let forward = Vec3::NEG_Z;
        let at = |deg: f32| Vec2::new(deg.to_radians().sin(), deg.to_radians().cos());
        let velocity = Vec2::Y; // world +Z; at() measures the stick away from it
        assert!(glide_heading_ok(at(99.5), velocity, forward, cone));
        assert!(!glide_heading_ok(at(100.5), velocity, forward, cone));
        assert!(!glide_heading_ok(at(180.0), velocity, forward, cone));
        // At a standstill the 360 heading is local forward (atan2(0, 0) = 0), world -Z here.
        assert!(glide_heading_ok(
            Vec2::new(0.0, -1.0),
            Vec2::ZERO,
            forward,
            cone
        ));
        assert!(!glide_heading_ok(
            Vec2::new(0.0, 1.0),
            Vec2::ZERO,
            forward,
            cone
        ));
    }

    #[test]
    fn reverse_stick_ends_the_glide() {
        use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;
        let ctrl = AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 5001)
            .map(|r| AcCtrlParam::calculate(&r.data))
            .expect("design 5001");
        let collision = Collision::default();
        let mut p = AcMotion::new(ctrl);
        let mut pos = Vec3::ZERO;
        p.boost = true;
        p.glide = true;
        p.en = ctrl.en_capacity;
        p.velocity = Vec3::new(0.0, 0.0, -0.4); // gliding toward world -Z

        // Forward stick stays inside the cone.
        let forward = InputFrame {
            stick: Vec2::Y,
            ..InputFrame::default()
        };
        step(&mut p, &forward, &mut pos, &collision);
        assert!(p.glide, "forward stick keeps the glide");

        // Reversing the stick is outside the 100 degree cone -> the action ends the glide.
        let backward = InputFrame {
            stick: -Vec2::Y,
            ..InputFrame::default()
        };
        step(&mut p, &backward, &mut pos, &collision);
        assert!(!p.glide, "a reverse stick ends the glide");
    }

    #[test]
    fn en_glide_drains_and_ends_on_depletion() {
        use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;
        let ctrl = AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 5001)
            .map(|r| AcCtrlParam::calculate(&r.data))
            .expect("design 5001");
        let collision = Collision::default();
        let mut p = AcMotion::new(ctrl);
        let mut pos = Vec3::ZERO;
        p.boost = true;
        p.glide = true;
        // Start with just enough EN for 2 ticks of glide (net drain per tick = glide - net_rec).
        let net_rec = ctrl.en_output_tick - ctrl.passive_en_drain_tick;
        let net_drain = ctrl.glide_en_tick - net_rec;
        assert!(
            net_drain > 0.0,
            "glide drains faster than generator surplus"
        );
        p.en = ctrl.glide_en_tick * 1.5;

        let input = InputFrame {
            stick: Vec2::Y,
            ..InputFrame::default()
        };
        step(&mut p, &input, &mut pos, &collision);
        assert!(p.glide, "still gliding while EN >= glide_en_tick");
        // Keep stepping until EN drops below glide_en_tick -> glide must end.
        for _ in 0..20 {
            if !p.glide {
                break;
            }
            step(&mut p, &input, &mut pos, &collision);
        }
        assert!(!p.glide, "glide ends once EN < glide_en_tick");
        let after_stop = p.en;
        step(&mut p, &input, &mut pos, &collision);
        assert!(
            p.en > after_stop,
            "EN immediately recharges after glide ends"
        );
    }

    #[test]
    fn en_wall_kick_drains_and_skips_when_low() {
        use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;
        let ctrl = AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 5001)
            .map(|r| AcCtrlParam::calculate(&r.data))
            .expect("design 5001");
        assert!((ctrl.trijump_en_drain - ctrl.hi_en * 1.5).abs() < 1e-2);
        let collision = Collision::default();
        let mut p = AcMotion::new(ctrl);
        let mut pos = Vec3::new(0.0, 20.0, 0.0);
        p.airborne = true;
        p.wj = Some(WallJumpPhase::Accel {
            timer: 0.0,
            dir: Vec3::Y,
            clip_dir: 0,
        });
        let input = InputFrame::default();
        let before = p.en;
        step(&mut p, &input, &mut pos, &collision);
        assert!(p.velocity.y > 0.0, "wall kick accelerates with EN");
        assert!(p.en < before, "wall kick drains trijump_en_tick");

        // Insufficient EN skips wall-kick acceleration (0x828245e8).
        p.velocity = Vec3::ZERO;
        p.en = ctrl.trijump_en_tick * 0.5;
        step(&mut p, &input, &mut pos, &collision);
        assert!(
            p.velocity.y <= 0.0,
            "wall kick skips accel when EN < trijump_en_tick"
        );
    }

    #[test]
    fn boost_charge_kick_triggers_from_hold_drains_en_and_rebounds() {
        use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;
        let ctrl = AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 5001)
            .map(|r| AcCtrlParam::calculate(&r.data))
            .expect("design 5001");
        assert!(ctrl.kick_power > 4000.0 && ctrl.kick_recoil > 1500.0);
        assert_eq!(ctrl.kick_accel_frames, 3.0);
        assert_eq!(ctrl.kick_after_frames, 35.0);

        let ground = Collision::with_hits(
            vec![crate::collision::Plane {
                y: 0.0,
                layer: Layer::Ground,
            }],
            Vec::new(),
        );
        // 1. Holding V (□ + HOLD) through High Boost Phase 1 -> Phase 2 launches KickState.
        let mut p = AcMotion::new(ctrl);
        let mut pos = Vec3::ZERO;
        let en_start = p.en;
        assert!(start_quick_boost(&mut p, false));
        let hold_v = InputFrame {
            stick: Vec2::Y,
            qb_held: true,
            ..InputFrame::default()
        };
        let mut entered_kick = false;
        for _ in 0..40 {
            step(&mut p, &hold_v, &mut pos, &ground);
            if p.in_kick() {
                entered_kick = true;
                break;
            }
        }
        assert!(
            entered_kick,
            "holding High Boost into Phase 2 must launch KickState"
        );
        assert_eq!(p.kicks_started, 1);
        assert!(
            p.en < en_start - ctrl.qb_en_drain,
            "kick must drain qb_en_drain"
        );
        let kick_speed = p.horiz_speed_kmh();
        assert!(
            kick_speed >= ctrl.kick_max_speed * 0.9,
            "kick speed {kick_speed} km/h should be >= 90% kick_max_speed {}",
            ctrl.kick_max_speed
        );

        // Contact rebounds & stops KickState cleanly (`0x828433b8`).
        p.on_kick_contact();
        assert!(!p.in_kick());
        assert_eq!(p.kicks_landed, 1);

        // 2. Insufficient EN blocks KickState (`0x8281f278`).
        p.en = ctrl.qb_en_drain * 0.5;
        assert!(!p.start_kick(), "KickState blocked when EN < qb_en_drain");

        // 3. Downward pitch dive (`NewAcBehavior +0x538`, `0x82823938`) adds downward vy.
        p.en = p.en_max;
        p.airborne = true;
        pos = Vec3::new(0.0, 50.0, 0.0);
        p.velocity = Vec3::ZERO;
        p.aim_pitch = -0.7; // looking down
        assert!(p.start_kick());
        step(
            &mut p,
            &InputFrame::default(),
            &mut pos,
            &Collision::default(),
        );
        assert!(p.velocity.y < 0.0, "downward pitch kick must dive downward");
    }

    #[test]
    fn blade_homing_lunge_and_swing_phases() {
        use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;
        let ctrl = AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 5001)
            .map(|r| AcCtrlParam::calculate(&r.data))
            .expect("design 5001");
        let ground = Collision::with_hits(
            vec![crate::collision::Plane {
                y: 0.0,
                layer: Layer::Ground,
            }],
            Vec::new(),
        );
        let mut p = AcMotion::new(ctrl);
        let mut pos = Vec3::ZERO;
        // Locked target 25 m ahead along -Z (beyond 12 m instant-swing distance).
        let target_pos = Vec3::new(0.0, 3.0, -25.0);
        let launch = BladeLaunch {
            hand_idx: 1,
            physical: false,
            target: Some((0, target_pos)),
            hit_radius: 22.0,
            blade_radius: 2.0,
            max_hits: 3,
            decay_power: 0.5,
            decay_recoil: 0.5,
            damage_type: crate::damage::DamageType::Te,
            damage_power: 8107.0,
            damage_recoil: 0.0,
            trail_fx: 4104,
            hit_low_fx: 746,
            hit_hi_fx: 746,
            hit_low_se: 9501,
            hit_hi_se: 9501,
        };
        assert!(p.start_blade(launch));
        assert!(p.in_blade());
        assert_eq!(p.blade.as_ref().unwrap().phase, BladePhase::Charge);

        let mut saw_dash = false;
        let mut saw_swing = false;
        let mut saw_recovery = false;
        for _ in 0..120 {
            step(&mut p, &InputFrame::default(), &mut pos, &ground);
            match p.blade.as_ref().map(|b| b.phase) {
                Some(BladePhase::Dash) => saw_dash = true,
                Some(BladePhase::Swing) => saw_swing = true,
                Some(BladePhase::Recovery) => saw_recovery = true,
                Some(BladePhase::Charge) => {}
                None => break,
            }
        }
        assert!(
            saw_dash,
            "locked target 25 m away must trigger BladePhase::Dash"
        );
        assert!(
            saw_swing,
            "blade dash must transition into BladePhase::Swing"
        );
        assert!(
            saw_recovery,
            "blade swing must transition into BladePhase::Recovery"
        );
        assert!(
            !p.in_blade(),
            "blade attack must finish cleanly after recovery"
        );
        assert!(
            pos.z < -8.0,
            "homing blade dash should close significant distance toward target (z={})",
            pos.z
        );
    }

    #[test]
    fn water_return_respawns_submerged_ac_to_nearest_point() {
        use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;
        let ctrl = AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 5001)
            .map(|r| AcCtrlParam::calculate(&r.data))
            .expect("design 5001");
        let mut p = AcMotion::new(ctrl);
        p.velocity = Vec3::new(3.0, -2.0, 1.0);
        p.airborne = true;
        p.glide = true;

        let wr = crate::map::WaterReturnPoints {
            points: vec![
                crate::map::Start {
                    position: Vec3::new(100.0, 20.0, 0.0),
                    yaw: 0.5,
                },
                crate::map::Start {
                    position: Vec3::new(-50.0, 15.0, 10.0),
                    yaw: -1.0,
                },
            ],
            water_height: Some(3.0),
        };
        let fallback = crate::map::Start {
            position: Vec3::ZERO,
            yaw: 0.0,
        };
        let ground = Collision::default();

        // Floating above the surface, or wading/hovering with the feet within
        // WATER_RETURN_DEPTH of it (here 3 m), never returns: this is the `--plane --water 2`
        // case where the AC glides 2 m under the test surface.
        let mut above = Vec3::new(-45.0, 5.0, 12.0);
        assert!(!check_water_return(
            &mut p, &mut above, &wr, fallback, &ground
        ));
        let mut wading = Vec3::new(-45.0, 1.0, 12.0);
        assert!(!check_water_return(
            &mut p,
            &mut wading,
            &wr,
            fallback,
            &ground
        ));

        // Sunk past the torso (feet more than WATER_RETURN_DEPTH under 3.0): teleports to the
        // nearest return point (-50, 15, 10).
        let mut submerged = Vec3::new(-45.0, -1.0, 12.0);
        assert!(check_water_return(
            &mut p,
            &mut submerged,
            &wr,
            fallback,
            &ground
        ));
        assert_eq!(submerged, Vec3::new(-50.0, 15.0, 10.0));
        assert_eq!(p.yaw, -1.0);
        assert_eq!(p.velocity, Vec3::ZERO);
        assert!(!p.airborne && !p.glide);
    }

    #[test]
    fn water_return_skips_maps_without_kind_50_points() {
        use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;
        let ctrl = AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 5001)
            .map(|r| AcCtrlParam::calculate(&r.data))
            .expect("design 5001");
        let mut p = AcMotion::new(ctrl);
        let wr = crate::map::WaterReturnPoints {
            points: Vec::new(),
            water_height: Some(127.0),
        };
        let fallback = crate::map::Start {
            position: Vec3::ZERO,
            yaw: 0.0,
        };
        let ground = Collision::default();
        // m3750 Agria/M42: env water_height 127, start (375.6, -1.1, 182.3), no kind-50
        // return points. The dry interior must not respawn the AC every frame.
        let mut pos = Vec3::new(375.6, -1.1, 182.3);
        assert!(!check_water_return(
            &mut p, &mut pos, &wr, fallback, &ground
        ));
        assert_eq!(pos, Vec3::new(375.6, -1.1, 182.3));
    }

    /// Boosters on hold the AC on the water surface; boosters off let it fall through into the
    /// water return.
    #[test]
    fn boost_hovers_over_water() {
        use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;
        let ctrl = AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 5001)
            .map(|r| AcCtrlParam::calculate(&r.data))
            .expect("design 5001");
        let collision = Collision::with_hits(
            vec![
                crate::collision::Plane {
                    y: 0.0,
                    layer: Layer::Ground,
                },
                crate::collision::Plane {
                    y: 10.0,
                    layer: Layer::Water,
                },
            ],
            Vec::new(),
        );

        // Boosters on: a falling, moving AC is caught at the 10 m surface and stays there (the
        // horizontal move must not walk it under over time).
        let moving = InputFrame {
            stick: Vec2::Y,
            ..InputFrame::default()
        };
        let mut p = AcMotion::new(ctrl);
        p.airborne = true;
        p.boost = true;
        p.velocity = Vec3::new(0.0, -5.0, 0.0);
        let mut pos = Vec3::new(0.0, 12.0, 0.0);
        for _ in 0..120 {
            step(&mut p, &moving, &mut pos, &collision);
            assert!(
                pos.y >= 10.0 - 1e-3,
                "boosted AC sank to {} while moving",
                pos.y
            );
        }
        assert!(
            (pos.y - 10.0).abs() < 1e-2,
            "expected hover at 10 m, got {}",
            pos.y
        );

        // A tick that dipped under the surface while moving is pulled back up.
        let mut p = AcMotion::new(ctrl);
        p.airborne = true;
        p.boost = true;
        p.velocity = Vec3::ZERO;
        let mut pos = Vec3::new(0.0, 9.0, 0.0);
        step(&mut p, &moving, &mut pos, &collision);
        assert!(
            (pos.y - 10.0).abs() < 1e-3,
            "under-surface recovery should hold 10 m, got {}",
            pos.y
        );

        // Boosters off: it falls through the water layer towards the 0 m ground.
        let mut p = AcMotion::new(ctrl);
        p.airborne = true;
        p.boost = false;
        p.velocity = Vec3::new(0.0, -5.0, 0.0);
        let mut pos = Vec3::new(0.0, 12.0, 0.0);
        for _ in 0..30 {
            step(&mut p, &InputFrame::default(), &mut pos, &collision);
        }
        assert!(
            pos.y < 10.0,
            "boosters off should drop below the surface, got {}",
            pos.y
        );
    }

    #[test]
    fn qb_shortage_and_high_boost_tae_ids() {
        assert_eq!(high_boost_tae_id(false, false, false), 0x0C1D);
        assert_eq!(high_boost_tae_id(false, false, true), 0x0C1E);
        assert_eq!(high_boost_tae_id(true, false, false), 0x0C1F);
        assert_eq!(high_boost_tae_id(true, false, true), 0x0C20);
        assert_eq!(high_boost_tae_id(true, true, false), 0x0C21);
        assert_eq!(high_boost_tae_id(true, true, true), 0x0C22);

        let design = &find(
            acvd_data::generated::ac_unit::PARAM_ACASSEMBLYDRAWING_BIN,
            5001,
        )
        .expect("design 5001")
        .data;
        let mut p = AcMotion::new(AcCtrlParam::calculate(design));
        // Drain EN below qb_en_drain -> QB attempt fails and increments qb_shortages.
        p.en = (p.ctrl.qb_en_drain * 0.5).max(0.0);
        assert!(!start_quick_boost(&mut p, false));
        assert_eq!(p.qb_shortages, 1);
        assert_eq!(p.qb_charge_starts, 0);

        // Refill EN -> QB charge starts and records 0xC1D (grounded).
        p.en = p.en_max;
        assert!(start_quick_boost(&mut p, false));
        assert_eq!(p.qb_charge_starts, 1);
        assert_eq!(p.last_qb_tae, 0x0C1D);

        // Release at second-stage sweet spot while airborne -> records 0xC22.
        p.airborne = true;
        let sweet_ratio = p.ctrl.qb_sweet_frames / p.ctrl.qb_charge_max_frames;
        fire_quick_boost(&mut p, Vec2::Y, sweet_ratio);
        assert!(p.second_stage_qb);
        assert_eq!(p.last_qb_tae, 0x0C22);
    }

    #[test]
    fn walk_and_boost_crush_destructible_props() {
        use crate::collision::{GroundSurface, Hit, Layer, Plane};
        use crate::ids::PropId;
        use crate::map::Destructible as DestructibleProp;

        let design = &find(
            acvd_data::generated::ac_unit::PARAM_ACASSEMBLYDRAWING_BIN,
            5001,
        )
        .expect("design 5001")
        .data;
        let ctrl = AcCtrlParam::calculate(design);
        let prop_wall = |z: f32, prop_id: u32| {
            Collision::with_hits(
                vec![Plane {
                    y: 0.0,
                    layer: Layer::Ground,
                }],
                vec![
                    Hit {
                        a: Vec3::new(-4.0, 0.0, z),
                        b: Vec3::new(4.0, 0.0, z),
                        c: Vec3::new(4.0, 4.0, z),
                        layer: Layer::Ground,
                        wall_kickable: false,
                        blocks_fcs_los: true,
                        blocks_ai_los: true,
                        blocks_bullet: true,
                        blocks_camera: true,
                        surface: GroundSurface::Concrete,
                        prop_id,
                        only_when_broken: false,
                    },
                    Hit {
                        a: Vec3::new(-4.0, 0.0, z),
                        b: Vec3::new(4.0, 4.0, z),
                        c: Vec3::new(-4.0, 4.0, z),
                        layer: Layer::Ground,
                        wall_kickable: false,
                        blocks_fcs_los: true,
                        blocks_ai_los: true,
                        blocks_bullet: true,
                        blocks_camera: true,
                        surface: GroundSurface::Concrete,
                        prop_id,
                        only_when_broken: false,
                    },
                ],
            )
        };
        let make_prop = |prop_id: u32, hp: f32, cutoff: f32, mult: f32| DestructibleProp {
            prop_id: PropId(prop_id),
            model: "o2024".into(),
            center: Vec3::new(0.0, 1.5, -5.0),
            radius: 3.0,
            hp,
            max_hp: hp,
            cutoff_damage: cutoff,
            collision_damage_mult: mult,
            damage_id: 0,
            broken: false,
            just_broken: false,
        };
        let walk_fwd = InputFrame {
            stick: Vec2::Y,
            ..InputFrame::default()
        };

        // 1. 耐久A (AP=1, collision_defensibility=0 -> mult=1.0): walking into it shatters it
        //    and the AC passes straight past z = -5.0 without getting blocked.
        let mut col_a = prop_wall(-5.0, 1);
        let mut prop_a = make_prop(1, 1.0, 0.0, 1.0);
        let mut p = AcMotion::new(ctrl);
        let mut pos = Vec3::ZERO;
        for _ in 0..90 {
            step_with_props(&mut p, &walk_fwd, &mut pos, &mut col_a, |id, raw| {
                id == prop_a.prop_id.0 && prop_a.apply_collision_damage(raw)
            });
        }
        assert!(
            prop_a.broken,
            "walking into 耐久A ground prop must shatter it"
        );
        assert!(
            pos.z < -6.5,
            "AC must crash straight through broken 耐久A prop, got z={}",
            pos.z
        );

        // 2. 耐久B (AP=500, collision_defensibility=0 -> mult=1.0): boosting into it shatters it
        //    and the AC crashes through.
        let mut col_b = prop_wall(-5.0, 2);
        let mut prop_b = make_prop(2, 500.0, 0.0, 1.0);
        let mut p = AcMotion::new(ctrl);
        p.boost = true;
        let mut pos = Vec3::ZERO;
        for _ in 0..60 {
            step_with_props(&mut p, &walk_fwd, &mut pos, &mut col_b, |id, raw| {
                id == prop_b.prop_id.0 && prop_b.apply_collision_damage(raw)
            });
        }
        assert!(prop_b.broken, "boosting into 耐久B prop must shatter it");
        assert!(
            pos.z < -6.5,
            "AC must crash through broken 耐久B prop, got z={}",
            pos.z
        );

        // 3. 耐久C / indestructible (collision_defensibility=100 -> mult=0.0): resists AC body
        //    collision and blocks the AC at the wall.
        let mut col_c = prop_wall(-5.0, 3);
        let mut prop_c = make_prop(3, 2500.0, 0.0, 0.0);
        let mut p = AcMotion::new(ctrl);
        p.boost = true;
        let mut pos = Vec3::ZERO;
        for _ in 0..60 {
            step_with_props(&mut p, &walk_fwd, &mut pos, &mut col_c, |id, raw| {
                id == prop_c.prop_id.0 && prop_c.apply_collision_damage(raw)
            });
        }
        assert!(
            !prop_c.broken,
            "collision_defensibility=100 prop must not break on body contact"
        );
        assert!(
            pos.z > -5.0,
            "intact collision-immune prop must block the AC, got z={}",
            pos.z
        );
    }

    /// Boosting up a kerb or ramp. `quad` builds two triangles.
    fn ramp_collision(deg: f32, kerb: f32) -> Collision {
        use crate::collision::Hit;
        let quad = |x0: f32, x1: f32, z0: f32, z1: f32, y0: f32, y1: f32| {
            let a = Vec3::new(x0, y0, z0);
            let b = Vec3::new(x1, y0, z0);
            let c = Vec3::new(x1, y1, z1);
            let d = Vec3::new(x0, y1, z1);
            vec![Hit::solid(a, b, c), Hit::solid(a, c, d)]
        };
        let mut hits = quad(-50.0, 50.0, 0.0, 30.0, 0.0, 0.0);
        if kerb > 0.0 {
            hits.push(Hit::solid(
                Vec3::new(-50.0, 0.0, 0.0),
                Vec3::new(50.0, 0.0, 0.0),
                Vec3::new(50.0, kerb, 0.0),
            ));
            hits.push(Hit::solid(
                Vec3::new(-50.0, 0.0, 0.0),
                Vec3::new(50.0, kerb, 0.0),
                Vec3::new(-50.0, kerb, 0.0),
            ));
            hits.extend(quad(-50.0, 50.0, -60.0, 0.0, kerb, kerb));
        } else {
            let h = 10.0 * deg.to_radians().tan();
            hits.extend(quad(-50.0, 50.0, -10.0, 0.0, h, 0.0));
            hits.extend(quad(-50.0, 50.0, -60.0, -10.0, h, h));
        }
        Collision::with_hits(Vec::new(), hits)
    }

    fn boost_forward(ctrl: AcCtrlParam) -> (AcMotion, Vec3) {
        let mut p = AcMotion::new(ctrl);
        p.boost = true;
        p.en = ctrl.en_capacity;
        (p, Vec3::new(0.0, 0.0, 10.0))
    }

    fn test_ctrl() -> AcCtrlParam {
        use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;
        AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 5001)
            .map(|r| AcCtrlParam::calculate(&r.data))
            .expect("design 5001")
    }

    /// The body spheres are floor-tangent, so even a 15 cm kerb used to push the AC back and
    /// stop a boost dead at the foot of every ramp. It now steps up and keeps its speed.
    #[test]
    fn boost_steps_over_kerbs() {
        for kerb in [0.15f32, 0.5] {
            let collision = ramp_collision(0.0, kerb);
            let (mut p, mut pos) = boost_forward(test_ctrl());
            let input = InputFrame {
                stick: Vec2::Y,
                ..InputFrame::default()
            };
            for _ in 0..120 {
                step(&mut p, &input, &mut pos, &collision);
            }
            assert!(pos.z < -5.0, "kerb {kerb}: boost stopped at z={}", pos.z);
            assert!(
                (pos.y - kerb).abs() < 1e-3,
                "kerb {kerb}: AC should stand on the kerb, y={}",
                pos.y
            );
            assert!(!p.airborne);
        }
    }

    /// 45-55 degree city ramps were the ones that clipped or stopped the AC: with only this
    /// tick's travel the ground ray started under the next triangle, and at/over 45 degrees the
    /// old floor/wall split could push or miss the ramp. Both now climb without leaving the
    /// ground.
    #[test]
    fn boost_climbs_steep_ramps_without_clipping() {
        for deg in [45.0f32, 52.0] {
            let ctrl = test_ctrl();
            let collision = ramp_collision(deg, 0.0);
            let (mut p, mut pos) = boost_forward(ctrl);
            let input = InputFrame {
                stick: Vec2::Y,
                ..InputFrame::default()
            };
            let tan = deg.to_radians().tan();
            let h = 10.0 * tan;
            let mut worst = f32::INFINITY;
            for _ in 0..80 {
                step(&mut p, &input, &mut pos, &collision);
                assert!(!p.airborne, "{deg} deg: AC left the ground at {pos:?}");
                if (-10.0..0.0).contains(&pos.z) {
                    worst = worst.min(pos.y - (-pos.z) * tan);
                }
            }
            assert!(
                worst > -0.05,
                "{deg} deg: clip through the floor by {}",
                -worst
            );
            assert!(
                pos.z < -12.0 && (pos.y - h).abs() < 1e-2,
                "{deg} deg: AC should be on the top platform, got {pos:?}"
            );
        }
    }

    /// Glide Boost opens with a 5 m vertical ascent. On an incline the ramp surface rises
    /// faster than that, so the lift (and the held-height glide after it) used to pass through
    /// the slope. It must land on the ramp instead.
    #[test]
    fn glide_boost_lands_on_rising_ground() {
        let ctrl = test_ctrl();
        let collision = ramp_collision(45.0, 0.0);
        let (mut p, mut pos) = boost_forward(ctrl);
        p.glide = true;
        let input = InputFrame {
            stick: Vec2::Y,
            ..InputFrame::default()
        };
        let mut worst = f32::INFINITY;
        for _ in 0..90 {
            step(&mut p, &input, &mut pos, &collision);
            if (-10.0..0.0).contains(&pos.z) {
                worst = worst.min(pos.y - (-pos.z));
            }
        }
        assert!(
            worst > -0.05,
            "glide boost: clip through the ramp by {}",
            -worst
        );
        assert!(
            pos.z < -12.0 && (pos.y - 10.0).abs() < 0.1,
            "glide boost: AC should end on the top platform, got {pos:?}"
        );
    }

    /// Plain airborne boost (no glide) is held level by the boost gravity, so a ramp can also
    /// rise through it; the same upward probe must land it.
    #[test]
    fn airborne_boost_lands_on_rising_ground() {
        let ctrl = test_ctrl();
        let collision = ramp_collision(45.0, 0.0);
        let mut p = AcMotion::new(ctrl);
        p.boost = true;
        p.airborne = true;
        p.en = ctrl.en_capacity;
        p.velocity = Vec3::new(0.0, 0.0, -0.6);
        let mut pos = Vec3::new(0.0, 2.0, 2.0);
        let input = InputFrame {
            stick: Vec2::Y,
            ..InputFrame::default()
        };
        let mut worst = f32::INFINITY;
        for _ in 0..90 {
            step(&mut p, &input, &mut pos, &collision);
            if (-10.0..0.0).contains(&pos.z) {
                worst = worst.min(pos.y - (-pos.z));
            }
        }
        assert!(worst > -0.05, "airborne boost: clip by {}", -worst);
        assert!(
            pos.z < -12.0 && (pos.y - 10.0).abs() < 0.1,
            "airborne boost: AC should end on the top platform, got {pos:?}"
        );
    }

    /// Steeper than the 55 degree walkable limit is still a wall and stops the AC.
    #[test]
    fn steep_ramp_still_blocks() {
        let collision = ramp_collision(60.0, 0.0);
        let (mut p, mut pos) = boost_forward(test_ctrl());
        let input = InputFrame {
            stick: Vec2::Y,
            ..InputFrame::default()
        };
        for _ in 0..80 {
            step(&mut p, &input, &mut pos, &collision);
        }
        assert!(
            pos.z > 0.0,
            "a 60 degree ramp must stay a wall, got {pos:?}"
        );
    }
}
