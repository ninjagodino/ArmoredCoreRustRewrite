//! Piloting the shown AC, with the pad layout of the game's manual (`lang/en/text/menu/manual.fmg`):
//! WASD / left stick move relative to its facing, Q/E / right stick X turn, Up/Down / right stick Y
//! pitch the follow camera, Shift / L1 toggles boost mode, Space / South (×) jumps and turns boost
//! mode on, Ctrl / L3 glides while boosting on the ground. Movement steps at the game's 60 Hz tick
//! in metres per tick with the AC's `AcCtrlParam`
//! (`sheets/ac_ctrl_calc.csv`: its parts and build weight through the game's
//! `AcCtrlParamCalc.lua`), and every step plays the clip of the `sheets/ac_states.csv` state it
//! lands in, through that state's `param/acmotion.bin` row.

use acvd_data::generated::ac_unit::{MotionCollateSt, PARAM_ACANIMHOKAN_BIN, PARAM_ACMOTION_BIN, PARAM_MOTIONCOLLATE_BIN};
use acvd_data::generated::camera::{
    AcCameraActionSt, AccamBehaviorParamSt, PARAM_ACCAMBEHAVIORPARAM_BIN, PARAM_ACCAMERAACTION_BIN, PARAM_ACCAMERAOFFSETPARAM_BIN,
};
use acvd_data::generated::ctrl::AcCtrlParam;
use acvd_data::generated::tuning::AC_ANIM_PARAM;
use acvd_data::{find, Row};
use acvd_render::app::Orbit;
use bevy::prelude::*;

use crate::blur::ZoomBlur;
use crate::collision::{Collision, Layer, RAY_HALF};
use crate::pose::{Driven, Fade, Motion};

/// Movement ticks per second; `AcCtrlParam`'s `_tick` values are per tick.
const TICK_RATE: f32 = 60.0;
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

/// Whether input drives the AC (`P` toggles) instead of browsing clips.
#[derive(Resource)]
pub struct Piloting(pub bool);

/// Keys held for the whole run (`--hold w,shift`), for scripted checks.
#[derive(Resource, Default)]
pub struct Held(pub Vec<KeyCode>);

/// Where an unturned AC faces: the assembled models' fronts point down -Z in Bevy axes.
const FORWARD: Vec3 = Vec3::NEG_Z;

