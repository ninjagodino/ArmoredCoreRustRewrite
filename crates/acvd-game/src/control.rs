//! Piloting the shown AC, with the pad layout of the game's manual (`lang/en/text/menu/manual.fmg`):
//! WASD / left stick move relative to its facing, Q/E / right stick X turn, Up/Down / right stick Y
//! pitch the follow camera, V / L1 toggles boost mode, Space / South (×) jumps and turns boost
//! mode on, Ctrl / L3 glides while boosting on the ground (the air clips take over once it
//! leaves the ground), Shift / West (□) high-boosts (quick boost)
//! along the stick, forward without one. Movement steps at the game's 60 Hz tick
//! in metres per tick with the AC's `AcCtrlParam`
//! (`sheets/ac_ctrl_calc.csv`: its parts and build weight through the game's
//! `AcCtrlParamCalc.lua`), and every step plays the clip of the `sheets/ac_states.csv` state it
//! lands in, through that state's `param/acmotion.bin` row.

use acvd_data::generated::ac_unit::{
    MotionCollateSt, PARAM_ACANIMHOKAN_BIN, PARAM_ACMOTION_BIN, PARAM_MOTIONCOLLATE_BIN,
};
use acvd_data::generated::camera::{
    AcCameraActionSt, AccamBehaviorParamSt, PARAM_ACCAMBEHAVIORPARAM_BIN, PARAM_ACCAMERAACTION_BIN,
    PARAM_ACCAMERAOFFSETPARAM_BIN,
};
use acvd_data::generated::ctrl::AcCtrlParam;
use acvd_data::generated::tuning::{AC_ANIM_PARAM, NEW_AC_BEHAVIOR};
use acvd_data::{find, Row};
use acvd_render::app::Orbit;
use bevy::prelude::*;

use crate::blur::ZoomBlur;
use crate::collision::{Collision, Layer, RAY_HALF};
use crate::hud::Status;
use crate::pose::{Driven, Fade, Motion};

/// Movement ticks per second; `AcCtrlParam`'s `_tick` values are per tick.
const TICK_RATE: f32 = 60.0;
/// Speed blur ease rate (360 `CamFx_update` `0x82bfb748` -> `0x82bfa9f0` runs once per 1/30 s game
/// frame; Xenia probe `private/xenia/blur_rate.txt`).
const BLUR_RATE: f32 = 30.0;
/// Metres per tick to km/h.
const TICK_TO_KMH: f32 = TICK_RATE * 3.6;
/// Pitch block at AC+0x234 +0x5c (01.02 dump): each tick `vel = vel x (1 - 0.4) + look x 0.007`,
/// then `angle += vel`, clamped to ±1.2217305 rad (±70°).
const PITCH_ACC: f32 = 0.007;
const PITCH_DAMP: f32 = 0.4;
const PITCH_LIMIT: f32 = 1.221_730_5;
/// Camera actions that keep their own `accameraaction` row in boost mode (360 0x8285db68); the
/// rest become boost on the ground (38) or in the air (39).
const BOOST_KEEPS: [u32; 10] = [0, 0x13, 0x1a, 0x1b, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x32];
/// Eye follow rates of the camera action below this fall back to the behavior's (360 0x8285d610).
const ACTION_RATE_MIN: f32 = 0.01;
/// The look-at is pushed this far along the view before the eye lift and height are added, so
/// they barely tilt the view (360 0x82860960, 0x82095920).
const LOOK_AT_FAR: f32 = 1000.0;
/// The camera's base point is this clip bone's world position, not the AC root: in the 01.02
/// dump the eye before its lift sits LookAtHeight above the AC's waist copy (root + 4.06 m).
const WAIST_BONE: &str = "center";
/// Frames the sideways eye shift takes to change sides without a camera action (360 0x83713b68).
const NO_ACTION_SIDE_FRAMES: f32 = 120.0;
/// A camera action's EyeDistance replaces the behavior's EyeDist only above this (360 0x820ba68c).
const ACTION_DIST_MIN: f32 = 0.001;
/// The zoom-blur filter draws only above this alpha (360 0x82c98368: `if (9 < alpha)`).
const BLUR_MIN_ALPHA: u8 = 9;
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
enum WallJumpPhase {
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
    Accel {
        timer: f32,
        dir: Vec3,
        clip_dir: u8,
    },
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
enum QbPhase {
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
/// `0x82844570` → `0x8283e1f0` / `0x828433a0`, `AcMove_boostChargeKick` `0x82823740`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct KickState {
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

/// Parameters passed to `Pilot::start_blade` when triggering a melee blade weapon.
#[derive(Clone, Copy, Debug)]
pub struct BladeLaunch {
    /// `0` = right arm, `1` = left arm.
    pub hand_idx: usize,
    /// `0x8247f170`: true for KE physical blades & CE heat piles; false for TE laser blades.
    pub physical: bool,
    /// FCS-locked target entity & world position (if any).
    pub target: Option<(Entity, Vec3)>,
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

/// Live melee blade attack state on `Pilot`.
#[derive(Clone, Debug)]
pub(crate) struct BladeAction {
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
    /// Whether the attack uses the airborne downward-thrust clip (`NewAcBehavior +0x2e2` = 35°).
    pub downward: bool,
    /// True once the blade trail FFX has been spawned on entering `Swing`.
    pub trail_spawned: bool,
    /// Targets already struck during this swing (caps per-target hits at 1, total at `max_hits`).
    pub hit_entities: Vec<Entity>,
}
/// Whether input drives the AC (`P` toggles) instead of browsing clips.
#[derive(Resource)]
pub struct Piloting(pub bool);

/// Keys held for the whole run (`--hold w,shift`), for scripted checks.
#[derive(Resource, Default)]
pub struct Held(pub Vec<KeyCode>);

/// Where an unturned AC faces: the assembled models' fronts point down -Z in Bevy axes.
const FORWARD: Vec3 = Vec3::NEG_Z;

/// Heading of the move, degrees clockwise from forward, for a direction-wheel clip.
///
/// Velocity (m/tick) against the body wins once it is moving; `stick` covers the frames before
/// that. The sign is mirrored: the clip's 90 degree key matches a left stick (checked by eye
/// on the body lean). The booster nozzles use the same heading (`sheets/booster_anim.csv`).
pub(crate) fn lean_heading(yaw: f32, velocity: Vec3, stick: Vec2) -> f32 {
    let forward = Quat::from_rotation_y(yaw) * FORWARD;
    let right = forward.cross(Vec3::Y);
    let v = Vec2::new(velocity.dot(right), velocity.dot(forward));
    let local = if v.length() > 0.02 { v } else { stick };
    (-local.x).atan2(local.y).to_degrees().rem_euclid(360.0)
}

/// Ready-position camera stance state (360 `ac_action == 0xc`, `snipe_ctrl +0x11c`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum ReadyCamera {
    #[default]
    Off,
    /// Deploying ready stance (`snipe_ctrl +0x11c <= 1`): `accameraaction` row `40` (`0x28`, `狙撃構え`).
    Deploying,
    /// Deployed & zoomed (`snipe_ctrl +0x11c >= 2`, `0x8285d548`): `accameraaction` row `49` (`0x31`,
    /// `狙撃構え完了後`) with `FovMaxAngle` overridden to `fov_deg` (`0x8285e418` -> `cat-10 +0x168`)
    /// and aim turn speed scaled by `turn_scale` (`0x8287b658` -> `cat-10 +0x16c`).
    Zoomed { fov_deg: f32, turn_scale: f32 },
}

/// Movement state of the AC root. Velocity is in metres per tick, Bevy axes; `yaw` turns
/// about +Y (positive turns left), 0 facing `FORWARD`.
#[derive(Component)]
pub struct Pilot {
    pub ctrl: AcCtrlParam,
    /// Current generator energy (`AC + 0x214` `+0x04`, `0x82830910` / `0x828307c8`).
    pub en: f32,
    /// Generator energy capacity (`AC + 0x214` `+0x08`, `0x8247a390` -> `0x82858408`).
    pub en_max: f32,
    pub velocity: Vec3,
    pub yaw: f32,
    pub boost: bool,
    pub glide: bool,
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
    ow_dash_ticks: f32,
    ow_dash_dir: Vec3,
    /// Current map area band (`POINT_PARAM_ST` kinds `100`–`102`, `manual.fmg` 20500).
    pub area_zone: crate::map::AreaZone,
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
    qb: Option<QbPhase>,
    qb_cooldown: f32,
    /// Active Wall Jump state (`0x82092a20`), and wall-kick lockout ticks (`action_ctrl +0x114`:
    /// set to `wall_jump_post_jump_lockout` on normal jump `0x8283d4b8` and
    /// `trijump_lockout_frames` on wall kick `0x82841338`).
    wj: Option<WallJumpPhase>,
    wj_lockout: f32,
    wall_jump_queued: bool,
    /// Active Boost Charge Kick state (`KickState` `0x820928d0`, `0x8283fdb0`).
    pub(crate) kick: Option<KickState>,
    /// Active melee blade state (`BladeState` `0x83714024`).
    pub(crate) blade: Option<BladeAction>,
    /// World position before the last movement frame (for swept melee collision).
    pub(crate) prev_position: Vec3,
    /// The body-relative eighth of a high boost whose clip starts on the next state pick.
    quick_pending: Option<u8>,
    /// Ticks of take-off acceleration left.
    takeoff: f32,
    jump_queued: bool,
    /// Touched down during the last steps, and downward impact speed at touchdown (km/h).
    landed: bool,
    landing_impact_kmh: f32,
    /// Ready-position camera stance (`accameraaction` row 40 -> 49 + zoom FOV override `0x8285e418`).
    pub ready_cam: ReadyCamera,
    /// The state playing, and whether its clip is in the set.
    state: Option<(&'static str, Option<u8>)>,
    /// A sniper stance clip owns the body until it is released.
    suppress_locomotion: bool,
    clip_ok: bool,
    accumulator: f32,
    /// The legs' `motioncollate` HokanParamID: the `acanimhokan.bin` block (0, or 500 for tanks)
    /// each `acmotion` InterpolateID is added to.
    hokan_base: u32,
    /// Default follow camera (`sheets/camera_follow.csv`): look-at from the root, eye behind it.
    cam: FollowCam,
}

/// Placement of the default follow camera, eased toward its targets by the follow rates.
#[derive(Clone, Copy)]
struct FollowCam {
    look_at_height: f32,
    /// Local offset of the look-at from LookAtOffsetX, along -X and then -Z.
    look_at_offset: Vec3,
    /// Offset-row EyeDistOffset / EyeMinDist / EyeOffsetYOffsetRate (behavior-only fallbacks:
    /// 0 / 0 / 1).
    eye_dist_offset: f32,
    eye_min_dist: f32,
    lift_rate: f32,
    /// Eye distance and lift eased toward the camera action's EyeDistance (else behavior
    /// EyeDist) and EyeOffsetY over its EyeFadeInFrame; set outright on the first frame.
    eye_dist: Blend,
    lift: Blend,
    /// Sideways eye shift: the action's EyeOffsetX eased like the lift, times `side` and the
    /// offset row's EyeOffsetXOffsetRate. `side` eases toward `side_sign` (+1 = eye to the AC's
    /// right) over TurnOffsetMoveFrame; turning flips the sign in rows with bTrunMoveEnable.
    shift: Blend,
    shift_rate: f32,
    side: Blend,
    side_sign: f32,
    eye_height: f32,
    /// Behavior WaterStopYOffset: how far above water the eye stays.
    water_stop: f32,
    /// Behavior FovAngle (degrees) and the camera action's offset from it.
    base_fov: f32,
    fov: Envelope,
    /// The `accameraaction` row in use and its id, plus any weapon-driven FOV override (`0x8285e418`).
    action: Option<(u32, &'static AcCameraActionSt)>,
    fov_override: Option<f32>,
    /// Behavior ShakeMoveAmpl / ShakeMoveFreq x, y (the "freq" is a period in seconds), the
    /// shake's running phase, and the AC's change in horizontal speed over the last tick (km/h).
    shake_ampl: Vec2,
    shake_period: Vec2,
    shake_phase: f32,
    accel: f32,
    /// Impact shake (`0x8285cf88` msg 0x68 -> `CamFx_shake` `0x82bfabdc..0x82bfad08`): pending
    /// accel value (km/h) and remaining decay timer (s).
    impact_pending: f32,
    impact_timer: f32,
    /// Footstep shake (`0x8285cfb0` msg 0x69 -> `CamFx_shake` `0x82bfad08..0x82bfadd4`): pending
    /// step pulse and remaining decay timer (s).
    footstep_pending: bool,
    footstep_timer: f32,
    /// Timed impulse / rumble shake (`CamFx_shake` `0x82bfaea0` -> `0x82bf8f00`): linearly decays
    /// over `rumble_duration` seconds.
    rumble_duration: f32,
    rumble_elapsed: f32,
    rumble_ampl: Vec2,
    rumble_freq: Vec2,
    /// Behavior follow rates x / y / front / back, on the ground and in the air.
    eye_rates: [[f32; 4]; 2],
    look_at_rates: [[f32; 4]; 2],
    /// Seconds a rate change takes (CamFollowRateInterpolateFrame / 60), and the rates in use.
    rate_blend_secs: f32,
    eye_rate: [Blend; 4],
    look_at_rate: [Blend; 4],
    /// Offset-row EyeApproachRate / EyeDownRate, else the behavior's.
    approach: f32,
    down_rate: f32,
    eye_forward: f32,
    at_forward: f32,
    /// AC+0x234 +0x5c and its velocity at +0x60.
    pitch: f32,
    pitch_vel: f32,
    /// Mouselook: camera yaw relative to the AC (positive left); the AC turns to close it.
    yaw_off: f32,
    /// Camera yaw (body + offset) last frame, so mouse swings can rotate the eased points rigidly.
    cam_yaw: f32,
    /// Horizontal Lock-Sight lead (rad) during mouselook: `swing()` cancels horizontal camera lag
    /// for crisp mouse aim, so this carries the same `look_at_rate[0]` lag onto the Lock Sight
    /// billboard (always 0 when not in mouselook).
    hud_yaw_lead: f32,
    /// Waist bone offset from the AC root on the last `pilot` step, used by the 3D lock-sight
    /// datum (`0x8287cdd0` -> `0x8260f878`).
    waist: Vec3,
    /// Roll from sideways speed: ZTiltStart/MaxXSpeed (km/h), MaxZTiltAngle (rad), the seconds to
    /// tilt and to level out, and the -1..1 tilt in use.
    tilt_speeds: (f32, f32),
    tilt_max: f32,
    tilt_secs: (f32, f32),
    tilt: Blend,
    /// Behavior row 0, for the speed blur's Blur* fields, and the blur's eased 0..1 intensity
    /// stepped at 30 Hz (`BLUR_RATE`, 360 `0x82bfb748` -> `0x82bfa9f0`).
    behavior: &'static AccamBehaviorParamSt,
    blur: f32,
    blur_accum: f32,
    /// Shown eye and look-at, and last frame's targets; `None` until the first frame snaps.
    shown: Option<Shown>,
}

/// The game's linear float interpolator (disc 0x1136038): a new target restarts from the current
/// value and is reached after `duration` seconds. Starts all zero, like the camera's.
#[derive(Clone, Copy, Default)]
struct Blend {
    from: f32,
    value: f32,
    to: f32,
    t: f32,
    duration: f32,
}

impl Blend {
    fn toward(&mut self, target: f32, duration: f32, dt: f32) -> f32 {
        if target != self.to {
            (self.from, self.to, self.t, self.duration) = (self.value, target, 0.0, duration);
        }
        self.t = (self.t + dt).min(self.duration);
        self.value = if self.duration > 0.0 {
            self.from + (self.to - self.from) * self.t / self.duration
        } else {
            self.to
        };
        self.value
    }
}

/// The game's rise / hold / fall envelope (360 0x82c1b108, output 0x82c1b328): `out` goes from
/// `from` to `to` over `rise` seconds, holds (forever when `hold` < 0), then falls to 0 over `fall`.
#[derive(Clone, Copy, Default)]
struct Envelope {
    level: f32,
    t: f32,
    rise: f32,
    hold: f32,
    fall: f32,
    active: bool,
    from: f32,
    out: f32,
    to: f32,
}

impl Envelope {
    fn start(&mut self, to: f32, rise: f32, hold: f32, fall: f32) {
        let from = if self.active { self.out } else { 0.0 };
        *self = Self {
            rise,
            hold,
            fall,
            active: true,
            from,
            to,
            out: self.out,
            ..Self::default()
        };
    }

