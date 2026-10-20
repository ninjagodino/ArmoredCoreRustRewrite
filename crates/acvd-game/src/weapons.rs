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
    BULLET_BULLETENERGY_BIN, BULLET_BULLETRIGID_BIN, PARAM_BULLETHITSFXPARAM_BIN,
};
use acvd_data::generated::sfx::PARAM_ACWEAPONSFXPARAM_BIN;
use acvd_data::generated::sound::PARAM_ACWEAPONSOUNDPARAM_BIN;
use acvd_data::{ac_state, find, part_field};
use acvd_formats::vfs::{self, Disc};
use acvd_formats::{ani, jcon};
use acvd_render::Rig;
use bevy::audio::AudioSource;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::assemble::Placement;
use crate::collision::Collision;
use crate::control::{Held, Pilot, Piloting};
use crate::pose::{self, Driven, Motion};
use crate::sfx::{EffectPoint, Sfx};
use crate::sound::{self, Cues};

/// Hand-weapon muzzle effect point (FLVER dummy colour byte 1 on the arm weapons).
const MUZZLE_POINT: u8 = 101;
/// Uniform scale the muzzle effect plays at. Not game data: f0001218 as stored is a 16 m
/// flash (s4021 × 8 × 9.5) and a 20 m sprite, so the game must scale it at spawn; the scale's
/// source is not traced.
const MUZZLE_SCALE: f32 = 0.25;

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

/// The two hand weapons and the two bay weapons of the shown design.
/// `hands` / `bays` are `[right, left]`.
#[derive(Component)]
pub struct Armament {
    pub hands: [Gun; 2],
    pub bays: [Gun; 2],
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

#[derive(Clone, Copy, Debug)]
pub struct Gun {
    pub remaining: u16,
    pub magazine: u16,
    pub reload_time: f32,
    pub cooldown: f32,
    pub init_speed: f32,
    pub kind: Kind,
    pub flight: Flight,
    pub damage_power: f32,
    pub damage_recoil: f32,
    pub decay: RangeDecay,
    pub fx: Fx,
    /// `acweaponsoundparam.shoot`. `≤ 0` plays nothing.
    pub shoot: i16,
    /// Ready weapon (`ready_position`). Holding fire plays the body stance before the shot.
    /// A sniper rifle (`weapon_kind` 5) is not one of these.
    pub stance: bool,
    /// Sniper rifle (`weapon_kind` 5): shots kick with `sniper_$(LR)` instead of `gun_$(LR)`.
    pub sniper: bool,
}

impl Gun {
    /// Effective `(damage_power, damage_recoil)` at flight distance `dist` in metres
    /// (`0x827cf1c0` -> `0x827cee08` / `0x827cee18`).
    pub fn damage_at(&self, dist: f32) -> (f32, f32) {
        let mult = self.decay.multiplier(dist);
        (self.damage_power * mult, self.damage_recoil * mult)
    }
}

/// FFX effect ids of a weapon; 0 plays nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct Fx {
    pub bullet: i32,
    pub muzzle: i32,
    pub hit: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Rigid,
    Energy,
    Skip,
}

/// Weapon-root joint the shot leaves from.
#[derive(Component)]
pub struct Hardpoint {
    pub hand: Hand,
}

/// A weapon-clip joint. Body-driven joints keep the locomotion pose.
#[derive(Component)]
pub struct ClipJoint;

