//! Piloting the shown AC, with the pad layout of the game's manual (`lang/en/text/menu/manual.fmg`):
//! WASD / left stick move relative to its facing, Q/E / right stick X turn, Up/Down / right stick Y
//! pitch the follow camera, V / L1 toggles boost mode, Space / South (×) jumps and turns boost
//! mode on, Ctrl / L3 glides while boosting on the ground (the air clips take over once it
//! leaves the ground), Shift / West (□) high-boosts (quick boost)
//! along the stick, forward without one. The 60 Hz movement state machine now lives in
//! `acvd_sim::motion::AcMotion`; this module keeps the follow camera, animation clip selection,
//! camera shakes/blur, input sampling, and the Bevy system that drives them.

use std::ops::{Deref, DerefMut};

use acvd_data::generated::ac_unit::{
    MotionCollateSt, PARAM_ACANIMHOKAN_BIN, PARAM_ACMOTION_BIN, PARAM_MOTIONCOLLATE_BIN,
};
use acvd_data::generated::camera::{
    AcCameraActionSt, AccamBehaviorParamSt, PARAM_ACCAMBEHAVIORPARAM_BIN, PARAM_ACCAMERAACTION_BIN,
    PARAM_ACCAMERAOFFSETPARAM_BIN,
};
use acvd_data::generated::ctrl::AcCtrlParam;
use acvd_data::generated::tuning::AC_ANIM_PARAM;
use acvd_data::{find, Row};
use acvd_render::app::Orbit;
use acvd_sim::input::InputFrame;
use acvd_sim::motion::{body_stick, direction, glide_clip, travel_eighth, AcMotion};
use bevy::prelude::*;

use crate::blur::ZoomBlur;
use crate::collision::{Collision, Layer, RayFilter, RAY_HALF};
use crate::hud::Status;
use crate::pose::{Driven, Fade, Motion};

pub use acvd_sim::motion::{BladeLaunch, BladePhase, GlidePhase, QbPhase, WallJumpPhase};

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
/// Follow-camera clip (`0x82bfaef0` -> `0x82bfa378` / `0x82c4d680`): the eye stops this far
/// short of the wall it hit, so the near plane never sees through the surface.
const CAM_CLIP_MARGIN: f32 = 0.3;
/// The clipped eye never comes closer to the base point than this (the 360 clamps its min
/// distance to 4 m at `0x8371e034`; the rewrite lets it come closer so a wall in the AC's back
/// cannot clip, but keeps it off the base point).
const CAM_CLIP_MIN: f32 = 0.5;
/// Per-60-Hz-frame fraction the eye closes on the unobstructed distance after a wall clears
/// (`0x82bfa740` blends at `0x8371e03c` = 0.2); pulling in to a wall is immediate.
const CAM_CLIP_RECOVER: f32 = 0.2;
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

/// Placement of the default follow camera, eased toward its targets by the follow rates.
#[derive(Clone, Copy)]
pub(crate) struct FollowCam {
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
    /// Eye distance from the base point kept by the wall clip (`CamFx` `0x82bfaef0` ->
    /// `0x82bfa740`, distance state at `+0x2a0..+0x2b8`): 0 until the first clipped frame.
    clip_dist: f32,
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
            clip_dist: 0.0,
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