    fn fade(&mut self, fall: f32) {
        if self.active {
            (self.rise, self.hold, self.fall, self.t) = (0.0, 0.0, fall, (1.0 - self.level) * fall);
        }
    }

    fn step(&mut self, dt: f32) -> f32 {
        if !self.active {
            return self.out;
        }
        self.t += dt;
        let past = self.t - (self.rise + self.hold);
        if self.hold >= 0.0 && past > self.fall {
            *self = Self::default();
            return 0.0;
        }
        if self.t < self.rise {
            self.level = self.t / self.rise;
        } else if self.hold < 0.0 || self.t - self.rise < self.hold {
            self.level = 1.0;
        } else if past < self.fall {
            self.level = 1.0 - past / self.fall;
        }
        self.out = if self.t < self.rise {
            self.from + (self.to - self.from) * self.level
        } else {
            self.to * self.level
        };
        self.out
    }
}

/// Camera shake wave (360 0x82bf8cc0), per axis with a positive period: the phase angle wraps to
/// (-pi, pi], rises 0..1..0 over the positive half and reads `a x 2/pi - 3` over the negative half.
fn shake_wave(phase: f32, ampl: Vec2, period: Vec2) -> Vec2 {
    use std::f32::consts::{FRAC_2_PI, FRAC_PI_2, PI, TAU};
    let axis = |ampl: f32, period: f32| {
        if period <= 0.0 {
            return 0.0;
        }
        let mut a = (1f32.to_radians() / period) * phase * 360.0;
        a -= (a / TAU).trunc() * TAU;
        if a > PI {
            a -= TAU;
        } else if a < -PI {
            a += TAU;
        }
        ampl * if (0.0..=FRAC_PI_2).contains(&a) {
            a * FRAC_2_PI
        } else if (FRAC_PI_2..=3.0 * FRAC_PI_2).contains(&a) {
            2.0 - a * FRAC_2_PI
        } else {
            a * FRAC_2_PI - 3.0
        }
    };
    Vec2::new(axis(ampl.x, period.x), axis(ampl.y, period.y))
}

/// The `accameraaction` row id for a movement state (360 0x8285db68, AC action names): stop 0,
/// walk 2, turn 3, flight 8, landing 9, glide 26 (急加速), ready-position 40 (狙撃構え) -> 49
/// (狙撃構え完了後, `0x8285d548`), high boost 50 (クイックブースト); boost mode turns the rest
/// into 38 / 39 (in the air).
fn camera_action(state: &str, boost: bool, airborne: bool, ready: ReadyCamera) -> u32 {
    match ready {
        ReadyCamera::Deploying => return 0x28,
        ReadyCamera::Zoomed { .. } => return 0x31,
        ReadyCamera::Off => {}
    }
    let base = match state {
        "idle" => 0,
        "walk" | "dash" => 2,
        "turn_left" | "turn_right" => 3,
        "land" => 9,
        "glide_start" | "glide" | "glide_air_in" | "glide_air" => 0x1a,
        "kick" => 0x2a,
        "wall_jump_prep" | "wall_jump" => 0x2b,
        s if s.starts_with("blade_") || s.starts_with("phys_blade_") => 0x2c,
        "quick_boost" => 0x32,
        _ => 8,
    };
    if boost && !BOOST_KEEPS.contains(&base) {
        38 + u32::from(airborne)
    } else {
        base
    }
}

#[derive(Clone, Copy)]
struct Shown {
    eye: Vec3,
    look_at: Vec3,
    eye_target: Vec3,
    look_at_target: Vec3,
}

/// Moves `shown` toward `target` the way the game eases a camera point each frame: the target
/// slides from `last_target` in 60 Hz steps, and each step closes `rate` of the gap per local axis
/// of `frame` (front when the gap points down -Z), with `1 - (1 - rate)^t` for a partial step, and
/// moves a further `push` x t along local x / y (360 0x82bfbb68).
fn ease(
    shown: Vec3,
    last_target: Vec3,
    target: Vec3,
    rates: [f32; 4],
    push: Vec2,
    frame: Quat,
    dt: f32,
) -> Vec3 {
    let frames = dt * TICK_RATE;
    if frames <= 0.0 {
        return shown;
    }
    let step = (target - last_target) / frames;
    let whole = frames.floor();
    let frac = frames - whole;
    let close = |r: f32, t: f32| {
        if t >= 1.0 || r >= 1.0 {
            r
        } else {
            1.0 - (1.0 - r).powf(t)
        }
    };
    let mut goal = last_target;
    let mut shown = shown;
    let advance = |goal: Vec3, shown: &mut Vec3, t: f32| {
        let d = frame.inverse() * (goal - *shown);
        let z = if d.z < 0.0 { rates[2] } else { rates[3] };
        *shown += frame
            * Vec3::new(
                d.x * close(rates[0], t) + push.x * t,
                d.y * close(rates[1], t) + push.y * t,
                d.z * close(z, t),
            );
    };
    for _ in 0..whole as u32 {
        goal += step;
        advance(goal, &mut shown, 1.0);
    }
    if frac > 0.0 {
        goal += step * frac;
        advance(goal, &mut shown, frac);
    }
    shown
}

/// The `motioncollate` row of a legs motion set without arms or OW motion.
fn collate(legs_motion_id: u8) -> Option<&'static Row<MotionCollateSt>> {
    PARAM_MOTIONCOLLATE_BIN.iter().find(|r| {
        r.data.arms_motion_id == 0
            && r.data.legs_motion_id == legs_motion_id
            && r.data.ow_motion_id == 0
    })
}

impl FollowCam {
    /// Row 0 of `accambehaviorparam` with the `accameraoffsetparam` row of the legs' motion set
    /// (`motioncollate` without arms or OW motion).
    fn of(legs_motion_id: u8) -> Self {
        let behavior = &find(PARAM_ACCAMBEHAVIORPARAM_BIN, 0)
            .expect("accambehaviorparam row 0")
            .data;
        let offset = collate(legs_motion_id).and_then(|r| {
            find(
                PARAM_ACCAMERAOFFSETPARAM_BIN,
                u32::from(r.data.camera_param_id),
            )
        });
        let (eye_dist_offset, eye_min_dist, lift_rate, shift_rate, approach, down_rate) =
            match offset {
                Some(o) => {
                    let o = &o.data;
                    (
                        o.eye_dist_offset,
                        o.eye_min_dist,
                        o.eye_offset_y_offset_rate,
                        o.eye_offset_x_offset_rate,
                        o.eye_approach_rate,
                        o.eye_down_rate,
                    )
                }
                None => {
                    warn!("no accameraoffsetparam row for legs motion {legs_motion_id}");
                    (
                        0.0,
                        0.0,
                        1.0,
                        1.0,
                        behavior.eye_approach_rate,
                        behavior.eye_down_rate,
                    )
                }
            };
        let x = behavior.look_at_offset_x;
        debug!(
            "follow camera: legs motion {legs_motion_id}, eye dist offset {eye_dist_offset}, eye height {}, look-at height {}, offset x {x}",
            behavior.eye_height, behavior.look_at_height
        );
        Self {
            look_at_height: behavior.look_at_height,
            look_at_offset: Vec3::new(-x, 0.0, -x),
            eye_dist_offset,
            eye_min_dist,
            lift_rate,
            eye_dist: Blend::default(),
            lift: Blend::default(),
            shift: Blend::default(),
            shift_rate,
            side: Blend::default(),
            side_sign: 1.0,
            eye_height: behavior.eye_height,
            water_stop: behavior.water_stop_y_offset,
            base_fov: behavior.fov_angle,
            fov: Envelope::default(),
            action: None,
            fov_override: None,
            shake_ampl: Vec2::new(behavior.shake_move_ampl_x, behavior.shake_move_ampl_y),
            shake_period: Vec2::new(behavior.shake_move_freq_x, behavior.shake_move_freq_y),
            shake_phase: 0.0,
            accel: 0.0,
            impact_pending: 0.0,
            impact_timer: 0.0,
            footstep_pending: false,
            footstep_timer: 0.0,
            rumble_duration: 0.0,
            rumble_elapsed: 0.0,
            rumble_ampl: Vec2::ZERO,
            rumble_freq: Vec2::ZERO,
            eye_rates: [
                [
                    behavior.eye_follow_rate_x,
                    behavior.fly_eye_follow_rate_y,
                    behavior.eye_follow_rate_f,
                    behavior.eye_follow_rate_b,
                ],
                [
                    behavior.fly_eye_follow_rate_x,
                    behavior.fly_eye_follow_rate_y,
                    behavior.fly_eye_follow_rate_f,
                    behavior.fly_eye_follow_rate_b,
                ],
            ],
            look_at_rates: [
                [
                    behavior.look_at_follow_rate_x,
                    behavior.fly_look_at_follow_rate_y,
                    behavior.look_at_follow_rate_f,
                    behavior.look_at_follow_rate_b,
                ],
                [
                    behavior.fly_look_at_follow_rate_x,
                    behavior.fly_look_at_follow_rate_y,
                    behavior.fly_look_at_follow_rate_f,
                    behavior.fly_look_at_follow_rate_b,
                ],
            ],
            rate_blend_secs: f32::from(behavior.cam_follow_rate_interpolate_frame) / TICK_RATE,
            eye_rate: [Blend::default(); 4],
            look_at_rate: [Blend::default(); 4],
            approach,
            down_rate,
            eye_forward: behavior.eye_forward_length,
            at_forward: behavior.at_forward_length,
            pitch: 0.0,
            pitch_vel: 0.0,
            yaw_off: 0.0,
            cam_yaw: 0.0,
            hud_yaw_lead: 0.0,
            waist: Vec3::ZERO,
            tilt_speeds: (behavior.z_tilt_start_x_speed, behavior.z_tilt_max_x_speed),
            tilt_max: behavior.max_z_tilt_angle.to_radians(),
            tilt_secs: (
                f32::from(behavior.z_tilt_interpolate_frame) / TICK_RATE,
                f32::from(behavior.z_tilt_end_interpolate_frame) / TICK_RATE,
            ),
            tilt: Blend::default(),
            behavior,
            blur: 0.0,
            blur_accum: 0.0,
            shown: None,
        }
    }

    /// Advances speed blur at the 360's 30 Hz `CamFx_update` rate (`1 / 30` s per `0x82bfa9f0`
    /// ease step; Xenia probe `private/xenia/blur_rate.txt`).
    fn blur_step(&mut self, velocity: Vec3, dt: f32) {
        self.blur_accum = (self.blur_accum + dt).min(5.0 / BLUR_RATE);
        while self.blur_accum >= 1.0 / BLUR_RATE {
            self.blur_accum -= 1.0 / BLUR_RATE;
            self.blur_tick(velocity);
        }
    }

    /// One 30 Hz tick of the speed blur (360 0x82bfa9f0 / 0x82bf9c98): the horizontal and vertical
    /// speeds (km/h) each give (speed - BlurStartSpeed) / (BlurMaxSpeed - BlurStartSpeed) above
    /// the start; their sum, clamped to 0..1 and scaled by the camera action's BlurRate, is
    /// approached by BlurIntpRate of the gap, and the intensity stays within 0..1.
    fn blur_tick(&mut self, velocity: Vec3) {
        let b = self.behavior;
        let ramp = |speed: f32, start: f32, max: f32| {
            if speed > start && start < max {
                (speed - start) / (max - start)
            } else {
                0.0
            }
        };
        let h = ramp(
            velocity.with_y(0.0).length() * TICK_TO_KMH,
            b.blur_start_speed,
            b.blur_max_speed,
        );
        let v = ramp(
            velocity.y.abs() * TICK_TO_KMH,
            b.blur_start_speed_v,
            b.blur_max_speed_v,
        );
        let rate = self.action.map_or(0.0, |(_, a)| a.blur_rate);
        let target = (h + v).clamp(0.0, 1.0) * rate;
        self.blur = (self.blur + (target - self.blur) * b.blur_intp_rate).clamp(0.0, 1.0);
    }