/// Weapon-clip players for the shown AC. Replaced when the design changes.
#[derive(Resource, Default)]
pub struct WeaponAnims {
    rigs: Vec<WeaponRig>,
    /// Arm `a00` clips, posed over locomotion while the weapon on that side is deployed.
    arms: Vec<WeaponRig>,
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

#[derive(Component, Clone, Copy, Debug)]
pub struct Projectile {
    velocity: Vec3,
    flight: Flight,
    decay: RangeDecay,
    damage_power: f32,
    damage_recoil: f32,
    distance: f32,
    stall_drop: f32,
    stall_frames: f32,
    life: f32,
    hit: i32,
}

impl Projectile {
    /// Creates a projectile fired from `gun` along `aim` (`0x828a3138` -> `0x829c0160`).
    pub fn from_gun(gun: &Gun, aim: Vec3) -> Self {
        Self {
            velocity: aim * gun.init_speed.max(1.0),
            flight: gun.flight,
            decay: gun.decay,
            damage_power: gun.damage_power,
            damage_recoil: gun.damage_recoil,
            distance: 0.0,
            stall_drop: 0.0,
            stall_frames: 0.0,
            life: MAX_LIFE,
            hit: gun.fx.hit,
        }
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

#[derive(Resource)]
pub(crate) struct Tracers {
    mesh: Handle<Mesh>,
    rigid: Handle<StandardMaterial>,
    energy: Handle<StandardMaterial>,
}

impl Armament {
    pub fn from_design(design: &AcAssemblyDesignSt) -> Self {
        Self {
            hands: [gun(design.armwep_r as i64), gun(design.armwep_l as i64)],
            bays: [gun(design.hanger_r as i64), gun(design.hanger_l as i64)],
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
    });
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
    collision: Res<Collision>,
    pads: Query<&Gamepad>,
    mut speaker: Speaker,
    mut commands: Commands,
    mut acs: Query<(&mut Pilot, &mut Armament, &Transform), Without<ClipJoint>>,
    rigging: Rigging,
    points: Query<(Entity, &EffectPoint)>,
    mut body: Body,
    mut posed: Query<(&mut Transform, Option<&Driven>), With<ClipJoint>>,
    mut shots: Query<
        (Entity, &mut Projectile, &mut Transform),
        (Without<Armament>, Without<ClipJoint>),
    >,
) {
    if !piloting.0 {
        return;
    }
    let dt = time.delta_secs();
    let Ok((mut pilot, mut arms, ac_tf)) = acs.single_mut() else {
        return;
    };
    let down = |k: KeyCode| keys.pressed(k) || held.0.contains(&k);
    let mut want = [
        down(KeyCode::KeyF) || mouse.pressed(MouseButton::Left),
        down(KeyCode::KeyC) || mouse.pressed(MouseButton::Right),
    ];
    // Manual (lang/en/text/menu/manual.fmg): R2 right arm weapon, L2 left.
    for pad in &pads {
        want[0] |= pad.pressed(GamepadButton::RightTrigger2);
        want[1] |= pad.pressed(GamepadButton::LeftTrigger2);
    }
    // Bay shift (manual 20403): while the modifier is held, the fire buttons exchange that
    // arm with its bay weapon instead of shooting. The clip keeps the block after release.
    let shifting = body.shift.is_some()
        || keys.pressed(KeyCode::KeyR)
        || held.0.contains(&KeyCode::KeyR)
        || pads.iter().any(|pad| pad.pressed(GamepadButton::North));
    if shifting {
        want = [false, false];
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
    for (i, gun) in arms.hands.iter_mut().enumerate() {
        gun.cooldown = (gun.cooldown - dt * TICK_RATE).max(0.0);
        let hand = if i == 0 { Hand::Right } else { Hand::Left };
        let can = gun.kind != Kind::Skip && gun.remaining > 0 && gun.cooldown <= 0.0;
        // Ready weapons wait out the body stance. A sniper rifle does not take one.
        let stance_ready = !gun.stance
            || body.motion.is_none()
            || body.anims.as_ref().is_none_or(|a| a.sniper.allows(hand));
        let fired = if let Some(anims) = body.anims.as_mut() {
            let (fired, deploy) = match anims
                .rigs
                .iter_mut()
                .find(|r| r.hand == hand && !r.bay && !r.retired)
            {
                Some(rig) => {
                    let fired = rig.playback.tick(want[i], can, frames);
                    let deploy = rig.playback.deploys();
                    rig.apply(&mut posed);
                    (fired, deploy)
                }
                None => (want[i] && can, false),
            };
            let fired = fired && stance_ready;
            if deploy {
                if let Some(arm) = anims.arms.iter_mut().find(|r| r.hand == hand) {
                    arm.playback.tick(want[i], false, frames);
                    arm.apply(&mut posed);
                }
            }
            fired
        } else {
            want[i] && can && stance_ready
        };
        if fired {
            if let Some(anims) = body.anims.as_mut() {
                anims.kick(hand, gun.sniper);
            }
        }
        if !fired {
            continue;
        }
        let origin = rigging
            .hardpoints
            .iter()
            .find(|(h, _)| h.hand == hand)
            .map_or(ac_tf.translation, |(_, t)| t.translation());
        gun.remaining = gun.remaining.saturating_sub(1);
        gun.cooldown = gun.reload_time.max(1.0);
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
                .find(|(_, p)| p.column == column && p.id == MUZZLE_POINT)
            {
                Some((point, _)) => commands.spawn((
                    Sfx::new(gun.fx.muzzle),
                    Transform::from_scale(Vec3::splat(MUZZLE_SCALE)),
                    Visibility::default(),
                    ChildOf(point),
                )),
                None => commands.spawn((
                    Sfx::new(gun.fx.muzzle),
                    Transform::from_translation(origin)
                        .looking_to(-aim, Vec3::Y)
                        .with_scale(Vec3::splat(MUZZLE_SCALE)),
                )),
            };
        }
        let shot = (
            Transform::from_translation(origin).looking_to(aim, Vec3::Y),
            Projectile::from_gun(gun, aim),
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
                    .find(|(_, p)| p.column == column && p.id == MUZZLE_POINT)
                    .map(|(e, _)| e);
                let arm = &mut anims.aim[i];
                arm.sniper = gun.sniper;
                arm.step(want[i] && gun.kind != Kind::Skip && !gun.stance, frames);
                arm.frame = arm
                    .shoulder
                    .and_then(|s| rigging.aim_frame(s, hand, muzzle, aim));
            }
            anims.apply_recoil(frames, &mut posed);
        }
    }
    for (e, mut shot, mut transform) in &mut shots {
        let Some(delta) = shot.step(dt) else {
            commands.entity(e).despawn();
            continue;
        };
        let next = transform.translation + delta;
        let ground = collision
            .ground_below(transform.translation)
            .filter(|&g| delta.y < 0.0 && next.y <= g);
        if let Some(y) = ground {
            if shot.hit > 0 {
                let at = Vec3::new(next.x, y, next.z);
                commands.spawn((
                    Sfx::new(shot.hit),
                    Transform::from_translation(at)
                        .with_rotation(Quat::from_rotation_arc(Vec3::Z, Vec3::Y)),
                ));
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

pub(crate) fn silent() -> Gun {
    Gun {
        remaining: 0,
        magazine: 0,
        reload_time: 0.0,
        cooldown: 0.0,
        init_speed: 0.0,
        kind: Kind::Skip,
        flight: Flight::default(),
        damage_power: 0.0,
        damage_recoil: 0.0,
        decay: RangeDecay::default(),
        fx: Fx::default(),
        shoot: 0,
        stance: false,
        sniper: false,
    }
}

fn gun(id: i64) -> Gun {
    if id <= 0 {
        return silent();
    }
    let magazine = part_field(id, 10, "magazine").max(0.0) as u16;
    let init_speed = part_field(id, 10, "init_speed");
    let (kind, flight, speed) = flight(part_field(id, 10, "bullet_id") as u32, init_speed);
    Gun {
        remaining: magazine,
        magazine,
        reload_time: part_field(id, 10, "reload_time"),
        cooldown: 0.0,
        init_speed: speed,
        kind,
        flight,
        damage_power: part_field(id, 10, "damage_power"),
        damage_recoil: part_field(id, 10, "damage_recoil"),
        decay: RangeDecay {
            no_decay_range: part_field(id, 10, "no_decay_range"),
            damp_range: part_field(id, 10, "damp_range"),
            guarantee_rate: part_field(id, 10, "guarantee_rate"),
            min_power_rate: part_field(id, 10, "min_power_rate"),
        },
        fx: fx(id),
        shoot: shoot_of(id),
        stance: part_field(id, 10, "ready_position") != 0.0,
        sniper: part_field(id, 10, "weapon_kind") == 5.0,
    }
}

fn shoot_of(id: i64) -> i16 {
    let hit_id = part_field(id, 10, "hit_id") as u32;
    find(PARAM_ACWEAPONSOUNDPARAM_BIN, hit_id)
        .map(|r| r.data.shoot)
        .unwrap_or(DEFAULT_SHOOT)
}

fn fx(id: i64) -> Fx {
    let hit_id = part_field(id, 10, "hit_id") as u32;
    let Some(row) = find(PARAM_ACWEAPONSFXPARAM_BIN, hit_id)
        .or_else(|| find(PARAM_ACWEAPONSFXPARAM_BIN, 1))
        .map(|r| &r.data)
    else {
        return Fx::default();
    };
    let hit = u32::try_from(row.hit_sfx_type)
        .ok()
        .and_then(|t| find(PARAM_BULLETHITSFXPARAM_BIN, t))
        .map_or(0, |r| r.data.default2);
    Fx {
        bullet: row.bullet_sfx_id.into(),
        muzzle: row.muzzle_sfx_id.into(),
        hit: hit.into(),
    }
}

fn flight(id: u32, init_speed: f32) -> (Kind, Flight, f32) {
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
            speed(init_speed, d.max_speed_km_h),
        );
    }
    (Kind::Skip, Flight::default(), init_speed)
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

impl SniperStance {
    fn update(&mut self, want: Option<Hand>, motion: &mut Motion, disc: &Disc, pilot: &mut Pilot) {
        match want {
            Some(hand) => {
                let restart = self.phase == SniperPhase::Off
                    || self.phase == SniperPhase::Release
                    || self.hand != hand;
                if restart {
                    self.hand = hand;
                    self.phase = if play_stance(hand, false, motion, disc, pilot) {
                        SniperPhase::Engage
                    } else {
                        SniperPhase::Hold
                    };
                    pilot.body_held(true);
                } else if self.phase == SniperPhase::Engage && motion.finished() {
                    self.phase = SniperPhase::Hold;
                }
            }
            None => match self.phase {
                SniperPhase::Off => {}
                SniperPhase::Release if motion.finished() => {
                    self.phase = SniperPhase::Off;
                    pilot.body_held(false);
                }
                SniperPhase::Release => {}
                SniperPhase::Engage | SniperPhase::Hold => {
                    self.phase = if play_stance(self.hand, true, motion, disc, pilot) {
                        SniperPhase::Release
                    } else {
                        SniperPhase::Off
                    };
                    pilot.body_held(self.phase != SniperPhase::Off);
                }
            },
        }
    }

    /// The engage clip has reached its last frame, so a round may leave.
    fn allows(&self, hand: Hand) -> bool {
        self.phase == SniperPhase::Hold && self.hand == hand
    }
}

/// `sniper_ready_*` / `sniper_stow_*` in `sheets/ac_states.csv`.
fn play_stance(hand: Hand, release: bool, motion: &mut Motion, disc: &Disc, pilot: &Pilot) -> bool {
    let state = match (hand, release) {
        (Hand::Right, false) => "sniper_ready_r",
        (Hand::Right, true) => "sniper_stow_r",
        (Hand::Left, false) => "sniper_ready_l",
        (Hand::Left, true) => "sniper_stow_l",
    };
    let Some(row) = ac_state(state, None).and_then(|s| find(PARAM_ACMOTION_BIN, s.row)) else {
        return false;
    };
    motion
        .play(
            disc,
            row.data.anim_id,
            row.data.b_loop != 0,
            1.0,
            pilot.fade(row.data.interpolate_id),
        )
        .is_ok()
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
    pub fn load(&mut self, disc: &Disc, placement: &Placement, rig: &Rig, joints: &[Entity]) {
        let arm = placement.column == "arms";
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
            .chain(self.recoil_bones.iter().map(|b| b.entity))
    }

    /// Stowed pose, applied once at spawn so the first frame is not the bind pose.
    pub fn rest_pose(&self) -> Vec<(Entity, Transform)> {
        self.rigs.iter().flat_map(WeaponRig::sample).collect()
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
    }
}