    /// Keeps the eye out of map geometry (360 `CamFx` `0x82bfaef0`: a camera-filter ray
    /// `0x82bfa378` -> `0x82c4d680` (filter `0x00000008`) from the AC base point to the eye, a
    /// short sphere probe `0x82bf9a28`, then the distance blend `0x82bfa740`). `pivot` is the
    /// waist, `eye` / `look_at` the placed points. The eye is pulled to the first wall hit minus
    /// [`CAM_CLIP_MARGIN`] at once, and slides back out after the wall clears. Both points are
    /// moved by the same correction so the view direction does not change.
    fn clip(
        &mut self,
        pivot: Vec3,
        eye: Vec3,
        look_at: Vec3,
        collision: &Collision,
        dt: f32,
    ) -> (Vec3, Vec3) {
        let delta = eye - pivot;
        let want = delta.length();
        if want <= 1e-4 {
            return (eye, look_at);
        }
        let dir = delta / want;
        let hit = collision
            .raycast(pivot, eye, RayFilter::Camera)
            .map(|(t, _, _)| (t * want - CAM_CLIP_MARGIN).max(CAM_CLIP_MIN));
        let target = hit.unwrap_or(want);
        let dist = if self.clip_dist <= 0.0 || target < self.clip_dist {
            target
        } else {
            let step = 1.0 - (1.0 - CAM_CLIP_RECOVER).powf(dt * TICK_RATE);
            self.clip_dist + (target - self.clip_dist) * step
        };
        self.clip_dist = dist;
        let eye = pivot + dir * dist;
        // Keep the clipped eye as the follow state, so releasing a wall eases back out instead
        // of snapping to last frame's unobstructed point.
        if let Some(s) = self.shown.as_mut() {
            s.eye = eye;
        }
        (eye, look_at + (eye - (pivot + dir * want)))
    }
}

/// Whether input drives the AC (`P` toggles) instead of browsing clips.
#[derive(Resource)]
pub struct Piloting(pub bool);

/// Keys held for the whole run (`--hold w,shift`), for scripted checks.
#[derive(Resource, Default)]
pub struct Held(pub Vec<KeyCode>);

/// Bevy component: the pure movement simulation plus the follow camera and clip-mapping state.
#[derive(Component)]
pub struct Pilot {
    pub motion: AcMotion,
    pub(crate) cam: FollowCam,
    /// Ready-position camera stance (`accameraaction` row 40 -> 49 + zoom FOV override `0x8285e418`).
    pub ready_cam: ReadyCamera,
    /// The locomotion state playing, and whether its clip is in the set.
    state: Option<(&'static str, Option<u8>)>,
    /// A sniper stance clip owns the body until it is released.
    suppress_locomotion: bool,
    clip_ok: bool,
    accumulator: f32,
    /// The legs' `motioncollate` HokanParamID: the `acanimhokan.bin` block (0, or 500 for tanks)
    /// each `acmotion` InterpolateID is added to.
    hokan_base: u32,
}

impl Deref for Pilot {
    type Target = AcMotion;
    fn deref(&self) -> &Self::Target {
        &self.motion
    }
}

impl DerefMut for Pilot {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.motion
    }
}