    /// Applies the zoom-blur filter for this intensity (360 0x82bfa9f0, 0x82bf8c20); the filter
    /// skips drawing at alpha 9 or less (360 0x82c98368). Preserves `god_ray` and `tonemap`.
    fn apply_zoom_blur(&self, blur: &mut ZoomBlur) {
        let b = self.behavior;
        let i = self.blur;
        let alpha = ((b.blur_alpha as f32 * i) as i32).clamp(0, 255) as u8;
        let alpha = if alpha > BLUR_MIN_ALPHA { alpha } else { 0 };
        blur.set_speed_blur(
            b.blur_offset * i,
            alpha,
            b.blur_thin_pow * i,
            Vec2::new(b.blur_no_effect_size_x, b.blur_no_effect_size_y),
        );
    }

    /// Rotates the eased camera points about pivot by delta (yaw), so freelook turns the view
    /// rigidly instead of lagging behind it; only movement is left to the follow easing.
    fn swing(&mut self, pivot: Vec3, delta: f32) {
        if let Some(s) = self.shown.as_mut() {
            let r = Quat::from_rotation_y(delta);
            for v in [
                &mut s.eye,
                &mut s.look_at,
                &mut s.eye_target,
                &mut s.look_at_target,
            ] {
                *v = pivot + r * (*v - pivot);
            }
        }
    }

    /// Camera roll this frame from the AC's velocity (m/tick) along its local -X.
    fn roll(&mut self, velocity: Vec3, rotation: Quat, dt: f32) -> f32 {
        let side = -(rotation * Vec3::X).dot(velocity) * TICK_TO_KMH;
        let (start, max) = self.tilt_speeds;
        let (target, secs) = if side.abs() <= start || start >= max {
            (0.0, self.tilt_secs.1)
        } else {
            (
                ((side.abs() - start) / (max - start))
                    .min(1.0)
                    .copysign(side),
                self.tilt_secs.0,
            )
        };
        self.tilt_max * self.tilt.toward(target, secs, dt)
    }

    /// One 60 Hz tick of the pitch spring. `look` is command float 8, in [-1, 1].
    fn aim(&mut self, look: f32) {
        self.pitch_vel = self.pitch_vel * (1.0 - PITCH_DAMP) + look * PITCH_ACC;
        self.pitch += self.pitch_vel;
        if !(-PITCH_LIMIT..=PITCH_LIMIT).contains(&self.pitch) {
            self.pitch = self.pitch.clamp(-PITCH_LIMIT, PITCH_LIMIT);
            self.pitch_vel = 0.0;
        }
    }

    /// Eye and look-at from the pitched base: look-up uses H, look-down uses `d`. The eye stays
    /// WaterStopYOffset above the `water` under the AC (360 0x8285fe18 / 0x8285cfe0).
    fn place(&self, waist: Vec3, yaw: Quat, water: Option<f32>) -> (Vec3, Vec3) {
        let base = yaw * Quat::from_rotation_x(self.pitch);
        let p = waist + Vec3::Y * self.look_at_height + base * self.look_at_offset;
        let f = base * FORWARD;
        let dist = (self.eye_dist.value + self.eye_dist_offset).max(self.eye_min_dist);
        let (mut eye, look_at) = if f.y >= 0.0 {
            let flat = Vec3::new(f.x, 0.0, f.z).normalize_or_zero();
            let t = 1.0 - self.approach;
            let h = Vec3::new(
                f.x + (flat.x - f.x) * t,
                self.down_rate * f.y,
                f.z + (flat.z - f.z) * t,
            );
            let eye = p - h * dist;
            (eye, eye + f * dist)
        } else {
            let frac = (-f.normalize_or_zero().y.asin()) / std::f32::consts::FRAC_PI_2;
            let d = Vec3::new(f.x, 0.0, f.z).normalize_or_zero() * frac;
            (p - f * dist + d * self.eye_forward, p + d * self.at_forward)
        };
        let look_at = eye + (look_at - eye).normalize_or(FORWARD) * LOOK_AT_FAR;
        eye += base * Vec3::X * (self.shift.value * self.side.value * self.shift_rate);
        eye.y += self.lift.value * self.lift_rate + self.eye_height;
        if let Some(y) = water {
            eye.y = eye.y.max(y + self.water_stop);
        }
        (eye, look_at)
    }

    /// Switches to the `accameraaction` row of `action` when there is one (360 0x82860100): the
    /// old row's FOV fades over FovFadeFrame, the new one rises to FovMaxAngle (or `fov_override`
    /// from controller slot `0x14` `0x8285e418` when ready-position zoomed) over FovMaxFrame and
    /// holds for FovKeepFrame (-1 = for good).
    fn act(&mut self, action: u32, fov_override: Option<f32>) {
        if self.action.is_some_and(|(id, _)| id == action) && self.fov_override == fov_override {
            return;
        }
        let Some(row) = find(PARAM_ACCAMERAACTION_BIN, action).map(|r| &r.data) else {
            return;
        };
        debug!(
            "camera action {action}: eye follow {:?}, eye distance {}, fade-in {}, fov_override {:?}",
            [
                row.eye_follow_rate_x,
                row.eye_follow_rate_y,
                row.eye_follow_rate_f,
                row.eye_follow_rate_b
            ],
            row.eye_distance,
            row.eye_fade_in_frame,
            fov_override
        );
        let secs = |frames: i16| f32::from(frames) / TICK_RATE;
        if let Some((_, old)) = self.action.filter(|(_, old)| old.fov_enable != 0) {
            self.fov.fade(secs(old.fov_fade_frame));
        }
        if row.fov_enable != 0 {
            let max_angle = fov_override
                .filter(|&f| f > 0.0)
                .unwrap_or_else(|| f32::from(row.fov_max_angle));
            let to = max_angle - self.base_fov;
            self.fov.start(
                to,
                secs(row.fov_max_frame),
                secs(row.fov_keep_frame),
                secs(row.fov_fade_frame),
            );
        }
        self.action = Some((action, row));
        self.fov_override = fov_override;
    }

    /// Eases the eye distance and lift toward the camera action's (360 eye blender 0x8285f1a8:
    /// targets 0x8285d7f8 / 0x8285d8c8 over EyeFadeInFrame, set outright on its first update).
    fn blend_eye(&mut self, turn: f32, dt: f32) {
        let a = self.action.map(|(_, a)| a);
        let dist = a
            .map(|a| a.eye_distance)
            .filter(|&d| d > ACTION_DIST_MIN)
            .unwrap_or(self.behavior.eye_dist);
        let lift = a.map_or(0.0, |a| a.eye_offset_y);
        let shift = a.map_or(0.0, |a| a.eye_offset_x);
        let secs = a.map_or(0.0, |a| f32::from(a.eye_fade_in_frame) / TICK_RATE);
        let side_secs = a.map_or(NO_ACTION_SIDE_FRAMES, |a| {
            f32::from(a.turn_offset_move_frame)
        }) / TICK_RATE;
        if a.is_some_and(|a| a.b_trun_move_enable != 0) && turn != 0.0 {
            self.side_sign = -turn.signum();
        }
        if self.shown.is_none() {
            let set = |v: f32| Blend {
                value: v,
                to: v,
                ..Blend::default()
            };
            (self.eye_dist, self.lift, self.shift, self.side) =
                (set(dist), set(lift), set(shift), set(self.side_sign));
        }
        self.eye_dist.toward(dist, secs, dt);
        self.lift.toward(lift, secs, dt);
        self.shift.toward(shift, secs, dt);
        self.side.toward(self.side_sign, side_secs, dt);
    }

    /// Vertical field of view this frame, in radians.
    fn fov(&mut self, dt: f32) -> f32 {
        (self.base_fov + self.fov.step(dt)).to_radians()
    }

    /// Camera shake (`CamFx_shake` 360 `0x82bfab38`): sums movement shake, impact shake
    /// (`0x8285cf88` msg 0x68, `0x82bfabdc..0x82bfad08`), footstep shake (`0x8285cfb0` msg 0x69,
    /// `0x82bfad08..0x82bfadd4`), and timed impulse/rumble shake (`0x82bfaea0` -> `0x82bf8f00`).
    fn shake(&mut self, dt: f32) -> Vec2 {
        let mut out = Vec2::ZERO;
        // 1. Movement shake (`0x82bfab70..0x82bfabdc`).
        let weight = self.action.map_or(0.0, |(_, a)| {
            if a.shake_accel_min < a.shake_accel_max {
                let t = (self.accel - a.shake_accel_min) / (a.shake_accel_max - a.shake_accel_min);
                (t * a.shake_accel_rate + a.shake_no_move_rate).clamp(0.0, 1.0)
            } else {
                a.shake_no_move_rate
            }
        });
        if weight > 0.0 {
            self.shake_phase += dt;
            out += shake_wave(self.shake_phase, self.shake_ampl, self.shake_period) * weight;
        } else {
            self.shake_phase = 0.0;
        }

        let b = self.behavior;

        // 2. Impact shake (`0x82bfabdc..0x82bfad08`): timer = ShakeImpactTime * clamp((v - Min) / (Max - Min), 0, 1).
        let impact = std::mem::take(&mut self.impact_pending);
        if impact > 0.0
            && b.shake_impact_time > 0.0
            && b.shake_impact_accel_min < b.shake_impact_accel_max
        {
            let t = ((impact - b.shake_impact_accel_min)
                / (b.shake_impact_accel_max - b.shake_impact_accel_min))
                .clamp(0.0, 1.0);
            self.impact_timer = self.impact_timer.max(b.shake_impact_time * t);
        }
        if self.impact_timer > 0.0 && b.shake_impact_time > 0.0 {
            self.impact_timer = (self.impact_timer - dt).max(0.0);
            let ratio = (self.impact_timer / b.shake_impact_time).clamp(0.0, 1.0);
            let elapsed = b.shake_impact_time - self.impact_timer;
            out += shake_wave(
                elapsed,
                Vec2::new(b.shake_impact_ampl_x, b.shake_impact_ampl_y),
                Vec2::new(b.shake_impact_freq_x, b.shake_impact_freq_y),
            ) * ratio;
        }

        // 3. Footstep shake (`0x82bfad08..0x82bfadd4`): runs down over ShakeFootStepTime.
        if std::mem::take(&mut self.footstep_pending) && b.shake_foot_step_time > 0.0 {
            self.footstep_timer = b.shake_foot_step_time;
        }
        if self.footstep_timer > 0.0 && b.shake_foot_step_time > 0.0 {
            self.footstep_timer = (self.footstep_timer - dt).max(0.0);
            let ratio = (self.footstep_timer / b.shake_foot_step_time).clamp(0.0, 1.0);
            let elapsed = b.shake_foot_step_time - self.footstep_timer;
            out += shake_wave(
                elapsed,
                Vec2::new(b.shake_foot_step_ampl_x, b.shake_foot_step_ampl_y),
                Vec2::new(b.shake_foot_step_freq_x, b.shake_foot_step_freq_y),
            ) * ratio;
        }

        // 4. Timed impulse / rumble shake (`0x82bfaea0` -> `0x82bf8f00`).
        if self.rumble_duration > 0.0 {
            self.rumble_elapsed += dt;
            if self.rumble_elapsed > self.rumble_duration {
                self.rumble_duration = 0.0;
                self.rumble_elapsed = 0.0;
            } else {
                let ratio = (1.0 - self.rumble_elapsed / self.rumble_duration).clamp(0.0, 1.0);
                out += shake_wave(self.rumble_elapsed, self.rumble_ampl, self.rumble_freq) * ratio;
            }
        }

        out
    }