/// Movement state of the AC root. Velocity is in metres per tick, Bevy axes; `yaw` turns
/// about +Y (positive turns left), 0 facing `FORWARD`.
#[derive(Component)]
pub struct Pilot {
    pub ctrl: AcCtrlParam,
    pub velocity: Vec3,
    pub yaw: f32,
    pub boost: bool,
    pub glide: bool,
    pub airborne: bool,
    /// Ticks of take-off acceleration left.
    takeoff: f32,
    jump_queued: bool,
    /// Touched down during the last steps.
    landed: bool,
    /// The state playing, and whether its clip is in the set.
    state: Option<(&'static str, Option<u8>)>,
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
    /// The `accameraaction` row in use and its id.
    action: Option<(u32, &'static AcCameraActionSt)>,
    /// Behavior ShakeMoveAmpl / ShakeMoveFreq x, y (the "freq" is a period in seconds), the
    /// shake's running phase, and the AC's change in horizontal speed over the last tick (km/h).
    shake_ampl: Vec2,
    shake_period: Vec2,
    shake_phase: f32,
    accel: f32,
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
    /// Roll from sideways speed: ZTiltStart/MaxXSpeed (km/h), MaxZTiltAngle (rad), the seconds to
    /// tilt and to level out, and the -1..1 tilt in use.
    tilt_speeds: (f32, f32),
    tilt_max: f32,
    tilt_secs: (f32, f32),
    tilt: Blend,
    /// Behavior row 0, for the speed blur's Blur* fields, and the blur's eased 0..1 intensity.
    behavior: &'static AccamBehaviorParamSt,
    blur: f32,
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
        self.value = if self.duration > 0.0 { self.from + (self.to - self.from) * self.t / self.duration } else { self.to };
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
        *self = Self { rise, hold, fall, active: true, from, to, out: self.out, ..Self::default() };
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
        self.out = if self.t < self.rise { self.from + (self.to - self.from) * self.level } else { self.to * self.level };
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
/// walk 2, turn 3, flight 8, landing 9; boost mode turns the rest into 38 / 39 (in the air).
fn camera_action(state: &str, boost: bool, airborne: bool) -> u32 {
    let base = match state {
        "idle" => 0,
        "walk" | "dash" => 2,
        "turn_left" | "turn_right" => 3,
        "land" => 9,
        _ => 8,
    };
    if boost && !BOOST_KEEPS.contains(&base) { 38 + u32::from(airborne) } else { base }
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
fn ease(shown: Vec3, last_target: Vec3, target: Vec3, rates: [f32; 4], push: Vec2, frame: Quat, dt: f32) -> Vec3 {
    let frames = dt * TICK_RATE;
    if frames <= 0.0 {
        return shown;
    }
    let step = (target - last_target) / frames;
    let whole = frames.floor();
    let frac = frames - whole;
    let close = |r: f32, t: f32| if t >= 1.0 || r >= 1.0 { r } else { 1.0 - (1.0 - r).powf(t) };
    let mut goal = last_target;
    let mut shown = shown;
    let advance = |goal: Vec3, shown: &mut Vec3, t: f32| {
        let d = frame.inverse() * (goal - *shown);
        let z = if d.z < 0.0 { rates[2] } else { rates[3] };
        *shown += frame * Vec3::new(d.x * close(rates[0], t) + push.x * t, d.y * close(rates[1], t) + push.y * t, d.z * close(z, t));
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
    PARAM_MOTIONCOLLATE_BIN.iter().find(|r| r.data.arms_motion_id == 0 && r.data.legs_motion_id == legs_motion_id && r.data.ow_motion_id == 0)
}

impl FollowCam {
    /// Row 0 of `accambehaviorparam` with the `accameraoffsetparam` row of the legs' motion set
    /// (`motioncollate` without arms or OW motion).
    fn of(legs_motion_id: u8) -> Self {
        let behavior = &find(PARAM_ACCAMBEHAVIORPARAM_BIN, 0).expect("accambehaviorparam row 0").data;
        let offset = collate(legs_motion_id).and_then(|r| find(PARAM_ACCAMERAOFFSETPARAM_BIN, u32::from(r.data.camera_param_id)));
        let (eye_dist_offset, eye_min_dist, lift_rate, shift_rate, approach, down_rate) = match offset {
            Some(o) => {
                let o = &o.data;
                (o.eye_dist_offset, o.eye_min_dist, o.eye_offset_y_offset_rate, o.eye_offset_x_offset_rate, o.eye_approach_rate, o.eye_down_rate)
            }
            None => {
                warn!("no accameraoffsetparam row for legs motion {legs_motion_id}");
                (0.0, 0.0, 1.0, 1.0, behavior.eye_approach_rate, behavior.eye_down_rate)
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
            shake_ampl: Vec2::new(behavior.shake_move_ampl_x, behavior.shake_move_ampl_y),
            shake_period: Vec2::new(behavior.shake_move_freq_x, behavior.shake_move_freq_y),
            shake_phase: 0.0,
            accel: 0.0,
            eye_rates: [
                [behavior.eye_follow_rate_x, behavior.fly_eye_follow_rate_y, behavior.eye_follow_rate_f, behavior.eye_follow_rate_b],
                [behavior.fly_eye_follow_rate_x, behavior.fly_eye_follow_rate_y, behavior.fly_eye_follow_rate_f, behavior.fly_eye_follow_rate_b],
            ],
            look_at_rates: [
                [behavior.look_at_follow_rate_x, behavior.fly_look_at_follow_rate_y, behavior.look_at_follow_rate_f, behavior.look_at_follow_rate_b],
                [behavior.fly_look_at_follow_rate_x, behavior.fly_look_at_follow_rate_y, behavior.fly_look_at_follow_rate_f, behavior.fly_look_at_follow_rate_b],
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
            tilt_speeds: (behavior.z_tilt_start_x_speed, behavior.z_tilt_max_x_speed),
            tilt_max: behavior.max_z_tilt_angle.to_radians(),
            tilt_secs: (f32::from(behavior.z_tilt_interpolate_frame) / TICK_RATE, f32::from(behavior.z_tilt_end_interpolate_frame) / TICK_RATE),
            tilt: Blend::default(),
            behavior,
            blur: 0.0,
            shown: None,
        }
    }

    /// One tick of the speed blur (360 0x82bfa9f0 / 0x82bf9c98): the horizontal and vertical
    /// speeds (km/h) each give (speed - BlurStartSpeed) / (BlurMaxSpeed - BlurStartSpeed) above
    /// the start; their sum, clamped to 0..1 and scaled by the camera action's BlurRate, is
    /// approached by BlurIntpRate of the gap, and the intensity stays within 0..1.
    fn blur_tick(&mut self, velocity: Vec3) {
        let b = self.behavior;
        let ramp = |speed: f32, start: f32, max: f32| if speed > start && start < max { (speed - start) / (max - start) } else { 0.0 };
        let h = ramp(velocity.with_y(0.0).length() * TICK_TO_KMH, b.blur_start_speed, b.blur_max_speed);
        let v = ramp(velocity.y.abs() * TICK_TO_KMH, b.blur_start_speed_v, b.blur_max_speed_v);
        let rate = self.action.map_or(0.0, |(_, a)| a.blur_rate);
        let target = (h + v).clamp(0.0, 1.0) * rate;
        self.blur = (self.blur + (target - self.blur) * b.blur_intp_rate).clamp(0.0, 1.0);    }

    /// The zoom-blur filter for this intensity (360 0x82bfa9f0, 0x82bf8c20); the filter skips
    /// drawing at alpha 9 or less (360 0x82c98368).
    fn zoom_blur(&self) -> ZoomBlur {
        let b = self.behavior;
        let i = self.blur;
        let alpha = ((b.blur_alpha as f32 * i) as i32).clamp(0, 255) as u8;
        let alpha = if alpha > BLUR_MIN_ALPHA { alpha } else { 0 };
        ZoomBlur::new(b.blur_offset * i, alpha, b.blur_thin_pow * i, Vec2::new(b.blur_no_effect_size_x, b.blur_no_effect_size_y))
    }

    /// Rotates the eased camera points about pivot by delta (yaw), so freelook turns the view
    /// rigidly instead of lagging behind it; only movement is left to the follow easing.
    fn swing(&mut self, pivot: Vec3, delta: f32) {
        if let Some(s) = self.shown.as_mut() {
            let r = Quat::from_rotation_y(delta);
            for v in [&mut s.eye, &mut s.look_at, &mut s.eye_target, &mut s.look_at_target] {
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
            (((side.abs() - start) / (max - start)).min(1.0).copysign(side), self.tilt_secs.0)
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
            let h = Vec3::new(f.x + (flat.x - f.x) * t, self.down_rate * f.y, f.z + (flat.z - f.z) * t);
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
    /// old row's FOV fades over FovFadeFrame, the new one rises to FovMaxAngle over FovMaxFrame
    /// and holds for FovKeepFrame (-1 = for good).
    fn act(&mut self, action: u32) {
        if self.action.is_some_and(|(id, _)| id == action) {
            return;
        }
        let Some(row) = find(PARAM_ACCAMERAACTION_BIN, action).map(|r| &r.data) else { return };
        let secs = |frames: i16| f32::from(frames) / TICK_RATE;
        if let Some((_, old)) = self.action.filter(|(_, old)| old.fov_enable != 0) {
            self.fov.fade(secs(old.fov_fade_frame));
        }
        if row.fov_enable != 0 {
            let to = f32::from(row.fov_max_angle) - self.base_fov;
            self.fov.start(to, secs(row.fov_max_frame), secs(row.fov_keep_frame), secs(row.fov_fade_frame));
        }
        self.action = Some((action, row));
    }

    /// Eases the eye distance and lift toward the camera action's (360 eye blender 0x8285f1a8:
    /// targets 0x8285d7f8 / 0x8285d8c8 over EyeFadeInFrame, set outright on its first update).
    fn blend_eye(&mut self, turn: f32, dt: f32) {
        let a = self.action.map(|(_, a)| a);
        let dist = a.map(|a| a.eye_distance).filter(|&d| d > ACTION_DIST_MIN).unwrap_or(self.behavior.eye_dist);
        let lift = a.map_or(0.0, |a| a.eye_offset_y);
        let shift = a.map_or(0.0, |a| a.eye_offset_x);
        let secs = a.map_or(0.0, |a| f32::from(a.eye_fade_in_frame) / TICK_RATE);
        let side_secs = a.map_or(NO_ACTION_SIDE_FRAMES, |a| f32::from(a.turn_offset_move_frame)) / TICK_RATE;
        if a.is_some_and(|a| a.b_trun_move_enable != 0) && turn != 0.0 {
            self.side_sign = -turn.signum();
        }
        if self.shown.is_none() {
            let set = |v: f32| Blend { value: v, to: v, ..Blend::default() };
            (self.eye_dist, self.lift, self.shift, self.side) = (set(dist), set(lift), set(shift), set(self.side_sign));
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

    /// Move shake (360 0x82bfab38): weight ShakeNoMoveRate, plus ShakeAccelRate x where the
    /// speed change sits between ShakeAccelMin and Max when Min < Max, clamped to 0..1 (360 0x82860100).
    fn shake(&mut self, dt: f32) -> Vec2 {
        let weight = self.action.map_or(0.0, |(_, a)| {
            if a.shake_accel_min < a.shake_accel_max {
                let t = (self.accel - a.shake_accel_min) / (a.shake_accel_max - a.shake_accel_min);
                (t * a.shake_accel_rate + a.shake_no_move_rate).clamp(0.0, 1.0)
            } else {
                a.shake_no_move_rate
            }
        });
        if weight <= 0.0 {
            self.shake_phase = 0.0;
            return Vec2::ZERO;
        }
        self.shake_phase += dt;
        shake_wave(self.shake_phase, self.shake_ampl, self.shake_period) * weight
    }

    /// The shown eye and look-at for this frame's targets.
    fn follow(&mut self, eye: Vec3, look_at: Vec3, frame: Quat, airborne: bool, dt: f32) -> (Vec3, Vec3) {
        let air = usize::from(airborne);
        let secs = self.rate_blend_secs;
        let action = self.action.map(|(_, a)| [a.eye_follow_rate_x, a.eye_follow_rate_y, a.eye_follow_rate_f, a.eye_follow_rate_b]);
        let eye_rates: [f32; 4] = std::array::from_fn(|i| {
            let target = action.map_or(0.0, |a| a[i]);
            let target = if target < ACTION_RATE_MIN { self.eye_rates[air][i] } else { target };
            self.eye_rate[i].toward(target, secs, dt)
        });
        let look_at_rates: [f32; 4] = std::array::from_fn(|i| self.look_at_rate[i].toward(self.look_at_rates[air][i], secs, dt));
        let push = self.shake(dt);
        let shown = match self.shown {
            None => Shown { eye, look_at, eye_target: eye, look_at_target: look_at },
            Some(s) => Shown {
                eye: ease(s.eye, s.eye_target, eye, eye_rates, push, frame, dt),
                look_at: ease(s.look_at, s.look_at_target, look_at, look_at_rates, push, frame, dt),
                eye_target: eye,
                look_at_target: look_at,
            },
        };
        self.shown = Some(shown);
        (shown.eye, shown.look_at)
    }
}

impl Pilot {
    /// A standing AC moving by `ctrl`, with the default follow camera for its legs' motion set.
    pub fn new(ctrl: AcCtrlParam, legs_motion_id: u8) -> Self {
        Self {
            ctrl,
            velocity: Vec3::ZERO,
            yaw: 0.0,
            boost: false,
            glide: false,
            airborne: false,
            takeoff: 0.0,
            jump_queued: false,
            landed: false,
            state: None,
            clip_ok: false,
            accumulator: 0.0,
            hokan_base: collate(legs_motion_id).map_or(0, |r| r.data.hokan_param_id.into()),
            cam: FollowCam::of(legs_motion_id),
        }
    }

    /// Fire direction: the follow camera's pitched forward (`FollowCam::place`); core aim is not
    /// posed yet.
    pub fn aim_direction(&self) -> Vec3 {
        Quat::from_rotation_y(self.yaw + self.cam.yaw_off) * Quat::from_rotation_x(self.cam.pitch) * FORWARD
    }
}

/// Mouselook (M): camera yaw / pitch radians per pixel of mouse motion.
const MOUSE_YAW: f32 = 0.003;
const MOUSE_PITCH: f32 = 0.003;

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
}

fn read_input(keys: &ButtonInput<KeyCode>, held: &[KeyCode], pads: &Query<&Gamepad>) -> Input {
    let down = |k: KeyCode| keys.pressed(k) || held.contains(&k);
    let axis = |pos: KeyCode, neg: KeyCode| f32::from(u8::from(down(pos))) - f32::from(u8::from(down(neg)));
    let mut stick = Vec2::new(axis(KeyCode::KeyD, KeyCode::KeyA), axis(KeyCode::KeyW, KeyCode::KeyS));
    let mut turn = axis(KeyCode::KeyQ, KeyCode::KeyE);
    let mut look = axis(KeyCode::ArrowUp, KeyCode::ArrowDown);
    let mut jump = keys.just_pressed(KeyCode::Space) || held.contains(&KeyCode::Space);
    let mut boost_toggle = keys.just_pressed(KeyCode::ShiftLeft) || keys.just_pressed(KeyCode::ShiftRight);
    let mut glide = keys.just_pressed(KeyCode::ControlLeft) || held.contains(&KeyCode::ControlLeft);
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
    }
    Input { stick: stick.clamp_length_max(1.0), turn: turn.clamp(-1.0, 1.0), look: look.clamp(-1.0, 1.0), jump, boost_toggle, glide }
}

/// Eighth of the circle `stick` points at, 0 forward, clockwise.
fn direction(stick: Vec2) -> u8 {
    let angle = stick.x.atan2(stick.y).rem_euclid(std::f32::consts::TAU);
    ((angle / std::f32::consts::FRAC_PI_4).round() as u8) % 8
}

fn approach(v: Vec2, target: Vec2, step: f32) -> Vec2 {
    let d = target - v;
    if d.length() <= step { target } else { v + d.normalize() * step }
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

/// One 60 Hz tick of movement over the highest ground under the AC.
fn step(p: &mut Pilot, input: &Input, position: &mut Vec3, collision: &Collision) {
    let c = p.ctrl;
    p.yaw += input.turn * c.turn_rate / TICK_RATE;
    // Mouselook: move relative to where the camera looks, not where the body faces.
    let facing = Quat::from_rotation_y(p.yaw + p.cam.yaw_off);
    let forward = facing * FORWARD;
    let right = forward.cross(Vec3::Y);
    let wish3 = right * input.stick.x + forward * input.stick.y;
    let wish = Vec2::new(wish3.x, wish3.z);
    let mut horizontal = Vec2::new(p.velocity.x, p.velocity.z);
    let speed = horizontal.length();
    let moving = wish != Vec2::ZERO;
    p.glide &= p.boost && moving && !p.airborne;
    let brake = if speed >= c.brake_switch_tick { c.hispeed_brake_tick } else { c.lospeed_brake_tick };

    if !p.airborne {
        let (max, accel) = if p.glide {
            (c.glide_max_tick, c.glide_acc_tick)
        } else if p.boost {
            (c.boost_max_tick, c.boost_acc_tick)
        } else {
            (c.walk_max_tick, c.walk_acc_tick)
        };
        horizontal = if p.glide {
            approach(horizontal, wish * max, if speed <= max { accel } else { brake })
        } else if moving {
            integrate(horizontal, wish * accel, max, c.over_max_decel_tick)
        } else {
            approach(horizontal, Vec2::ZERO, brake)
        };
        if p.jump_queued {
            (p.airborne, p.takeoff, p.boost, p.glide) = (true, c.jump_frames, true, false);
        }
    } else if p.takeoff > 0.0 {
        horizontal += wish * c.jump_h_acc_tick;
    } else if p.boost && moving {
        horizontal = integrate(horizontal, wish * c.boost_acc_tick, c.boost_max_tick, c.over_max_decel_tick);
    } else {
        let drag = if p.boost { c.air_drag_boost_tick } else { c.air_drag_tick };
        horizontal *= (1.0 - drag).clamp(0.0, 1.0);
    }
    p.jump_queued = false;

    if p.airborne {
        if p.takeoff > 0.0 {
            p.velocity.y += c.jump_accel_tick * p.takeoff.min(1.0);
            p.takeoff = (p.takeoff - 1.0).max(0.0);
        } else {
            // Boost gravity (movement +0x284) only while boost mode is on and the AC falls; it
            // rises under full gravity (Xenia probe private/xenia/vertical.txt).
            p.velocity.y -= if p.boost && p.velocity.y < 0.0 { c.boost_gravity_tick } else { c.gravity_tick };
        }
        p.velocity.y = p.velocity.y.max(-c.fall_max_tick);
    }
    p.velocity = Vec3::new(horizontal.x, p.velocity.y, horizontal.y);
    *position += p.velocity;
    // Snap puts y on the triangle, so a grounded ray has to start above it. The lift is this
    // tick's travel (slopes) or 5 cm when still — a hit farther down than that is a drop.
    let lift = if p.airborne { 0.0 } else { horizontal.length().max(0.05) };
    let ground = collision.ground_below(Vec3::new(position.x, position.y + lift, position.z));
    if p.airborne {
        if let Some(g) = ground.filter(|&g| position.y <= g && p.velocity.y <= 0.0) {
            (position.y, p.velocity.y, p.airborne, p.landed) = (g, 0.0, false, true);
        }
    } else {
        match ground.filter(|&g| g >= position.y - lift) {
            Some(g) => position.y = g,
            None => p.airborne = true,
        }
    }
}

/// The state this frame's movement shows, given the one playing.
fn state(p: &Pilot, input: &Input, motion: &Motion) -> (&'static str, Option<u8>) {
    let moving = input.stick != Vec2::ZERO;
    let dir = moving.then(|| direction(input.stick));
    let current = p.state.unwrap_or(("idle", None));
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
        None if input.turn > 0.0 => ("turn_left", None),
        None if input.turn < 0.0 => ("turn_right", None),
        None => ("idle", None),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn pilot(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    held: Res<Held>,
    pads: Query<&Gamepad>,
    mut piloting: ResMut<Piloting>,
    garage: Res<crate::Garage>,
    collision: Res<Collision>,
    motion: Option<ResMut<Motion>>,
    mut acs: Query<(&mut Pilot, &mut Transform, &GlobalTransform)>,
    mut cams: Query<(&mut Orbit, &mut Transform, &mut Projection, &mut ZoomBlur), Without<Pilot>>,
    joints: Query<(&Driven, &GlobalTransform)>,
    mut window: Query<&mut Window, With<bevy::window::PrimaryWindow>>,
    mut cursor: Query<&mut bevy::window::CursorOptions, With<bevy::window::PrimaryWindow>>,
    mouse: Res<bevy::input::mouse::AccumulatedMouseMotion>,
    mut mouselook: Local<bool>,
) {
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
    let (Some(mut motion), Ok((mut p, mut transform, ac_global))) = (motion, acs.single_mut()) else { return };
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
        (p.cam.action, p.cam.fov, p.cam.shake_phase, p.cam.accel, p.cam.blur) = (None, Envelope::default(), 0.0, 0.0, 0.0);
        if let Ok((mut o, _, _, mut blur)) = cams.single_mut() {
            *blur = ZoomBlur::default();
            if o.follow {
                o.follow = false;
                o.frame(garage.bounds.0, garage.bounds.1);
            }
        }
        return;
    }
    let mut input = read_input(&keys, &held.0, &pads);
    if !*mouselook {
        p.cam.yaw_off = 0.0;
    }
    if *mouselook {
        // AC6-style: the mouse moves the camera freely; the AC turns toward it at its turn rate.
        p.cam.yaw_off = (p.cam.yaw_off - mouse.delta.x * MOUSE_YAW).clamp(-std::f32::consts::PI, std::f32::consts::PI);
        p.cam.pitch = (p.cam.pitch - mouse.delta.y * MOUSE_PITCH).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }
    if input.boost_toggle {
        p.boost = !p.boost;
    }
    p.boost |= held.0.contains(&KeyCode::ShiftLeft);
    p.glide |= input.glide && p.boost && !p.airborne;
    p.jump_queued |= input.jump && !p.airborne;
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
        let velocity = p.velocity;
        p.cam.blur_tick(velocity);
    }
    transform.rotation = Quat::from_rotation_y(p.yaw);

    // The walk / dash clip direction is relative to the body, the stick to the camera.
    let (sin, cos) = p.cam.yaw_off.sin_cos();
    let body = Input {
        stick: Vec2::new(input.stick.x * cos - input.stick.y * sin, input.stick.x * sin + input.stick.y * cos),
        ..input
    };
    let next = state(&p, &body, &motion);
    p.landed = false;
    let changed = p.state != Some(next);
    if changed {
        p.clip_ok = match acvd_data::ac_state(next.0, next.1).and_then(|s| acvd_data::find(PARAM_ACMOTION_BIN, s.row)) {
            Some(row) => {
                let speed = if next.0.starts_with("turn_") { AC_ANIM_PARAM.turn_clip_speed } else { 1.0 };
                let hokan = p.hokan_base + u32::from(row.data.interpolate_id);
                let fade = find(PARAM_ACANIMHOKAN_BIN, hokan).map(|h| Fade::of(&h.data)).unwrap_or_else(|| {
                    warn!("no acanimhokan row {hokan}");
                    [0.0; 8]
                });
                motion.play(&garage.usrdir, row.data.anim_id, row.data.b_loop != 0, speed, fade).map_err(|e| debug!("{}: {e:#}", next.0)).is_ok()
            }
            None => {
                warn!("no ac_states.csv row for {next:?}");
                false
            }
        };
        debug!("state {next:?} -> {} at {:.1?}", motion.name(), transform.translation);
        p.state = Some(next);
    }

    // Dash / air-move lean: one 360-frame clip whose frame is the heading in degrees (a key per
    // 45 degrees = the eight directions), so it blends smoothly between them.
    if matches!(next.0, "dash" | "air_move") && motion.clip.frames == 360 && p.clip_ok {
        // Mirrored: the clip's 90 degree key leans the way the stick's left does (checked by eye).
        // Heading of the actual velocity (m/tick) against the body, not the stick: the AC's
        // acceleration (its weight) then sets how fast the lean swings between directions.
        // Falls back to the stick while nearly still.
        let forward = Quat::from_rotation_y(p.yaw) * FORWARD;
        let right = forward.cross(Vec3::Y);
        let v = Vec2::new(p.velocity.dot(right), p.velocity.dot(forward));
        let local = if v.length() > 0.02 { v } else { body.stick };
        let target = (-local.x).atan2(local.y).to_degrees().rem_euclid(360.0);
        if motion.wheel.is_none() {
            motion.frame = target;
        }
        motion.wheel = Some(target);
        // The lean builds with speed: 0 at a standstill, full at the boost top speed.
        motion.lean = (p.velocity.with_y(0.0).length() / p.ctrl.boost_max_tick.max(1e-4)).clamp(0.0, 1.0);
    }

    if let Ok((mut o, mut cam, mut proj, mut blur)) = cams.single_mut() {
        o.follow = true;
        o.yaw += p.yaw - yaw;
        let airborne = p.airborne;
        let action = camera_action(next.0, p.boost, airborne);
        p.cam.act(action);
        // Mouselook turning is auto-generated and jittery near zero: ignore small values so the
        // side shift only flips for a real turn (the blend itself smooths the move).
        let cam_turn = if *mouselook && input.turn.abs() < 0.25 { 0.0 } else { input.turn };
        p.cam.blend_eye(cam_turn, time.delta_secs());
        let root = transform.translation;
        let center = motion.skeleton.bones.iter().position(|b| b.rest.as_ref().is_some_and(|r| r.name == WAIST_BONE));
        let waist = joints.iter().find(|(d, _)| Some(d.bone) == center).map_or(Vec3::ZERO, |(_, g)| g.translation() - ac_global.translation());
        let water = collision.ray_down(Vec3::new(root.x, root.y + RAY_HALF, root.z), root.y - RAY_HALF, Layer::Water);
        let cam_rot = transform.rotation * Quat::from_rotation_y(p.cam.yaw_off);
        let total_yaw = p.yaw + p.cam.yaw_off;
        if *mouselook {
            let delta = total_yaw - p.cam.cam_yaw;
            p.cam.swing(root + waist, delta);
        }
        p.cam.cam_yaw = total_yaw;
        let (eye, look_at) = p.cam.place(root + waist, cam_rot, water);
        let (eye, look_at) = p.cam.follow(eye, look_at, cam_rot, airborne, time.delta_secs());
        o.focus = look_at;
        let velocity = p.velocity;
        let roll = p.cam.roll(velocity, transform.rotation, time.delta_secs());
        *cam = Transform::from_translation(eye).looking_at(look_at, Vec3::Y);
        cam.rotate_local_z(roll);
        if let Projection::Perspective(persp) = proj.as_mut() {
            persp.fov = p.cam.fov(time.delta_secs());
        }
        *blur = p.cam.zoom_blur();
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
        assert_eq!(integrate(Vec2::new(0.0, walk * 0.5), Vec2::new(0.0, walk), walk, decel).length(), walk);
    }
}