impl Pilot {
    /// The locomotion state playing, and the hokan block its legs select (`motioncollate`
    /// `HokanParamID`: 0, or 500 for tanks).
    pub(crate) fn pose(&self) -> (Option<(&'static str, Option<u8>)>, u32) {
        (self.state, self.hokan_base)
    }

    /// Overrides the reported locomotion state (Garage View Mode action tests).
    pub(crate) fn set_pose_state(&mut self, state: Option<(&'static str, Option<u8>)>) {
        self.state = state;
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

    /// Mirrors the follow camera's aim into the movement state before a step.
    pub(crate) fn sync_aim(&mut self) {
        self.motion.aim_yaw_off = self.cam.yaw_off;
        self.motion.aim_pitch = self.cam.pitch;
        self.motion.hud_yaw_lead = self.cam.hud_yaw_lead;
    }

    /// A standing AC moving by `ctrl`, with the default follow camera for its legs' motion set.
    pub fn new(ctrl: AcCtrlParam, legs_motion_id: u8) -> Self {
        Self {
            motion: AcMotion::new(ctrl),
            cam: FollowCam::of(legs_motion_id),
            ready_cam: ReadyCamera::Off,
            state: None,
            suppress_locomotion: false,
            clip_ok: false,
            accumulator: 0.0,
            hokan_base: collate(legs_motion_id).map_or(0, |r| r.data.hokan_param_id.into()),
        }
    }

    /// Starts Boost Charge Kick, using the camera's current aim for its direction.
    pub fn start_kick(&mut self) -> bool {
        self.sync_aim();
        self.motion.start_kick()
    }

    /// Starts a melee blade attack, using the camera's current pitch for the downward check.
    pub fn start_blade(&mut self, launch: BladeLaunch) -> bool {
        self.sync_aim();
        self.motion.start_blade(launch)
    }

    /// Launches an Overed Weapon attack dash burst.
    pub fn start_ow_dash(&mut self, dir: Vec3) {
        self.sync_aim();
        self.motion.start_ow_dash(dir);
    }

    /// Stops and rebounds the AC when Boost Charge Kick strikes a target (`0x828433b8`), then
    /// queues the impact camera shake and rumble.
    pub fn on_kick_contact(&mut self) {
        if self.motion.kick.is_some() {
            self.motion.on_kick_contact();
            self.trigger_impact_shake(250.0);
            self.trigger_rumble_shake(0.22, Vec2::new(0.14, 0.22), Vec2::new(0.02, 0.02));
        }
    }

    /// Queues an impact camera shake (360 message `0x68` handler `0x8285cf88`).
    pub fn trigger_impact_shake(&mut self, accel_kmh: f32) {
        if accel_kmh > self.cam.impact_pending {
            self.cam.impact_pending = accel_kmh;
        }
    }

    /// Queues a footstep camera shake (360 message `0x69` handler `0x8285cfb0`).
    pub fn trigger_footstep_shake(&mut self) {
        self.cam.footstep_pending = true;
    }

    /// Triggers a timed impulse/rumble shake (`CamFx_shake` `0x82bfaea0`).
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

    /// Fire direction: the follow camera's pitched forward.
    pub fn aim_direction(&self) -> Vec3 {
        Quat::from_rotation_y(self.yaw + self.cam.yaw_off)
            * Quat::from_rotation_x(self.cam.pitch)
            * FORWARD
    }

    /// Lock-Sight 3D projection direction (`0x829d2120`).
    pub fn sight_direction(&self) -> Vec3 {
        Quat::from_rotation_y(self.yaw + self.cam.yaw_off + self.cam.hud_yaw_lead)
            * Quat::from_rotation_x(self.cam.pitch)
            * FORWARD
    }

    /// Current camera aim angles `(yaw, pitch)` in radians.
    pub fn aim_angles(&self) -> (f32, f32) {
        (self.yaw + self.cam.yaw_off, self.cam.pitch)
    }

    /// 3D Lock-Sight datum origin in world space, matching `FollowCam::place`'s waist.
    pub fn sight_origin(&self, root: Vec3) -> Vec3 {
        root + self.cam.waist + Vec3::Y * self.cam.look_at_height
    }

    /// Current follow-camera roll in radians (`FollowCam::roll`, `0x82bfa708`).
    pub fn camera_roll(&self) -> f32 {
        self.cam.tilt_max * self.cam.tilt.value
    }

    pub fn set_pitch(&mut self, pitch: f32) {
        self.cam.pitch = pitch;
        self.motion.aim_pitch = pitch;
    }

    /// Online prediction: keep the camera on the sampled absolute aim.
    pub(crate) fn set_net_aim(&mut self, yaw_off: f32, pitch: f32) {
        self.cam.yaw_off = yaw_off;
        self.cam.pitch = pitch;
    }
}

/// Mouselook (M): camera yaw / pitch radians per pixel of mouse motion.
const MOUSE_YAW: f32 = 0.003;
const MOUSE_PITCH: f32 = 0.003;

fn read_input(
    keys: &ButtonInput<KeyCode>,
    held: &[KeyCode],
    pads: &Query<&Gamepad>,
    mouse: &ButtonInput<MouseButton>,
) -> InputFrame {
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
    let shift_mod = down(KeyCode::KeyR);
    let mut fire_r = down(KeyCode::KeyF) || mouse.pressed(MouseButton::Right);
    let mut fire_l = down(KeyCode::KeyC) || mouse.pressed(MouseButton::Left);
    let mut fire_shoulder = down(KeyCode::KeyG) || down(KeyCode::KeyX) || down(KeyCode::Tab);
    let fire_ow = down(KeyCode::KeyU);
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
        fire_r |= pad.pressed(GamepadButton::RightTrigger2);
        fire_l |= pad.pressed(GamepadButton::LeftTrigger2);
        fire_shoulder |= pad.pressed(GamepadButton::RightTrigger);
    }
    // While the bay-shift modifier is held, the hand triggers exchange that arm's rack weapon
    // instead of firing. The server resolves the 150-tick swap from these bits.
    let bay_r = shift_mod && fire_r;
    let bay_l = shift_mod && fire_l;
    if shift_mod {
        fire_r = false;
        fire_l = false;
    }
    InputFrame {
        stick: stick.clamp_length_max(1.0),
        turn: turn.clamp(-1.0, 1.0),
        look: look.clamp(-1.0, 1.0),
        jump,
        boost_toggle,
        glide,
        quick_boost,
        qb_held,
        kick,
        fire_r,
        fire_l,
        fire_shoulder,
        bay_r,
        bay_l,
        fire_ow,
        ..InputFrame::default()
    }
}

/// The state this frame's movement shows, given the one playing.
fn state(p: &Pilot, input: &InputFrame, motion: &Motion) -> (&'static str, Option<u8>) {
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
            (false, true, _, BladePhase::Swing | BladePhase::Recovery, false) => {
                "blade_air_swing_l"
            }
            (false, false, _, BladePhase::Charge | BladePhase::Dash, true) => "blade_charge_r",
            (false, false, _, BladePhase::Charge | BladePhase::Dash, false) => "blade_charge_l",
            (false, false, _, BladePhase::Swing | BladePhase::Recovery, true) => "blade_swing_r",
            (false, false, _, BladePhase::Swing | BladePhase::Recovery, false) => "blade_swing_l",
            (true, true, true, BladePhase::Swing | BladePhase::Recovery, true) => {
                "phys_blade_down_r"
            }
            (true, true, true, BladePhase::Swing | BladePhase::Recovery, false) => {
                "phys_blade_down_l"
            }
            (true, true, _, BladePhase::Charge | BladePhase::Dash, true) => {
                "phys_blade_air_charge_r"
            }
            (true, true, _, BladePhase::Charge | BladePhase::Dash, false) => {
                "phys_blade_air_charge_l"
            }
            (true, true, _, BladePhase::Swing | BladePhase::Recovery, true) => {
                "phys_blade_air_swing_r"
            }
            (true, true, _, BladePhase::Swing | BladePhase::Recovery, false) => {
                "phys_blade_air_swing_l"
            }
            (true, false, _, BladePhase::Charge | BladePhase::Dash, true) => "phys_blade_charge_r",
            (true, false, _, BladePhase::Charge | BladePhase::Dash, false) => "phys_blade_charge_l",
            (true, false, _, BladePhase::Swing | BladePhase::Recovery, true) => {
                "phys_blade_swing_r"
            }
            (true, false, _, BladePhase::Swing | BladePhase::Recovery, false) => {
                "phys_blade_swing_l"
            }
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
    if current.0 == "quick_boost" && p.clip_ok && !motion.finished() && !p.glide && p.takeoff <= 0.0
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
        "turn_left" => Some(248),              // anim 226
        "turn_right" => Some(249),             // anim 227
        "dash" | "glide" => Some(268),         // anim 228: OW展開時 地上ブースト
        "air_move" | "glide_air" => Some(269), // anim 229: OW展開時 空中ブースト
        "jump" => Some(258),                   // anim 232
        "rise" => Some(259),                   // anim 233
        "fall" => Some(260),                   // anim 234
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

/// Mouse motion + buttons grouped to keep `pilot` under Bevy's 16-parameter limit.
#[derive(bevy::ecs::system::SystemParam)]
pub struct Pointer<'w> {
    motion: Res<'w, bevy::input::mouse::AccumulatedMouseMotion>,
    buttons: Res<'w, ButtonInput<MouseButton>>,
}

#[derive(bevy::ecs::system::SystemParam)]
pub struct PilotWorld<'w, 's> {
    collision: ResMut<'w, Collision>,
    props: Query<'w, 's, &'static mut crate::map::DestructibleProp>,
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
    mut world: PilotWorld,
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
    pointer: Pointer,
    active_input: Res<crate::input::ActiveInput>,
    mut net: Option<ResMut<crate::net::NetRuntime>>,
) {
    let collision = &mut *world.collision;
    let props = &mut world.props;
    let area_bounds = &world.area_bounds;
    let water_return = &world.water_return;
    let start = &world.start;
    if keys.just_pressed(KeyCode::KeyP) {
        piloting.0 = !piloting.0;
    }
    // Mouselook follows the last-used device: mouse+keyboard grabs and hides the cursor and lets
    // the mouse turn the AC; controller input releases it. `input::track_device` switches modes
    // automatically, so `M` is no longer a toggle.
    let mouselook = piloting.0 && !active_input.is_gamepad();
    if let Ok(mut c) = cursor.single_mut() {
        let (grab, visible) = if mouselook {
            (bevy::window::CursorGrabMode::Locked, false)
        } else {
            (bevy::window::CursorGrabMode::None, false)
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
    let mut input = read_input(&keys, &held.0, &pads, &pointer.buttons);
    // Ready-position aim turn speed scaling (`0x8287b658` -> `cat-10 +0x16c`, `0.18..0.5`).
    let zoom_turn_scale = match p.ready_cam {
        ReadyCamera::Zoomed { turn_scale, .. } => turn_scale.clamp(0.1, 1.0),
        _ => 1.0,
    };
    input.turn *= zoom_turn_scale;
    input.look *= zoom_turn_scale;
    if !mouselook {
        p.cam.yaw_off = 0.0;
    }
    if mouselook {
        // AC6-style: the mouse moves the camera freely; the AC turns toward it at its turn rate.
        p.cam.yaw_off = (p.cam.yaw_off - pointer.motion.delta.x * MOUSE_YAW * zoom_turn_scale)
            .clamp(-std::f32::consts::PI, std::f32::consts::PI);
        p.cam.pitch = (p.cam.pitch - pointer.motion.delta.y * MOUSE_PITCH * zoom_turn_scale)
            .clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }
    let net_mode = net.is_some();
    if !net_mode {
        if input.boost_toggle {
            p.boost = !p.boost;
        }
        p.boost |= held.0.contains(&KeyCode::KeyV);
    }
    p.command = input.stick;
    if !net_mode && p.qb.is_none() && p.wj.is_none() && p.kick.is_none() && p.blade.is_none() {
        let starting_glide = input.glide && p.boost && !p.airborne && p.en >= p.ctrl.glide_en_tick;
        if starting_glide && !p.glide {
            // `GlideState` is constructed at activation with the current height and a zeroed
            // phase timer (`0x82844fd8` -> `0x8283ddc8`); `0x82841b38` counts 0.5 s before it
            // calls the air move (`0x83713a94`).
            p.glide_phase = GlidePhase::Ground;
            p.glide_phase_time = 0.0;
            p.glide_lift_started = false;
        }
        p.glide |= starting_glide;
        p.jump_queued |= input.jump && !p.airborne;
        p.wall_jump_queued |= input.jump && p.airborne;
    }
    // Re-fire is gated by `HighBoostState` ending (`0x8200d378` slot `+0x50` = `0x8241a6b8`) and
    // `action_ctrl +0x110` (`qb_reload_ticks`, `0x8283ed98`), not the animation clip length.
    // A Hi-Boost tap during a glide is the glide quick boost (the glide top-level action keeps the
    // glide alive); holding it through Phase 2 is the Boost Charge Kick.
    if !net_mode && input.quick_boost {
        let from_glide = p.glide;
        acvd_sim::motion::start_quick_boost(&mut p.motion, from_glide);
    }
    p.prev_position = transform.translation;
    let yaw = p.yaw;
    if let Some(net) = net.as_mut() {
        // Online: publish the sampled frame and absolute aim; `net::step_prediction` owns the
        // 60 Hz motion step and reconciliation.
        p.cam.aim(input.look);
        let per_tick = (p.ctrl.turn_rate / TICK_RATE).max(1e-4);
        let aim_yaw = if mouselook {
            p.yaw + p.cam.yaw_off
        } else {
            p.yaw + p.cam.yaw_off + input.turn * per_tick
        };
        net.input = crate::net::NetInput {
            frame: input,
            aim_yaw,
            aim_pitch: p.cam.pitch,
        };
    } else {
        p.accumulator = (p.accumulator + time.delta_secs()).min(10.0 / TICK_RATE);
        while p.accumulator >= 1.0 / TICK_RATE {
            p.accumulator -= 1.0 / TICK_RATE;
            p.cam.aim(input.look);
            p.sync_aim();
            let before = p.velocity;
            let mut apply_prop = |prop_id: u32, raw_damage: f32| {
                for mut prop in props.iter_mut() {
                    if prop.prop_id.0 == prop_id {
                        return prop.apply_collision_damage(raw_damage);
                    }
                }
                false
            };
            if mouselook {
                let per_tick = (p.ctrl.turn_rate / TICK_RATE).max(1e-4);
                input.turn = (p.cam.yaw_off / per_tick).clamp(-1.0, 1.0);
                let yaw_before = p.yaw;
                acvd_sim::motion::step_tick_with_props(
                    &mut p.motion,
                    &input,
                    &mut transform.translation,
                    collision,
                    &mut apply_prop,
                );
                p.cam.yaw_off -= p.yaw - yaw_before;
            } else {
                acvd_sim::motion::step_tick_with_props(
                    &mut p.motion,
                    &input,
                    &mut transform.translation,
                    collision,
                    &mut apply_prop,
                );
            }
            p.cam.accel = (p.velocity - before).with_y(0.0).length() * TICK_TO_KMH;
        }
    }
    // Keep mouselook `input.turn` consistent on render frames where the 60 Hz accumulator did not
    // tick (e.g. >60 FPS monitors), so `state()` doesn't flap between `turn_*` and `idle`.
    if mouselook {
        let per_tick = (p.ctrl.turn_rate / TICK_RATE).max(1e-4);
        input.turn = if p.cam.yaw_off.abs() > 0.03 {
            (p.cam.yaw_off / per_tick).clamp(-1.0, 1.0)
        } else {
            0.0
        };
    }
    p.area_zone = if net_mode {
        crate::map::AreaZone::Inside
    } else {
        area_bounds
            .as_deref()
            .map_or(crate::map::AreaZone::Inside, |b| {
                b.enforce(&mut transform.translation, &mut p.velocity)
            })
    };
    if !net_mode {
        if let (Some(wr), Some(st)) = (water_return.as_deref(), start.as_deref()) {
            if acvd_sim::motion::check_water_return(
                &mut p.motion,
                &mut transform.translation,
                &wr.0,
                st.0,
                collision,
            ) {
                p.cam.yaw_off = 0.0;
                p.cam.pitch = 0.0;
                info!(
                    "water return triggered -> respawned at ({:.1}, {:.1}, {:.1})",
                    transform.translation.x, transform.translation.y, transform.translation.z
                );
            }
        }
    }
    if let Some(status) = status.as_deref_mut() {
        status.en = p.en;
        status.en_max = p.en_max;
    }
    transform.rotation = Quat::from_rotation_y(p.yaw);

    // The walk / dash clip direction is relative to the body, the stick to the camera.
    let body = InputFrame {
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
        if mouselook { 0.0 } else { input.turn },
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
        let cam_turn = if mouselook && input.turn.abs() < 0.25 {
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
        if mouselook {
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
        let (eye, look_at) = p
            .cam
            .clip(root + waist, eye, look_at, collision, time.delta_secs());
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
            if p.glide {
                match p.glide_phase {
                    GlidePhase::Ground => "glide",
                    GlidePhase::Air => "glide_air",
                }
            } else if p.boost {
                "on"
            } else {
                "off"
            },
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
        assert!(
            peak_impact > 0.0,
            "impact shake must produce non-zero offset"
        );
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
        assert_eq!(
            camera_action("idle", true, false, ReadyCamera::Deploying),
            40
        );
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
        assert_eq!(
            cam.blur, 0.0,
            "blur must not step after a single 1/60 s frame"
        );

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
    fn camera_clip_stops_at_wall_ignores_camera_material_and_eases_out() {
        use crate::collision::{GroundSurface, Hit, Layer, Plane};
        let wall = |z: f32, blocks_camera: bool| {
            let tri = |a: [f32; 3], b: [f32; 3], c: [f32; 3]| Hit {
                a: Vec3::from(a),
                b: Vec3::from(b),
                c: Vec3::from(c),
                layer: Layer::Ground,
                surface: GroundSurface::Concrete,
                wall_kickable: false,
                blocks_fcs_los: true,
                blocks_ai_los: true,
                blocks_bullet: true,
                blocks_camera,
                prop_id: 0,
                only_when_broken: false,
            };
            vec![
                tri([-50.0, 0.0, z], [50.0, 0.0, z], [-50.0, 50.0, z]),
                tri([50.0, 0.0, z], [50.0, 50.0, z], [-50.0, 50.0, z]),
            ]
        };
        let pivot = Vec3::new(0.0, 4.0, 0.0);
        let eye = Vec3::new(0.0, 4.0, -20.0);
        let look = Vec3::new(0.0, 4.0, -1020.0);
        let dt = 1.0 / 60.0;

        // A `camera`-material wall (`0x08`) is excluded by the 360 filter; the solid wall behind
        // it stops the eye 0.3 m short. (Camera wall at -4 is nearer and must be ignored.)
        let mut hits = wall(-4.0, false);
        hits.extend(wall(-8.0, true));
        let col = Collision::with_hits(
            vec![Plane {
                y: 0.0,
                layer: Layer::Ground,
            }],
            hits,
        );
        let mut cam = FollowCam::of(10);
        let (clipped, look_out) = cam.clip(pivot, eye, look, &col, dt);
        let dist = (clipped - pivot).length();
        assert!(
            (dist - 7.7).abs() < 1e-3,
            "solid wall at 8 m must stop the eye at 7.7 m, got {dist}"
        );
        let before = (look - eye).normalize();
        let after = (look_out - clipped).normalize();
        assert!((after - before).length() < 1e-5, "view direction changed");

        // The wall clears: the eye eases back out instead of snapping to the full 20 m.
        let clear = Collision::default();
        let (out, _) = cam.clip(pivot, eye, look, &clear, dt);
        let out_dist = (out - pivot).length();
        assert!(
            out_dist > dist && out_dist < 20.0,
            "released eye must ease out, got {out_dist}"
        );

        // A lone camera-material wall never clips the eye.
        let mut cam2 = FollowCam::of(10);
        let camera_only = Collision::with_hits(Vec::new(), wall(-4.0, false));
        let (free, _) = cam2.clip(pivot, eye, look, &camera_only, dt);
        assert!(
            (free - eye).length() < 1e-4,
            "camera material must not clip"
        );
    }
}