    /// The shown eye and look-at for this frame's targets.
    fn follow(
        &mut self,
        eye: Vec3,
        look_at: Vec3,
        frame: Quat,
        airborne: bool,
        dt: f32,
    ) -> (Vec3, Vec3) {
        let air = usize::from(airborne);
        let secs = self.rate_blend_secs;
        let action = self.action.map(|(_, a)| {
            [
                a.eye_follow_rate_x,
                a.eye_follow_rate_y,
                a.eye_follow_rate_f,
                a.eye_follow_rate_b,
            ]
        });
        let eye_rates: [f32; 4] = std::array::from_fn(|i| {
            let target = action.map_or(0.0, |a| a[i]);
            let target = if target < ACTION_RATE_MIN {
                self.eye_rates[air][i]
            } else {
                target
            };
            self.eye_rate[i].toward(target, secs, dt)
        });
        let look_at_rates: [f32; 4] = std::array::from_fn(|i| {
            self.look_at_rate[i].toward(self.look_at_rates[air][i], secs, dt)
        });
        let push = self.shake(dt);
        let shown = match self.shown {
            None => Shown {
                eye,
                look_at,
                eye_target: eye,
                look_at_target: look_at,
            },
            Some(s) => Shown {
                eye: ease(s.eye, s.eye_target, eye, eye_rates, push, frame, dt),
                look_at: ease(
                    s.look_at,
                    s.look_at_target,
                    look_at,
                    look_at_rates,
                    push,
                    frame,
                    dt,
                ),
                eye_target: eye,
                look_at_target: look_at,
            },
        };
        self.shown = Some(shown);
        (shown.eye, shown.look_at)
    }
}

impl Pilot {
    /// The locomotion state playing, and the hokan block its legs select (`motioncollate`
    /// `HokanParamID`: 0, or 500 for tanks).
    pub(crate) fn pose(&self) -> (Option<(&'static str, Option<u8>)>, u32) {
        (self.state, self.hokan_base)
    }

    /// Whether a High Boost / Boost Charge (`HighBoostState`, `0x8200d378`) is active.
    pub fn in_quick_boost(&self) -> bool {
        self.qb.is_some()
    }

    /// Whether `WallJumpState` (`0x82092a20`) is in its Phase 1 acceleration burst
    /// (`AcMove_wallJump` `0x82824560`, which lights booster SFX mode 4 at `wall_jump_sfx_scale`).
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

    /// Starts Boost Charge Kick (`AcActCtrl_startKick` `0x8283fdb0` → `0x8281f278` EN check/drain
    /// of `qb_en_drain` = `BST__HI_EN * 0.7`, `KickState` `0x8283f8a8`).
    pub fn start_kick(&mut self) -> bool {
        if self.kick.is_some() || self.blade.is_some() || self.wj.is_some() {
            return false;
        }
        if !self.try_drain_en(self.ctrl.qb_en_drain) {
            return false;
        }
        let dir = Quat::from_rotation_y(self.yaw + self.cam.yaw_off) * FORWARD;
        let dir = Vec3::new(dir.x, 0.0, dir.z).normalize_or(FORWARD);
        self.qb = None;
        self.glide = false;
        self.kicks_started += 1;
        // Carry at least kick_max_tick forward so a standalone kick or low-speed entry immediately
        // reaches charging speed, while preserving higher High Boost / Glide Boost entry speed.
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

    /// Stops and rebounds the AC when Boost Charge Kick strikes a target (`0x828433b8` → `0x8283cef8`).
    pub fn on_kick_contact(&mut self) {
        if let Some(kick) = self.kick.take() {
            self.kicks_landed += 1;
            self.velocity.x = -kick.dir.x * (self.ctrl.walk_max_tick * 0.5);
            self.velocity.z = -kick.dir.z * (self.ctrl.walk_max_tick * 0.5);
            self.trigger_impact_shake(250.0);
            self.trigger_rumble_shake(0.22, Vec2::new(0.14, 0.22), Vec2::new(0.02, 0.02));
        }
    }

    /// Queues an impact camera shake (360 message `0x68` handler `0x8285cf88` -> `CamFx_shake`
    /// `0x82bfabdc`). `accel_kmh` is compared against `ShakeImpactAccelMin..Max` (`150..250`).
    pub fn trigger_impact_shake(&mut self, accel_kmh: f32) {
        if accel_kmh > self.cam.impact_pending {
            self.cam.impact_pending = accel_kmh;
        }
    }

    /// Queues a footstep camera shake (360 message `0x69` handler `0x8285cfb0` -> `CamFx_shake`
    /// `0x82bfad08`, row 0: `0.15 m` vertical over `0.1 s`).
    pub fn trigger_footstep_shake(&mut self) {
        self.cam.footstep_pending = true;
    }

    /// Triggers a timed impulse/rumble shake (`CamFx_shake` `0x82bfaea0` -> `0x82bf8f00`) for
    /// heavy weapon recoil, Overed Weapon fire, kick impact, or stagger hits.
    pub fn trigger_rumble_shake(&mut self, duration: f32, ampl: Vec2, freq: Vec2) {
        if duration <= 0.0 {
            return;
        }
        let cur_strength = if self.cam.rumble_duration > 0.0 {
            let rem = (1.0 - self.cam.rumble_elapsed / self.cam.rumble_duration).clamp(0.0, 1.0);
            self.cam.rumble_ampl.length() * rem
        } else {
            0.0
        };
        if ampl.length() >= cur_strength {
            self.cam.rumble_duration = duration;
            self.cam.rumble_elapsed = 0.0;
            self.cam.rumble_ampl = ampl;
            self.cam.rumble_freq = freq;
        }
    }

    /// Starts a melee blade attack (`BladeState` `0x83714024`, `0x8283ec20` → `0x828ad698`).
    pub fn start_blade(&mut self, launch: BladeLaunch) -> bool {
        if self.blade.is_some() || self.kick.is_some() || self.wj.is_some() {
            return false;
        }
        self.qb = None;
        self.glide = false;
        let entry_speed_tick = Vec2::new(self.velocity.x, self.velocity.z).length();
        // `0x828ad510`: dash duration = `blade_homing_time_speed_coef * entry_speed * 0.01 + blade_dash_max_frames / 60`
        // (with a responsive homing window when FCS-locked onto a target outside instant-swing range).
        let speed_bonus_ticks =
            NEW_AC_BEHAVIOR.blade_homing_time_speed_coef * (entry_speed_tick * TICK_TO_KMH) * 0.01 * TICK_RATE;
        let max_dash_ticks = if launch.target.is_some() {
            (NEW_AC_BEHAVIOR.blade_dash_max_frames + speed_bonus_ticks).clamp(18.0, 45.0)
        } else {
            NEW_AC_BEHAVIOR.blade_dash_max_frames
        };
        let target_pos = launch.target.map(|(_, pos)| pos);
        let downward = self.airborne
            && self.cam.pitch.to_degrees() <= -f32::from(NEW_AC_BEHAVIOR.blade_down_angle_deg);
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

    /// Launches an Overed Weapon attack dash burst (`0x82869d40` -> `AcMove +0x124/+0x128`,
    /// `NewAcBehavior` field 234 `ow_attack_accel_mult` = 1.5, field 235 `ow_attack_dash_frames` = 30).
    pub fn start_ow_dash(&mut self, dir: Vec3) {
        let horiz = Vec3::new(dir.x, 0.0, dir.z).normalize_or_zero();
        let dir = if horiz != Vec3::ZERO {
            horiz
        } else {
            Quat::from_rotation_y(self.yaw) * FORWARD
        };
        let frames = (acvd_data::generated::tuning::NEW_AC_BEHAVIOR.ow_attack_dash_frames as f32)
            .max(1.0);
        self.ow_dash_ticks = frames;
        self.ow_dash_dir = dir;
        self.qb = None;
        self.glide = false;
    }

    /// A standing AC moving by `ctrl`, with the default follow camera for its legs' motion set.
    pub fn new(ctrl: AcCtrlParam, legs_motion_id: u8) -> Self {
        let en_max = ctrl.en_capacity.max(0.0);
        Self {
            ctrl,
            en: en_max,
            en_max,
            velocity: Vec3::ZERO,
            yaw: 0.0,
            boost: false,
            glide: false,
            airborne: false,
            scan_mode: false,
            ow_active: false,
            ow_lockout: false,
            ow_dash_ticks: 0.0,
            ow_dash_dir: FORWARD,
            area_zone: crate::map::AreaZone::Inside,
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
            ready_cam: ReadyCamera::Off,
            state: None,
            suppress_locomotion: false,
            clip_ok: false,
            accumulator: 0.0,
            hokan_base: collate(legs_motion_id).map_or(0, |r| r.data.hokan_param_id.into()),
            cam: FollowCam::of(legs_motion_id),
        }
    }

    /// Crossfade times for an `acmotion` interpolate id, added to the legs' hokan block.
    pub(crate) fn fade(&self, interpolate_id: u8) -> [f32; 8] {
        let hokan = self.hokan_base + u32::from(interpolate_id);
        find(PARAM_ACANIMHOKAN_BIN, hokan)
            .map(|h| Fade::of(&h.data))
            .unwrap_or([0.0; 8])
    }

    /// While held, the locomotion state does not replace the clip. Clearing it replays the
    /// state on the next step.
    pub(crate) fn body_held(&mut self, hold: bool) {
        if self.suppress_locomotion && !hold {
            self.state = None;
        }
        self.suppress_locomotion = hold;
    }

    /// Attempts to spend `amount` EN (`360 0x82830910(..., is_passive = 0)` via `0x828b2648` /
    /// `0x828a1820`). Rejects without draining when `en < amount` (Xenia probe
    /// `private/xenia/en_drain2.jsonl`).
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

    /// Fire direction: the follow camera's pitched forward (`FollowCam::place`); core aim is not
    /// posed yet.
    pub fn aim_direction(&self) -> Vec3 {
        Quat::from_rotation_y(self.yaw + self.cam.yaw_off)
            * Quat::from_rotation_x(self.cam.pitch)
            * FORWARD
    }

    /// Lock-Sight 3D projection direction (`0x829d2120`): `aim_direction` plus mouselook's
    /// horizontal follow-rate lead (`hud_yaw_lead`, 0 outside mouselook).
    pub fn sight_direction(&self) -> Vec3 {
        Quat::from_rotation_y(self.yaw + self.cam.yaw_off + self.cam.hud_yaw_lead)
            * Quat::from_rotation_x(self.cam.pitch)
            * FORWARD
    }

    /// Current camera aim angles `(yaw, pitch)` in radians (`yaw + yaw_off`, `pitch`), matching
    /// the 360 HUD status block `+0x24` / `+0x20` (`0x829d1df0`).
    pub fn aim_angles(&self) -> (f32, f32) {
        (self.yaw + self.cam.yaw_off, self.cam.pitch)
    }

    /// 3D Lock-Sight datum origin in world space (`0x8287cdd0` -> `0x8260f878`), matching
    /// `FollowCam::place`'s waist + `look_at_height`.
    pub fn sight_origin(&self, root: Vec3) -> Vec3 {
        root + self.cam.waist + Vec3::Y * self.cam.look_at_height
    }

    /// Current follow-camera roll in radians (`FollowCam::roll`, `0x82bfa708`).
    pub fn camera_roll(&self) -> f32 {
        self.cam.tilt_max * self.cam.tilt.value
    }

    pub fn set_pitch(&mut self, pitch: f32) {
        self.cam.pitch = pitch;
    }
}

/// Mouselook (M): camera yaw / pitch radians per pixel of mouse motion.
const MOUSE_YAW: f32 = 0.003;
const MOUSE_PITCH: f32 = 0.003;

#[derive(Default)]
struct Input {
    /// x: right, y: forward, length at most 1.
    stick: Vec2,
    /// Positive turns left.
    turn: f32,
    /// Command float 8: stick Y in [-1, 1], positive looks up.
    look: f32,
    jump: bool,
    boost_toggle: bool,
    glide: bool,
    quick_boost: bool,
    qb_held: bool,
    kick: bool,
}

fn read_input(keys: &ButtonInput<KeyCode>, held: &[KeyCode], pads: &Query<&Gamepad>) -> Input {
    let down = |k: KeyCode| keys.pressed(k) || held.contains(&k);
    let axis = |pos: KeyCode, neg: KeyCode| {
        f32::from(u8::from(down(pos))) - f32::from(u8::from(down(neg)))
    };
    let mut stick = Vec2::new(
        axis(KeyCode::KeyD, KeyCode::KeyA),
        axis(KeyCode::KeyW, KeyCode::KeyS),
    );
    let mut turn = axis(KeyCode::KeyQ, KeyCode::KeyE);
    let mut look = axis(KeyCode::ArrowUp, KeyCode::ArrowDown);
    let mut jump = keys.just_pressed(KeyCode::Space) || held.contains(&KeyCode::Space);
    let mut boost_toggle = keys.just_pressed(KeyCode::KeyV);
    let mut glide = keys.just_pressed(KeyCode::ControlLeft) || held.contains(&KeyCode::ControlLeft);
    let mut quick_boost = keys.just_pressed(KeyCode::ShiftLeft)
        || keys.just_pressed(KeyCode::ShiftRight)
        || held.contains(&KeyCode::ShiftLeft)
        || held.contains(&KeyCode::ShiftRight);
    let mut qb_held = down(KeyCode::ShiftLeft) || down(KeyCode::ShiftRight);
    let kick = keys.just_pressed(KeyCode::KeyK) || held.contains(&KeyCode::KeyK);
    for pad in pads {
        let left = pad.left_stick();
        if left.length() > 0.2 {
            stick += left;
        }
        let right = pad.right_stick();
        if right.x.abs() > 0.2 {
            turn -= right.x;
        }
        if right.y.abs() > 0.2 {
            look += right.y;
        }
        jump |= pad.just_pressed(GamepadButton::South);
        boost_toggle |= pad.just_pressed(GamepadButton::LeftTrigger);
        glide |= pad.just_pressed(GamepadButton::LeftThumb);
        quick_boost |= pad.just_pressed(GamepadButton::West);
        qb_held |= pad.pressed(GamepadButton::West);
    }
    Input {
        stick: stick.clamp_length_max(1.0),
        turn: turn.clamp(-1.0, 1.0),
        look: look.clamp(-1.0, 1.0),
        jump,
        boost_toggle,
        glide,
        quick_boost,
        qb_held,
        kick,
    }
}

/// `stick` (camera-relative) turned into the body's frame, for the directional clips.
fn body_stick(stick: Vec2, yaw_off: f32) -> Vec2 {
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
fn start_quick_boost(p: &mut Pilot) -> bool {
    if p.qb.is_some() || p.kick.is_some() || p.blade.is_some() || p.qb_cooldown >= 1.0 {
        return false;
    }
    if !p.try_drain_en(p.ctrl.qb_en_drain) {
        p.qb_shortages += 1;
        p.qb_airborne = p.airborne;
        return false;
    }
    p.qb_cooldown = p.ctrl.qb_reload_ticks;
    p.glide = false;
    p.qb_charge_starts += 1;
    p.qb_airborne = p.airborne;
    p.last_qb_tae = high_boost_tae_id(false, false, p.airborne);
    p.qb = Some(QbPhase::Charge { held_ticks: 0.0 });
    true
}

/// Evaluates the Boost Charge curve at `hold_ratio` (`0..=1`) and enters `HighBoostState` Phase 1
/// (`360 0x8237c1b8`, `0x8237c2dc..0x8237c364`). Does not force boost mode on (`private/xenia/quick_boost.jsonl` Hit 1).
fn fire_quick_boost(p: &mut Pilot, stick: Vec2, hold_ratio: f32) {
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
    let forward = Quat::from_rotation_y(p.yaw + p.cam.yaw_off) * FORWARD;
    let world = forward.cross(Vec3::Y) * stick.x + forward * stick.y;
    let eighth = direction(body_stick(stick, p.cam.yaw_off));
    p.glide = false;
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
    let qb_max = c.quick_boost_min_tick + (c.quick_boost_max_tick - c.quick_boost_min_tick) * w1;
    debug!(
        "high boost (hold {:.2}, w1 {:.3}, w2 {:.3}, 2nd={}): target {:.3} m/tick ({:.1} km/h), p1 {:.2}f, p2 {:.2}f",
        x,
        w1,
        w2,
        p.second_stage_qb,
        qb_max,
        qb_max * TICK_TO_KMH,
        p1_ticks,
        p2_ticks
    );
}

/// Steps `HighBoostState` (`360 0x8237c5a0`: Phase 0 `0x8237c1b8`, Phase 1 `0x8237ba68`,
/// Phase 2 `0x8237bf28`) by one 60 Hz tick.
fn step_qb(p: &mut Pilot, input: &Input, moving: bool) -> Option<QbThrust> {
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
            // `0x8237bf28` -> `0x8237bcb0` -> `0x8283fdb0`: holding High Boost (`□ + HOLD`,
            // manual 20301/20401) into Phase 2 launches Boost Charge Kick (`KickState`).
            if input.qb_held && p.start_kick() {
                return None;
            }
            // Glide Boost cancels an active Phase 2 burst.
            if input.glide && p.boost && moving && !p.airborne && p.en >= p.ctrl.glide_en_tick {
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
fn direction(stick: Vec2) -> u8 {
    let angle = stick.x.atan2(stick.y).rem_euclid(std::f32::consts::TAU);
    ((angle / std::f32::consts::FRAC_PI_4).round() as u8) % 8
}

/// Travel eighth for the air-glide entry (`0x82825918`: 0 forward, clockwise). Velocity in the
/// body's frame, the stick when nearly still. `x` is right, `y` is forward, the same axes as
/// [`direction`].
fn travel_eighth(p: &Pilot, stick: Vec2) -> u8 {
    let forward = Quat::from_rotation_y(p.yaw) * FORWARD;
    let right = forward.cross(Vec3::Y);
    let v = Vec2::new(p.velocity.dot(right), p.velocity.dot(forward));
    direction(if v.length() > 0.02 { v } else { stick })
}

/// Glide clip while `glide` is held. On the ground: the start, then the ground wheel. In the
/// air: one directional entry (`glide_air_in`), then the air-float wheel (`glide_air`). Landing
/// returns to the ground wheel without replaying the start.
fn glide_clip(
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
/// - Stick obliquely into the wall -> horizontal `wall_normal` rotated by `±glance_rad` toward `wish`.
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
fn step_wall_jump(p: &mut Pilot, input: &Input, facing: Quat) -> Option<WallJumpThrust> {
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

/// Steps `KickState` (`360 0x820928d0`: `0x82844570` → `0x828433a0`, `AcMove_boostChargeKick`
/// `0x82823740`) by one 60 Hz tick. Returns `(horizontal, skip_gravity)` when active.
fn step_kick(p: &mut Pilot, horizontal: Vec2) -> Option<(Vec2, bool)> {
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
            horizontal.normalize_or(dir2) * (cur * (1.0 - c.over_max_decel_tick)).max(c.kick_max_tick)
        } else {
            dir2 * cur.max(c.kick_max_tick * 0.85)
        }
    };

    // Vertical thrust (`0x82823788..0x82823964`): downward dive when pitched down
    // (`NewAcBehavior +0x538` = 3.0 m/s per tick / 60), else slight lift capped at `qb_hop_max_tick`.
    let pitch = p.cam.pitch;
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
    p: &mut Pilot,
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
    let dist_to_target = b
        .target_pos
        .map(|t| (t - position).with_y(0.0).length());

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
                if b.max_dash_ticks > 0.0
                    && dist_to_target.is_some_and(|d| d > instant_dist)
                {
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
                    p.velocity.y = (p.velocity.y - c.quick_boost_v_acc_tick).max(-c.blade_rise_max_tick);
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
fn step_en(p: &mut Pilot) {
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

/// One 60 Hz tick of movement over the highest ground under the AC.
fn step(p: &mut Pilot, input: &Input, position: &mut Vec3, collision: &Collision) {
    let c = p.ctrl;
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
    let facing = Quat::from_rotation_y(p.yaw + p.cam.yaw_off);
    let forward = facing * FORWARD;
    let right = forward.cross(Vec3::Y);
    let wish3 = right * input.stick.x + forward * input.stick.y;
    let wish = Vec2::new(wish3.x, wish3.z);
    let mut horizontal = Vec2::new(p.velocity.x, p.velocity.z);
    let speed = horizontal.length();
    let moving = wish != Vec2::ZERO;
    // A glide starts on the ground (`pilot`) but carries on over a drop.
    p.glide &= p.boost && moving && p.qb.is_none() && p.wj.is_none() && p.kick.is_none() && p.blade.is_none();
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

    // Glide Boost -> Boost Charge Kick (`0x82840ee0` -> `0x8283fdb0`), or direct Kick key.
    if (p.glide && input.qb_held) || input.kick {
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
            p.velocity.y = (p.velocity.y + c.quick_boost_v_acc_tick * qb.scale).min(c.qb_hop_max_tick);
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
        // Glide Boost (`0x8281ea30` / `0x8281e9d4`) drains `glide_en_tick` (`BST__HI_EN / 60`)
        // each tick and ends immediately when `en < glide_en_tick` (`private/xenia/en_drain2.jsonl`).
        if p.glide && !p.try_drain_en(c.glide_en_tick) {
            p.glide = false;
        }
        if !p.glide && p.boost && moving {
            let _ = p.try_drain_en(c.boost_ep_tick);
        }
        // After a high boost ends, speed sits above the boost/walk max; `integrate` keeps steering
        // with the stick while speed decays by `over_max_decel_tick` per tick.
        let (max, accel) = if p.glide {
            (c.glide_max_tick, c.glide_acc_tick)
        } else if p.boost {
            (c.boost_max_tick, c.boost_acc_tick)
        } else {
            (c.walk_max_tick, c.walk_acc_tick)
        };
        horizontal = if p.glide {
            approach(
                horizontal,
                wish * max,
                if speed <= max { accel } else { brake },
            )
        } else if moving && !p.airborne {
            integrate(horizontal, wish * accel, max, c.over_max_decel_tick)
        } else if !p.airborne {
            approach(horizontal, Vec2::ZERO, brake)
        } else if p.boost && moving {
            integrate(horizontal, wish * c.boost_acc_tick, c.boost_max_tick, c.over_max_decel_tick)
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
    if p.jump_queued && !p.airborne && p.qb.is_none() && p.wj.is_none() && p.kick.is_none() && p.blade.is_none() {
        (p.airborne, p.takeoff, p.boost, p.glide) = (true, c.jump_frames, true, false);
        // `0x8283d4b8`: entering Jump (state 3) sets `action_ctrl +0x114` to
        // `NewAcBehavior +0x4ae` (10 ticks), preventing immediate wall kick off the ground.
        p.wj_lockout = c.wall_jump_post_jump_lockout;
    }
    p.jump_queued = false;

    if p.airborne && !qb_hopped && !wj_accelerated && !melee_skip_gravity {
        if p.takeoff > 0.0 {
            p.velocity.y += c.jump_accel_tick * p.takeoff.min(1.0);
            p.takeoff = (p.takeoff - 1.0).max(0.0);
        } else if p.glide {
            // The vertical update 0x82820168 skips the gravity loop (0x8281f960) in the glide
            // states 4/5 (0x8281f3d0 at 0x828201b4): the vertical speed holds. 0x8281dd28 only
            // caps rising (block +0x288), which a glide off the ground never does.
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
    let before_y = position.y;
    *position += p.velocity;
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
    // tick's travel (slopes) or 5 cm when still — a hit farther down than that is a drop.
    // Airborne, the ray starts at last tick's height so a fast fall cannot step past a floor.
    let lift = if p.airborne {
        (before_y - position.y).max(0.0)
    } else {
        horizontal.length().max(0.05)
    };
    let ground = collision.ground_below_with_surface(Vec3::new(
        position.x,
        position.y + lift,
        position.z,
    ));
    if p.airborne {
        if let Some((g, surf)) = ground.filter(|&(g, _)| position.y <= g && p.velocity.y <= 0.0) {
            let impact_kmh = p.velocity.y.abs() * TICK_TO_KMH;
            p.landing_impact_kmh = p.landing_impact_kmh.max(impact_kmh);
            p.ground_surface = surf;
            p.landings += 1;
            (position.y, p.velocity.y, p.airborne, p.landed, p.wj) = (g, 0.0, false, true, None);
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
    step_en(p);
}

/// The state this frame's movement shows, given the one playing.
fn state(p: &Pilot, input: &Input, motion: &Motion) -> (&'static str, Option<u8>) {
    let moving = input.stick != Vec2::ZERO;
    let dir = moving.then(|| direction(input.stick));
    let current = p.state.unwrap_or(("idle", None));
    // Boost Charge Kick (`KickState` `0x820928d0`, `0x828433a0` -> `0x828845e8`): row 243, anim 90.
    if p.kick.is_some() {
        return ("kick", None);
    }
    // Melee Blade (`BladeState` `0x83714024`): laser blade rows 100-113, physical/pile rows 293-306.
    if let Some(b) = &p.blade {
        let right = b.launch.hand_idx == 0;
        let phys = b.launch.physical;
        let clip = match (phys, b.air_start, b.downward, b.phase, right) {
            (false, true, true, BladePhase::Swing | BladePhase::Recovery, true) => "blade_down_r",
            (false, true, true, BladePhase::Swing | BladePhase::Recovery, false) => "blade_down_l",
            (false, true, _, BladePhase::Charge | BladePhase::Dash, true) => "blade_air_charge_r",
            (false, true, _, BladePhase::Charge | BladePhase::Dash, false) => "blade_air_charge_l",
            (false, true, _, BladePhase::Swing | BladePhase::Recovery, true) => "blade_air_swing_r",
            (false, true, _, BladePhase::Swing | BladePhase::Recovery, false) => "blade_air_swing_l",
            (false, false, _, BladePhase::Charge | BladePhase::Dash, true) => "blade_charge_r",
            (false, false, _, BladePhase::Charge | BladePhase::Dash, false) => "blade_charge_l",
            (false, false, _, BladePhase::Swing | BladePhase::Recovery, true) => "blade_swing_r",
            (false, false, _, BladePhase::Swing | BladePhase::Recovery, false) => "blade_swing_l",
            (true, true, true, BladePhase::Swing | BladePhase::Recovery, true) => "phys_blade_down_r",
            (true, true, true, BladePhase::Swing | BladePhase::Recovery, false) => "phys_blade_down_l",
            (true, true, _, BladePhase::Charge | BladePhase::Dash, true) => "phys_blade_air_charge_r",
            (true, true, _, BladePhase::Charge | BladePhase::Dash, false) => "phys_blade_air_charge_l",
            (true, true, _, BladePhase::Swing | BladePhase::Recovery, true) => "phys_blade_air_swing_r",
            (true, true, _, BladePhase::Swing | BladePhase::Recovery, false) => "phys_blade_air_swing_l",
            (true, false, _, BladePhase::Charge | BladePhase::Dash, true) => "phys_blade_charge_r",
            (true, false, _, BladePhase::Charge | BladePhase::Dash, false) => "phys_blade_charge_l",
            (true, false, _, BladePhase::Swing | BladePhase::Recovery, true) => "phys_blade_swing_r",
            (true, false, _, BladePhase::Swing | BladePhase::Recovery, false) => "phys_blade_swing_l",
        };
        return (clip, None);
    }
    // Wall Jump (`WallJumpState` 0x82092a20): Phase 0 plays `wall_jump_prep` (rows 231/233/235/237,
    // `0x82884da8`), Phase 1/2 plays `wall_jump` (rows 232/234/236/238, `0x82884688`).
    if let Some(wj) = p.wj {
        return match wj {
            WallJumpPhase::Prep { clip_dir, .. } => ("wall_jump_prep", Some(clip_dir)),
            WallJumpPhase::Accel { clip_dir, .. } | WallJumpPhase::After { clip_dir, .. } => {
                ("wall_jump", Some(clip_dir))
            }
        };
    }
    // Boost Charge wind-up plays row 284; the Phase 1 -> Phase 2 transition triggers the
    // directional High Boost clip (rows 285-292), which plays out unless a new QB, glide, or jump
    // interrupts after `HighBoostState` ends.
    if let Some(d) = p.quick_pending {
        return ("quick_boost", Some(d));
    }
    if matches!(p.qb, Some(QbPhase::Charge { .. } | QbPhase::Phase1 { .. })) {
        return if current.0 == "quick_boost" && p.clip_ok && !motion.finished() {
            current
        } else {
            ("quick_boost_charge", None)
        };
    }
    if current.0 == "quick_boost"
        && p.clip_ok
        && !motion.finished()
        && !p.glide
        && p.takeoff <= 0.0
    {
        return current;
    }
    // On the ground a glide is the start, then the ground wheel. Off the ground it is the
    // directional air entry, then the air-float wheel (acmotion 223-230, then 222).
    if p.glide {
        return glide_clip(
            current,
            p.airborne,
            p.clip_ok,
            motion.finished(),
            travel_eighth(p, input.stick),
        );
    }
    // Take-off and touchdown play out unless input moves the AC on.
    let holding = matches!(current.0, "jump" | "land") && p.clip_ok && !motion.finished();
    if p.airborne {
        if current.0 == "jump" && holding {
            return current;
        }
        if p.takeoff > 0.0 {
            return ("jump", None);
        }
        return match dir {
            Some(d) => ("air_move", Some(d)),
            // A direction change lets the stick pass through zero for a frame. Keep the wheel
            // while the AC is still moving, or that frame cuts to rise/fall and the lean snaps.
            None if current.0 == "air_move"
                && p.boost
                && Vec2::new(p.velocity.x, p.velocity.z).length() > 0.05 =>
            {
                current
            }
            None if p.velocity.y > 0.0 => ("rise", None),
            None => ("fall", None),
        };
    }
    if p.landed && !moving {
        return ("land", None);
    }
    if current.0 == "land" && holding && !moving {
        return current;
    }
    match dir {
        Some(d) if p.boost => ("dash", Some(d)),
        Some(d) => ("walk", Some(d)),
        None if input.turn > 0.05 => ("turn_left", None),
        None if input.turn < -0.05 => ("turn_right", None),
        // Same as the air case: keyboard direction changes zero the stick for a frame.
        // Dropping to idle restarts the clip, which is the strafe twitch.
        None if current.0 == "dash"
            && p.boost
            && Vec2::new(p.velocity.x, p.velocity.z).length() > 0.05 =>
        {
            current
        }
        None => ("idle", None),
    }
}

/// Computes the Core Control target `(yaw, pitch)` in AC-local radians (360 `0x8289e730`).
///
/// Bevy AC local axes: forward is `-Z` (`FORWARD`), left is `-X`, up is `+Y`; positive yaw
/// turns left (`+Y` rotation) and positive pitch looks up (`+X` rotation).
/// When an FCS target is locked, aims the upper body directly at `locked_pos`; otherwise tracks
/// camera aim (`cam_yaw_off`, `cam_pitch`) plus turn lead (`turn`) and sideways strafe
/// counter-twist (`strafe_x`) so the torso stays squared to the aim while legs strafe/dash.
pub fn core_aim_angles(
    body_yaw: f32,
    ac_pos: Vec3,
    cam_yaw_off: f32,
    cam_pitch: f32,
    strafe_x: f32,
    turn: f32,
    locked_pos: Option<Vec3>,
) -> (f32, f32) {
    if let Some(target) = locked_pos {
        let to_target = target - (ac_pos + Vec3::Y * 5.0);
        if to_target.length_squared() > 1e-4 {
            let local = Quat::from_rotation_y(-body_yaw) * to_target;
            let yaw = (-local.x).atan2(-local.z);
            let horiz = Vec2::new(local.x, local.z).length().max(1e-4);
            let pitch = local.y.atan2(horiz);
            return (yaw, pitch);
        }
    }
    let strafe_blend = 1.0 - (cam_yaw_off.abs() / 0.25).min(1.0);
    let strafe_counter = strafe_x.clamp(-1.0, 1.0) * 18.0_f32.to_radians() * strafe_blend;
    let turn_lead = turn.clamp(-1.0, 1.0) * 26.0_f32.to_radians();
    (cam_yaw_off + strafe_counter + turn_lead, cam_pitch)
}

/// Maps a locomotion state (`ac_states.csv` name + 8-way direction) to its `PARAM_ACMOTION_BIN`
/// row in the `motion_ow_equip` bank (`0x8284e9d8`, anims `201..248`).
pub fn ow_locomotion_row(name: &str, dir: Option<u8>) -> Option<u32> {
    match name {
        "stand" => Some(221), // anim 201: OW展開時 待機
        "walk" => {
            // Directions clockwise from forward: F, FR, R, BR, B, BL, L, FL.
            const WALK: [u32; 8] = [194, 198, 202, 208, 204, 206, 200, 196];
            WALK.get(dir.unwrap_or(0) as usize).copied()
        }
        "turn_left" => Some(248),  // anim 226
        "turn_right" => Some(249), // anim 227
        "dash" | "glide" => Some(268),     // anim 228: OW展開時 地上ブースト
        "air_move" | "glide_air" => Some(269), // anim 229: OW展開時 空中ブースト
        "jump" => Some(258), // anim 232
        "rise" => Some(259), // anim 233
        "fall" => Some(260), // anim 234
        "quick_boost" => match dir.unwrap_or(0) {
            0 | 1 | 7 => Some(254), // anim 245 front QB
            2 => Some(257),         // anim 248 right QB
            3 | 4 | 5 => Some(255), // anim 246 back QB
            _ => Some(256),         // anim 247 left QB
        },
        "kick" => Some(274), // anim 243
        _ => None,
    }
}

/// Checks if the AC at `pos` has dropped below `water_height` (`env::Scene.water_height` from
/// `m####_env.msb` sub-block 0 `+0x08`, 360 `0x827ffa80`, or `--water <y>`). When submerged,
/// teleports the AC to the nearest `POINT_WATER_RETURN` (`50`) point, zeroes velocity, and
/// clears active dash/air/melee states. Returns `true` if a water-return respawn occurred.
pub fn check_water_return(
    p: &mut Pilot,
    pos: &mut Vec3,
    water_return: &crate::map::WaterReturnPoints,
    fallback: crate::map::Start,
    collision: &Collision,
) -> bool {
    let Some(wy) = water_return.water_height else {
        return false;
    };
    if pos.y >= wy {
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
    p.cam.yaw_off = 0.0;
    true
}

#[derive(bevy::ecs::system::SystemParam)]
pub struct PilotWorld<'w> {
    collision: Res<'w, Collision>,
    area_bounds: Option<Res<'w, crate::map::AreaBounds>>,
    water_return: Option<Res<'w, crate::map::WaterReturnPoints>>,
    start: Option<Res<'w, crate::map::Start>>,
}

#[allow(clippy::too_many_arguments)]
pub fn pilot(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    held: Res<Held>,
    pads: Query<&Gamepad>,
    mut piloting: ResMut<Piloting>,
    garage: Res<crate::Garage>,
    world: PilotWorld,
    motion: Option<ResMut<Motion>>,
    mut acs: Query<(
        &mut Pilot,
        &mut Transform,
        &GlobalTransform,
        Option<&mut Status>,
        Option<&crate::fcs::Fcs>,
    )>,
    mut cams: Query<(&mut Orbit, &mut Transform, &mut Projection, &mut ZoomBlur), Without<Pilot>>,
    joints: Query<(&Driven, &GlobalTransform)>,
    mut window: Query<&mut Window, With<bevy::window::PrimaryWindow>>,
    mut cursor: Query<&mut bevy::window::CursorOptions, With<bevy::window::PrimaryWindow>>,
    mouse: Res<bevy::input::mouse::AccumulatedMouseMotion>,
    mut mouselook: Local<bool>,
) {
    let collision = &world.collision;
    let area_bounds = &world.area_bounds;
    let water_return = &world.water_return;
    let start = &world.start;
    if keys.just_pressed(KeyCode::KeyP) {
        piloting.0 = !piloting.0;
    }
    // M toggles mouselook: the grabbed mouse turns the AC (like Q/E) and pitches the camera.
    if keys.just_pressed(KeyCode::KeyM) {
        *mouselook = !*mouselook;
    }
    if !piloting.0 {
        *mouselook = false;
    }
    if let Ok(mut c) = cursor.single_mut() {
        let (grab, visible) = if *mouselook {
            (bevy::window::CursorGrabMode::Locked, false)
        } else {
            (bevy::window::CursorGrabMode::None, true)
        };
        if c.grab_mode != grab {
            c.grab_mode = grab;
            c.visible = visible;
        }
    }
    let (Some(mut motion), Ok((mut p, mut transform, ac_global, mut status, fcs))) =
        (motion, acs.single_mut())
    else {
        return;
    };
    if motion.in_place != piloting.0 {
        motion.in_place = piloting.0;
    }
    if !piloting.0 {
        p.cam.shown = None;
        p.cam.pitch = 0.0;
        p.cam.pitch_vel = 0.0;
        p.cam.yaw_off = 0.0;
        p.cam.eye_rate = [Blend::default(); 4];
        p.cam.look_at_rate = [Blend::default(); 4];
        p.cam.tilt = Blend::default();
        (
            p.cam.action,
            p.cam.fov_override,
            p.cam.fov,
            p.cam.shake_phase,
            p.cam.accel,
            p.cam.impact_pending,
            p.cam.impact_timer,
            p.cam.footstep_pending,
            p.cam.footstep_timer,
            p.cam.rumble_duration,
            p.cam.rumble_elapsed,
            p.cam.blur,
            p.cam.blur_accum,
            p.ready_cam,
        ) = (
            None,
            None,
            Envelope::default(),
            0.0,
            0.0,
            0.0,
            0.0,
            false,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            ReadyCamera::Off,
        );
        if let Ok((mut o, cam_t, _, mut blur)) = cams.single_mut() {
            blur.clear_speed_blur();
            if o.follow {
                o.follow = false;
                let center = transform.translation + (garage.bounds.0 + garage.bounds.1) * 0.5;
                let offset = cam_t.translation - center;
                let radius = offset.length();
                if radius > 0.5 {
                    let dir = offset / radius;
                    o.focus = center;
                    o.radius = radius;
                    o.yaw = dir.x.atan2(dir.z);
                    o.pitch = dir.y.clamp(-1.0, 1.0).asin().clamp(-1.5, 1.5);
                } else {
                    o.frame(
                        transform.translation + garage.bounds.0,
                        transform.translation + garage.bounds.1,
                    );
                }
            }
        }
        return;
    }
    let mut input = read_input(&keys, &held.0, &pads);
    // Ready-position aim turn speed scaling (`0x8287b658` -> `cat-10 +0x16c`, `0.18..0.5`).
    let zoom_turn_scale = match p.ready_cam {
        ReadyCamera::Zoomed { turn_scale, .. } => turn_scale.clamp(0.1, 1.0),
        _ => 1.0,
    };
    input.turn *= zoom_turn_scale;
    input.look *= zoom_turn_scale;
    if !*mouselook {
        p.cam.yaw_off = 0.0;
    }
    if *mouselook {
        // AC6-style: the mouse moves the camera freely; the AC turns toward it at its turn rate.
        p.cam.yaw_off = (p.cam.yaw_off - mouse.delta.x * MOUSE_YAW * zoom_turn_scale)
            .clamp(-std::f32::consts::PI, std::f32::consts::PI);
        p.cam.pitch = (p.cam.pitch - mouse.delta.y * MOUSE_PITCH * zoom_turn_scale)
            .clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }
    if input.boost_toggle {
        p.boost = !p.boost;
    }
    p.boost |= held.0.contains(&KeyCode::KeyV);
    p.command = input.stick;
    if p.qb.is_none() && p.wj.is_none() && p.kick.is_none() && p.blade.is_none() {
        p.glide |= input.glide && p.boost && !p.airborne && p.en >= p.ctrl.glide_en_tick;
        p.jump_queued |= input.jump && !p.airborne;
        p.wall_jump_queued |= input.jump && p.airborne;
    }
    // Re-fire is gated by `HighBoostState` ending (`0x8200d378` slot `+0x50` = `0x8241a6b8`) and
    // `action_ctrl +0x110` (`qb_reload_ticks`, `0x8283ed98`), not the animation clip length.
    if input.quick_boost && !p.glide {
        start_quick_boost(&mut p);
    }
    p.prev_position = transform.translation;
    let yaw = p.yaw;
    p.accumulator = (p.accumulator + time.delta_secs()).min(10.0 / TICK_RATE);
    while p.accumulator >= 1.0 / TICK_RATE {
        p.accumulator -= 1.0 / TICK_RATE;
        p.cam.aim(input.look);
        let before = p.velocity;
        if *mouselook {
            let per_tick = (p.ctrl.turn_rate / TICK_RATE).max(1e-4);
            input.turn = (p.cam.yaw_off / per_tick).clamp(-1.0, 1.0);
            let yaw_before = p.yaw;
            step(&mut p, &input, &mut transform.translation, &collision);
            p.cam.yaw_off -= p.yaw - yaw_before;
        } else {
            step(&mut p, &input, &mut transform.translation, &collision);
        }
        p.cam.accel = (p.velocity - before).with_y(0.0).length() * TICK_TO_KMH;
    }
    // Keep mouselook `input.turn` consistent on render frames where the 60 Hz accumulator did not
    // tick (e.g. >60 FPS monitors), so `state()` doesn't flap between `turn_*` and `idle`.
    if *mouselook {
        let per_tick = (p.ctrl.turn_rate / TICK_RATE).max(1e-4);
        input.turn = if p.cam.yaw_off.abs() > 0.03 {
            (p.cam.yaw_off / per_tick).clamp(-1.0, 1.0)
        } else {
            0.0
        };
    }
    p.area_zone = area_bounds
        .as_deref()
        .map_or(crate::map::AreaZone::Inside, |b| {
            b.enforce(&mut transform.translation, &mut p.velocity)
        });
    if let (Some(wr), Some(st)) = (water_return.as_deref(), start.as_deref()) {
        if check_water_return(&mut p, &mut transform.translation, wr, *st, &collision) {
            info!(
                "water return triggered -> respawned at ({:.1}, {:.1}, {:.1})",
                transform.translation.x, transform.translation.y, transform.translation.z
            );
        }
    }
    if let Some(status) = status.as_deref_mut() {
        status.en = p.en;
        status.en_max = p.en_max;
    }
    transform.rotation = Quat::from_rotation_y(p.yaw);

    // The walk / dash clip direction is relative to the body, the stick to the camera.
    let body = Input {
        stick: body_stick(input.stick, p.cam.yaw_off),
        ..input
    };
    let had_quick_pending = p.quick_pending.is_some();
    let landed_impact = if p.landed {
        Some(std::mem::take(&mut p.landing_impact_kmh))
    } else {
        None
    };
    let next = state(&p, &body, &motion);
    (p.landed, p.quick_pending) = (false, None);
    // Landing camera shake (360 msg 0x68 `0x8285cf88` + msg 0x69 `0x8285cfb0`).
    if let Some(impact_kmh) = landed_impact {
        p.trigger_footstep_shake();
        if impact_kmh > 0.0 {
            p.trigger_impact_shake(impact_kmh);
        }
    }
    // Footstep camera shake on TAE kind 103 events while walking/turning/landing
    // (360 msg 0x69 `0x8285cfb0`, gated by controller slot 0x38 `0x8285e498`).
    if matches!(next.0, "walk" | "turn_left" | "turn_right" | "land")
        && motion
            .tae_events
            .iter()
            .any(|ev| matches!(ev, crate::pose::TaeFired::Footstep { .. }))
    {
        p.trigger_footstep_shake();
    }
    let ow_bank_changed = motion.ow_bank_active != motion.playing_ow_clip
        && (!motion.ow_bank_active
            || ow_locomotion_row(next.0, next.1)
                .and_then(|row_id| acvd_data::find(PARAM_ACMOTION_BIN, row_id))
                .is_some_and(|r| motion.has_ow_clip(r.data.anim_id)));
    let changed = p.state != Some(next) || had_quick_pending || ow_bank_changed;
    if changed {
        if !p.suppress_locomotion {
            let ow_row = if motion.ow_bank_active {
                ow_locomotion_row(next.0, next.1)
                    .and_then(|row_id| acvd_data::find(PARAM_ACMOTION_BIN, row_id))
                    .filter(|r| motion.has_ow_clip(r.data.anim_id))
            } else {
                None
            };
            let row = ow_row.or_else(|| {
                acvd_data::ac_state(next.0, next.1)
                    .and_then(|s| acvd_data::find(PARAM_ACMOTION_BIN, s.row))
            });
            p.clip_ok = match row {
                Some(row) => {
                    let speed = if next.0.starts_with("turn_") {
                        AC_ANIM_PARAM.turn_clip_speed
                    } else {
                        1.0
                    };
                    let hokan = p.hokan_base + u32::from(row.data.interpolate_id);
                    let fade = find(PARAM_ACANIMHOKAN_BIN, hokan)
                        .map(|h| Fade::of(&h.data))
                        .unwrap_or_else(|| {
                            warn!("no acanimhokan row {hokan}");
                            [0.0; 8]
                        });
                    motion
                        .play(
                            &garage.disc,
                            row.data.anim_id,
                            row.data.b_loop != 0,
                            speed,
                            fade,
                        )
                        .map_err(|e| debug!("{}: {e:#}", next.0))
                        .is_ok()
                }
                None => {
                    warn!("no ac_states.csv row for {next:?}");
                    false
                }
            };
        }
        debug!(
            "state {next:?} -> {} at {:.1?}",
            motion.name(),
            transform.translation
        );
        p.state = Some(next);
    }

    // Dash / air-move / glide / boost-charge lean: one 360-frame clip whose frame is the heading
    // in degrees (a key per 45 degrees = the eight directions), so it blends smoothly between them.
    if matches!(
        next.0,
        "dash" | "air_move" | "glide" | "glide_air" | "quick_boost_charge"
    ) && motion.clip.frames == 360
        && p.clip_ok
    {
        // Mirrored: the clip's 90 degree key leans the way the stick's left does (checked by eye).
        // Heading of the actual velocity (m/tick) against the body, not the stick: the AC's
        // acceleration (its weight) then sets how fast the lean swings between directions.
        // Falls back to the stick while nearly still.
        let target = lean_heading(p.yaw, p.velocity, body.stick);
        if motion.wheel.is_none() {
            motion.frame = target;
        }
        motion.wheel = Some(target);
        // The lean builds with speed: 0 at a standstill, full at the boost top speed (or full
        // during Boost Charge wind-up when stick is held).
        motion.lean = if next.0 == "quick_boost_charge" {
            body.stick.length().clamp(0.0, 1.0)
        } else {
            (p.velocity.with_y(0.0).length() / p.ctrl.boost_max_tick.max(1e-4)).clamp(0.0, 1.0)
        };
    }

    // Core Control (360 `0x8284a828` -> `0x8289e730` -> `jcondata.bin` "Core"):
    // track locked FCS target or camera aim on `core` (+ slight `head_01` lead), gated by
    // `AC_MOTIONINFO.b_disable_core_ctrl` and blade attacks.
    let core_allowed = acvd_data::ac_state(next.0, next.1)
        .and_then(|s| acvd_data::find(PARAM_ACMOTION_BIN, s.row))
        .is_none_or(|r| r.data.b_disable_core_ctrl == 0)
        && !p.in_blade();
    let (core_yaw, core_pitch) = core_aim_angles(
        p.yaw,
        transform.translation,
        p.cam.yaw_off,
        p.cam.pitch,
        body.stick.x,
        if *mouselook { 0.0 } else { input.turn },
        fcs.and_then(|f| f.locked.as_ref()).map(|l| l.world_pos),
    );
    motion.core_ctrl.enabled = core_allowed;
    motion.core_ctrl.target_yaw = core_yaw;
    motion.core_ctrl.target_pitch = core_pitch;

    if let Ok((mut o, mut cam, mut proj, mut blur)) = cams.single_mut() {
        o.follow = true;
        o.yaw += p.yaw - yaw;
        let airborne = p.airborne;
        let action = camera_action(next.0, p.boost, airborne, p.ready_cam);
        let fov_override = match p.ready_cam {
            ReadyCamera::Zoomed { fov_deg, .. } => Some(fov_deg),
            _ => None,
        };
        p.cam.act(action, fov_override);
        let velocity = p.velocity;
        p.cam.blur_step(velocity, time.delta_secs());
        // Mouselook turning is auto-generated and jittery near zero: ignore small values so the
        // side shift only flips for a real turn (the blend itself smooths the move).
        let cam_turn = if *mouselook && input.turn.abs() < 0.25 {
            0.0
        } else {
            input.turn
        };
        p.cam.blend_eye(cam_turn, time.delta_secs());
        let root = transform.translation;
        let center = motion
            .skeleton
            .bones
            .iter()
            .position(|b| b.rest.as_ref().is_some_and(|r| r.name == WAIST_BONE));
        let waist = joints
            .iter()
            .find(|(d, _)| Some(d.bone) == center)
            .map_or(Vec3::ZERO, |(_, g)| {
                g.translation() - ac_global.translation()
            });
        let water = collision.ray_down(
            Vec3::new(root.x, root.y + RAY_HALF, root.z),
            root.y - RAY_HALF,
            Layer::Water,
        );
        let cam_rot = transform.rotation * Quat::from_rotation_y(p.cam.yaw_off);
        let total_yaw = p.yaw + p.cam.yaw_off;
        if *mouselook {
            let delta = total_yaw - p.cam.cam_yaw;
            p.cam.swing(root + waist, delta);
            p.cam.hud_yaw_lead = (p.cam.hud_yaw_lead + delta).clamp(-0.22, 0.22);
        }
        p.cam.cam_yaw = total_yaw;
        p.cam.waist = waist;
        let (eye, look_at) = p.cam.place(root + waist, cam_rot, water);
        let (eye, look_at) = p
            .cam
            .follow(eye, look_at, cam_rot, airborne, time.delta_secs());
        let yaw_follow = p.cam.look_at_rate[0].value.clamp(0.01, 1.0);
        p.cam.hud_yaw_lead *= (1.0 - yaw_follow).powf(time.delta_secs() * TICK_RATE);
        o.focus = look_at;
        let velocity = p.velocity;
        let roll = p.cam.roll(velocity, transform.rotation, time.delta_secs());
        *cam = Transform::from_translation(eye).looking_at(look_at, Vec3::Y);
        cam.rotate_local_z(roll);
        if let Projection::Perspective(persp) = proj.as_mut() {
            persp.fov = p.cam.fov(time.delta_secs());
        }
        p.cam.apply_zoom_blur(&mut blur);
    }
    if changed || time.elapsed_secs() % 0.25 < time.delta_secs() {
        let speed = Vec2::new(p.velocity.x, p.velocity.z).length() * TICK_TO_KMH;
        let title = format!(
            "{} | weight {:.0} (legs carry {:.0}/{:.0}) | {} {} | {:.0} km/h, alt {:.1} m | boost {} | {}",
            garage.status,
            p.ctrl.total_weight,
            p.ctrl.leg_capability,
            p.ctrl.leg_capability / p.ctrl.weight_ratio,
            next.0,
            next.1.map_or(String::new(), |d| d.to_string()),
            speed,
            transform.translation.y,
            if p.glide { "glide" } else if p.boost { "on" } else { "off" },
            motion.name()
        );
        if let Ok(mut w) = window.single_mut() {
            if w.title != title {
                w.title = title;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            let mut p = Pilot::new(ctrl, 0);
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
            let right = Input {
                stick: Vec2::X,
                ..Input::default()
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
        let mut p = Pilot::new(ctrl, 0);
        let mut pos = Vec3::ZERO;
        assert!(start_quick_boost(&mut p));
        assert!(!start_quick_boost(&mut p), "blocked while active");
        let tap_release = Input {
            stick: Vec2::Y,
            qb_held: false,
            ..Input::default()
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
        assert!(hopped, "grounded High Boost should add a small vertical hop");
        assert!(
            (tap_peak - expected_tap_max).abs() < 1e-3,
            "tap peak {tap_peak} vs expected {expected_tap_max}"
        );
        assert!(!p.boost, "High Boost must not force boost mode on");
        // Immediately re-fireable once HighBoostState ends (well under 20 ticks).
        assert!(burst_ticks <= 16, "burst took {burst_ticks} ticks");
        assert!(start_quick_boost(&mut p), "re-fire unlocked right after burst");
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
        let mut p2 = Pilot::new(ctrl, 0);
        p2.boost = true;
        p2.airborne = true;
        let mut pos2 = Vec3::new(0.0, 500.0, 0.0);
        assert!(start_quick_boost(&mut p2));
        let sweet_ticks = ctrl.qb_sweet_frames.round() as u32;
        let hold = Input {
            stick: Vec2::Y,
            qb_held: true,
            ..Input::default()
        };
        for _ in 0..sweet_ticks.saturating_sub(1) {
            step(&mut p2, &hold, &mut pos2, &Collision::default());
        }
        step(&mut p2, &tap_release, &mut pos2, &Collision::default());
        assert!(p2.second_stage_qb, "sweet-spot release must trigger Second-Stage QB");
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
        assert!(oblique.y.abs() < 1e-5 && oblique.x > 0.0 && oblique.z < 0.0, "{oblique}");
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
        let mut p = Pilot::new(ctrl, 0);
        let mut pos = Vec3::new(5.5, 0.0, 0.0);
        p.jump_queued = true;
        step(&mut p, &Input::default(), &mut pos, &world);
        assert!(p.airborne);
        assert_eq!(p.wj_lockout, ctrl.wall_jump_post_jump_lockout);
        p.wall_jump_queued = true;
        step(&mut p, &Input::default(), &mut pos, &world);
        assert!(p.wj.is_none(), "post-jump lockout must block wall kick");
        for _ in 0..12 {
            step(&mut p, &Input::default(), &mut pos, &world);
        }
        assert!(p.wj_lockout < 1.0);
        // Trigger directional wall kick away from wall (+X stick).
        p.velocity.y = -0.3;
        p.wall_jump_queued = true;
        let away = Input {
            stick: Vec2::new(1.0, 0.0),
            ..Input::default()
        };
        step(&mut p, &away, &mut pos, &world);
        assert!(matches!(p.wj, Some(WallJumpPhase::Prep { .. })));
        assert_eq!(camera_action("wall_jump_prep", true, true, ReadyCamera::Off), 43);
        assert_eq!(camera_action("wall_jump", true, true, ReadyCamera::Off), 43);

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
        let mut p2 = Pilot::new(ctrl, 0);
        p2.airborne = true;
        p2.boost = true;
        let mut pos2 = Vec3::new(5.5, 5.0, 0.0);
        p2.wall_jump_queued = true;
        let neutral = Input::default();
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
        assert!(vert_h < 1e-4, "vertical wall kick should add no horizontal speed, got {vert_h}");
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
    fn air_glide_plays_the_entry_then_the_float() {
        let ground = glide_clip(("glide", None), false, true, false, 2);
        assert_eq!(ground, ("glide", None));
        let entry = glide_clip(("glide", None), true, true, false, 2);
        assert_eq!(entry, ("glide_air_in", Some(2)));
        let held = glide_clip(("glide_air_in", Some(2)), true, true, false, 0);
        assert_eq!(held, ("glide_air_in", Some(2)));
        let float = glide_clip(("glide_air_in", Some(2)), true, true, true, 0);
        assert_eq!(float, ("glide_air", None));
        let landed = glide_clip(("glide_air", None), false, true, false, 0);
        assert_eq!(landed, ("glide", None));
        assert_eq!(acvd_data::ac_state("glide_air", None).unwrap().row, 222);
        assert_eq!(
            acvd_data::ac_state("glide_air_in", Some(0)).unwrap().row,
            223
        );
        assert_eq!(
            acvd_data::ac_state("glide_air_in", Some(1)).unwrap().row,
            230
        );
        assert_eq!(
            acvd_data::ac_state("glide_air_in", Some(6)).unwrap().row,
            225
        );
        assert_eq!(
            acvd_data::ac_state("glide_air_in", Some(7)).unwrap().row,
            224
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
        let mut p = Pilot::new(ctrl, 0);
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

        let mut p = Pilot::new(ctrl, 0);
        assert_eq!(p.en, p.en_max);
        assert!(start_quick_boost(&mut p));
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
        assert!(scan_net_tick > net_tick, "Scan Mode recharges EN faster than Combat Mode");
        step_en(&mut p);
        assert!((p.en - (before_scan + scan_net_tick)).abs() < 1e-3);
        p.scan_mode = false;

        // High Boost is blocked without draining when EN < qb_en_drain.
        p.qb = None;
        p.qb_cooldown = 0.0;
        p.en = ctrl.qb_en_drain - 10.0;
        let low_en = p.en;
        assert!(!start_quick_boost(&mut p));
        assert_eq!(p.en, low_en);
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
        let mut p = Pilot::new(ctrl, 0);
        let mut pos = Vec3::ZERO;
        p.boost = true;
        p.glide = true;
        // Start with just enough EN for 2 ticks of glide (net drain per tick = glide - net_rec).
        let net_rec = ctrl.en_output_tick - ctrl.passive_en_drain_tick;
        let net_drain = ctrl.glide_en_tick - net_rec;
        assert!(net_drain > 0.0, "glide drains faster than generator surplus");
        p.en = ctrl.glide_en_tick * 1.5;

        let input = Input {
            stick: Vec2::Y,
            ..Input::default()
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
        let mut p = Pilot::new(ctrl, 0);
        let mut pos = Vec3::new(0.0, 20.0, 0.0);
        p.airborne = true;
        p.wj = Some(WallJumpPhase::Accel {
            timer: 0.0,
            dir: Vec3::Y,
            clip_dir: 0,
        });
        let input = Input::default();
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
        let mut p = Pilot::new(ctrl, 0);
        let mut pos = Vec3::ZERO;
        let en_start = p.en;
        assert!(start_quick_boost(&mut p));
        let hold_v = Input {
            stick: Vec2::Y,
            qb_held: true,
            ..Input::default()
        };
        let mut entered_kick = false;
        for _ in 0..40 {
            step(&mut p, &hold_v, &mut pos, &ground);
            if p.in_kick() {
                entered_kick = true;
                break;
            }
        }
        assert!(entered_kick, "holding High Boost into Phase 2 must launch KickState");
        assert_eq!(p.kicks_started, 1);
        assert!(p.en < en_start - ctrl.qb_en_drain, "kick must drain qb_en_drain");
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
        p.cam.pitch = -0.7; // looking down
        assert!(p.start_kick());
        step(&mut p, &Input::default(), &mut pos, &Collision::default());
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
        let mut p = Pilot::new(ctrl, 0);
        let mut pos = Vec3::ZERO;
        // Locked target 25 m ahead along -Z (beyond 12 m instant-swing distance).
        let target_pos = Vec3::new(0.0, 3.0, -25.0);
        let launch = BladeLaunch {
            hand_idx: 1,
            physical: false,
            target: Some((Entity::PLACEHOLDER, target_pos)),
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
            step(&mut p, &Input::default(), &mut pos, &ground);
            match p.blade.as_ref().map(|b| b.phase) {
                Some(BladePhase::Dash) => saw_dash = true,
                Some(BladePhase::Swing) => saw_swing = true,
                Some(BladePhase::Recovery) => saw_recovery = true,
                Some(BladePhase::Charge) => {}
                None => break,
            }
        }
        assert!(saw_dash, "locked target 25 m away must trigger BladePhase::Dash");
        assert!(saw_swing, "blade dash must transition into BladePhase::Swing");
        assert!(saw_recovery, "blade swing must transition into BladePhase::Recovery");
        assert!(!p.in_blade(), "blade attack must finish cleanly after recovery");
        assert!(
            pos.z < -8.0,
            "homing blade dash should close significant distance toward target (z={})",
            pos.z
        );
    }

    #[test]
    fn core_aim_angles_tracks_fcs_lock_and_strafe() {
        // Bevy AC forward is -Z, left is -X, up is +Y.
        // Target straight ahead (0, 5, -100) -> 0 deg yaw, 0 deg pitch.
        let (yaw0, pitch0) = core_aim_angles(
            0.0,
            Vec3::ZERO,
            0.0,
            0.0,
            0.0,
            0.0,
            Some(Vec3::new(0.0, 5.0, -100.0)),
        );
        assert!(yaw0.abs() < 1e-4, "straight-ahead yaw={}", yaw0.to_degrees());
        assert!(pitch0.abs() < 1e-4, "straight-ahead pitch={}", pitch0.to_degrees());

        // Locked target 100 m forward (-Z) and 100 m left (-X) at AC torso height (+5 Y) -> +45 deg yaw, 0 pitch.
        let (yaw, pitch) = core_aim_angles(
            0.0,
            Vec3::ZERO,
            0.0,
            0.0,
            0.0,
            0.0,
            Some(Vec3::new(-100.0, 5.0, -100.0)),
        );
        assert!((yaw.to_degrees() - 45.0).abs() < 0.1, "yaw={}", yaw.to_degrees());
        assert!(pitch.abs() < 0.01);

        // Locked target ahead-right (+X, -Z) and above (+Y) -> -45 deg yaw, positive pitch,
        // and the resulting aim ray points directly at the target even when the AC is rotated.
        let body_yaw = 0.7_f32;
        let target = Vec3::new(60.0, 45.0, -80.0);
        let (ryaw, rpitch) = core_aim_angles(body_yaw, Vec3::ZERO, 0.0, 0.0, 0.0, 0.0, Some(target));
        let aim_dir =
            Quat::from_rotation_y(body_yaw + ryaw) * Quat::from_rotation_x(rpitch) * FORWARD;
        let expected = (target - Vec3::Y * 5.0).normalize();
        assert!(
            aim_dir.dot(expected) > 0.9999,
            "aim_dir={aim_dir:?} expected={expected:?} (yaw={} pitch={})",
            ryaw.to_degrees(),
            rpitch.to_degrees()
        );

        // Unlocked strafe right (stick.x = +1) counter-twists torso left (+yaw) to hold forward aim.
        let (syaw, spitch) = core_aim_angles(0.0, Vec3::ZERO, 0.0, -0.1, 1.0, 0.0, None);
        assert!((syaw.to_degrees() - 18.0).abs() < 0.1);
        assert!((spitch + 0.1).abs() < 1e-5);
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
        let mut p = Pilot::new(ctrl, 0);
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

        // Above water: no respawn.
        let mut above = Vec3::new(-45.0, 5.0, 12.0);
        assert!(!check_water_return(&mut p, &mut above, &wr, fallback, &ground));

        // Below water_height (3.0): teleports to nearest return point (-50, 15, 10).
        let mut submerged = Vec3::new(-45.0, 2.5, 12.0);
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
    fn ow_locomotion_row_maps_motion_ow_equip_states() {
        assert_eq!(ow_locomotion_row("stand", None), Some(221));
        assert_eq!(ow_locomotion_row("walk", Some(0)), Some(194));
        assert_eq!(ow_locomotion_row("walk", Some(4)), Some(204));
        assert_eq!(ow_locomotion_row("dash", Some(0)), Some(268));
        assert_eq!(ow_locomotion_row("air_move", Some(0)), Some(269));
        assert_eq!(ow_locomotion_row("jump", None), Some(258));
        assert_eq!(ow_locomotion_row("quick_boost", Some(0)), Some(254));
        assert_eq!(ow_locomotion_row("kick", None), Some(274));
    }

    #[test]
    fn camera_shake_impact_footstep_and_rumble_decay() {
        let mut cam = FollowCam::of(10);
        // Idle action has zero movement shake.
        cam.act(0, None);

        // 1. Impact below ShakeImpactAccelMin (150) produces no shake.
        cam.impact_pending = 120.0;
        assert_eq!(cam.shake(1.0 / 60.0), Vec2::ZERO);
        assert_eq!(cam.impact_timer, 0.0);

        // Impact at ShakeImpactAccelMax (250) starts full ShakeImpactTime (0.1 s), shakes, then decays to 0.
        cam.impact_pending = 250.0;
        let mut peak_impact = 0.0f32;
        for _ in 0..5 {
            peak_impact = peak_impact.max(cam.shake(0.015).length());
        }
        assert!(peak_impact > 0.0, "impact shake must produce non-zero offset");
        for _ in 0..10 {
            cam.shake(0.02);
        }
        assert_eq!(cam.impact_timer, 0.0);
        assert_eq!(cam.shake(1.0 / 60.0), Vec2::ZERO);

        // 2. Footstep shake (row 0: ampl (0, 0.15), freq (0, 0.07), time 0.1 s) is purely vertical and decays.
        cam.footstep_pending = true;
        let step1 = cam.shake(0.017);
        assert_eq!(step1.x, 0.0, "row 0 footstep shake is vertical-only");
        assert!(step1.y.abs() > 0.0, "footstep shake must displace Y");
        for _ in 0..10 {
            cam.shake(0.02);
        }
        assert_eq!(cam.footstep_timer, 0.0);
        assert_eq!(cam.shake(1.0 / 60.0), Vec2::ZERO);

        // 3. Timed impulse / rumble shake (`0x82bf8f00`) decays over duration and clears.
        cam.rumble_duration = 0.2;
        cam.rumble_elapsed = 0.0;
        cam.rumble_ampl = Vec2::new(0.1, 0.2);
        cam.rumble_freq = Vec2::new(0.04, 0.04);
        let early = cam.shake(0.01);
        assert!(early.length() > 0.0, "rumble shake must be non-zero early");
        for _ in 0..15 {
            cam.shake(0.02);
        }
        assert_eq!(cam.rumble_duration, 0.0);
        assert_eq!(cam.shake(1.0 / 60.0), Vec2::ZERO);
    }

    #[test]
    fn ready_position_zoom_camera_transitions_and_stows() {
        // Action mapping (`0x8285db68` + `0x8285d548`): Deploying -> 40, Zoomed -> 49, not overridden by boost.
        assert_eq!(camera_action("idle", true, false, ReadyCamera::Deploying), 40);
        assert_eq!(
            camera_action(
                "dash",
                true,
                true,
                ReadyCamera::Zoomed {
                    fov_deg: 10.0,
                    turn_scale: 0.25
                }
            ),
            49
        );

        let mut cam = FollowCam::of(10);
        cam.act(0, None);
        cam.blend_eye(0.0, 1.0 / 60.0);
        cam.shown = Some(Shown {
            eye: Vec3::ZERO,
            look_at: Vec3::Z,
            eye_target: Vec3::ZERO,
            look_at_target: Vec3::Z,
        });
        assert!((cam.fov(1.0 / 60.0).to_degrees() - 45.0).abs() < 1e-3);
        assert!((cam.eye_dist.value - 16.0).abs() < 1e-3);

        // Enter zoomed ready stance with sniper cannon FOV 10 deg (`0x8285e418` -> row 49).
        cam.act(49, Some(10.0));
        for _ in 0..35 {
            cam.blend_eye(0.0, 1.0 / 60.0);
            let _ = cam.fov(1.0 / 60.0);
        }
        assert!(
            (cam.fov(0.0).to_degrees() - 10.0).abs() < 0.2,
            "zoomed sniper cannon FOV should reach 10 deg, got {}",
            cam.fov(0.0).to_degrees()
        );
        assert!(
            (cam.eye_dist.value - 6.0).abs() < 0.1,
            "row 49 eye_dist should pull in to 6.0 m, got {}",
            cam.eye_dist.value
        );

        // Release back to idle (row 0): FOV returns to 45 deg and eye_dist returns toward 16 m.
        cam.act(0, None);
        for _ in 0..130 {
            cam.blend_eye(0.0, 1.0 / 60.0);
            let _ = cam.fov(1.0 / 60.0);
        }
        assert!(
            (cam.fov(0.0).to_degrees() - 45.0).abs() < 0.2,
            "stowed FOV should return to 45 deg, got {}",
            cam.fov(0.0).to_degrees()
        );
        assert!(
            (cam.eye_dist.value - 16.0).abs() < 0.2,
            "stowed eye_dist should return to 16.0 m, got {}",
            cam.eye_dist.value
        );
    }

    #[test]
    fn speed_blur_steps_at_30hz_not_60hz() {
        let mut cam = FollowCam::of(10);
        // Boost on the ground (row 38) has BlurRate = 4.0.
        cam.act(38, None);
        // Velocity 190 km/h (halfway between 80 and 300 km/h -> ramp 0.5, target = 0.5 * 4 = 2.0).
        let vel = Vec3::new(0.0, 0.0, 190.0 / TICK_TO_KMH);

        // One 60 Hz frame (1/60 s) has not reached a 30 Hz step yet.
        cam.blur_step(vel, 1.0 / 60.0);
        assert_eq!(cam.blur, 0.0, "blur must not step after a single 1/60 s frame");

        // Second 60 Hz frame completes 1/30 s -> exactly one 0x82bfa9f0 ease step.
        cam.blur_step(vel, 1.0 / 60.0);
        let expected_one_step = (2.0 * cam.behavior.blur_intp_rate).clamp(0.0, 1.0);
        assert!(
            (cam.blur - expected_one_step).abs() < 1e-5,
            "one 30 Hz step expected {expected_one_step}, got {}",
            cam.blur
        );

        // Over 0.5 s at 60 fps (30 frames = 15 steps at 30 Hz), must match 15 blur_tick calls.
        let mut cam_step = FollowCam::of(10);
        cam_step.act(38, None);
        let mut cam_ref = FollowCam::of(10);
        cam_ref.act(38, None);
        let slow_vel = Vec3::new(0.0, 0.0, 100.0 / TICK_TO_KMH); // lower target so it doesn't saturate
        for _ in 0..30 {
            cam_step.blur_step(slow_vel, 1.0 / 60.0);
        }
        for _ in 0..15 {
            cam_ref.blur_tick(slow_vel);
        }
        assert!(
            (cam_step.blur - cam_ref.blur).abs() < 1e-5,
            "30 Hz blur_step ({}) must match 15 discrete ticks ({})",
            cam_step.blur,
            cam_ref.blur
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

        let design = &find(acvd_data::generated::ac_unit::PARAM_ACASSEMBLYDRAWING_BIN, 5001)
            .expect("design 5001")
            .data;
        let mut p = Pilot::new(AcCtrlParam::calculate(design), 10);
        // Drain EN below qb_en_drain -> QB attempt fails and increments qb_shortages.
        p.en = (p.ctrl.qb_en_drain * 0.5).max(0.0);
        assert!(!start_quick_boost(&mut p));
        assert_eq!(p.qb_shortages, 1);
        assert_eq!(p.qb_charge_starts, 0);

        // Refill EN -> QB charge starts and records 0xC1D (grounded).
        p.en = p.en_max;
        assert!(start_quick_boost(&mut p));
        assert_eq!(p.qb_charge_starts, 1);
        assert_eq!(p.last_qb_tae, 0x0C1D);

        // Release at second-stage sweet spot while airborne -> records 0xC22.
        p.airborne = true;
        let sweet_ratio = p.ctrl.qb_sweet_frames / p.ctrl.qb_charge_max_frames;
        fire_quick_boost(&mut p, Vec2::Y, sweet_ratio);
        assert!(p.second_stage_qb);
        assert_eq!(p.last_qb_tae, 0x0C22);
    }
}
