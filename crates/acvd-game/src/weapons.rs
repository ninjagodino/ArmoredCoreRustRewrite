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

/// Game ticks per second. `reload_time` is read as ticks and `BulletRigidSt.gravity` as metres
/// per second added each tick; both units are unconfirmed (see `docs/status.md`, Weapons).
const TICK_RATE: f32 = 60.0;
/// `init_speed` is read as km/h, the unit of the bullet rows' max/min speeds; unconfirmed.
const KMH_PER_MS: f32 = 3.6;
/// Tracer lifetime in seconds; not game data.
const MAX_LIFE: f32 = 5.0;
/// Shoot id `FUN_82891e18` writes when the weapon has no `acweaponsoundparam` row.
const DEFAULT_SHOOT: i16 = 0x12B;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Hand {
    Right,
    Left,
}

/// The two hand weapons of the shown design, with magazine and fire cooldown.
#[derive(Component)]
pub struct Armament {
    pub hands: [Gun; 2],
}

#[derive(Clone, Copy, Debug)]
pub struct Gun {
    pub remaining: u16,
    pub reload_time: f32,
    pub cooldown: f32,
    pub init_speed: f32,
    pub kind: Kind,
    pub gravity: f32,
    pub fx: Fx,
    /// `acweaponsoundparam.shoot`. `≤ 0` plays nothing.
    pub shoot: i16,
    /// Ready weapon (`ready_position`). Holding fire plays the body stance before the shot.
    /// A sniper rifle (`weapon_kind` 5) is not one of these.
    pub stance: bool,
    /// Sniper rifle (`weapon_kind` 5): shots kick with `sniper_$(LR)` instead of `gun_$(LR)`.
    pub sniper: bool,
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
    sniper: SniperStance,
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

#[derive(Component)]
pub struct Projectile {
    velocity: Vec3,
    gravity: f32,
    life: f32,
    hit: i32,
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
    hardpoints: Query<(&Hardpoint, &GlobalTransform)>,
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
    if let Some(motion) = body.motion.as_mut() {
        if let Some(anims) = body.anims.as_mut() {
            anims
                .sniper
                .update(sniper_hand, motion, &disc, &mut *pilot);
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
            let (fired, deploy) = match anims.rigs.iter_mut().find(|r| r.hand == hand) {
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
        let origin = hardpoints
            .iter()
            .find(|(h, _)| h.hand == hand)
            .map_or(ac_tf.translation, |(_, t)| t.translation());
        let velocity = aim * gun.init_speed.max(1.0) / KMH_PER_MS;
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
            Projectile {
                velocity,
                gravity: gun.gravity,
                life: MAX_LIFE,
                hit: gun.fx.hit,
            },
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
    if let Some(anims) = body.anims.as_mut() {
        anims.apply_recoil(frames, &mut posed);
    }
    for (e, mut shot, mut transform) in &mut shots {
        shot.life -= dt;
        let next = transform.translation + shot.velocity * dt;
        shot.velocity.y -= shot.gravity * TICK_RATE * dt;
        let ground = collision
            .ground_below(transform.translation)
            .filter(|&g| shot.velocity.y < 0.0 && next.y <= g);
        if shot.life <= 0.0 || ground.is_some() {
            if let Some(y) = ground.filter(|_| shot.hit > 0) {
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
        if shot.velocity.length_squared() > 0.0 {
            transform.look_to(shot.velocity.normalize(), Vec3::Y);
        }
    }
}

fn gun(id: i64) -> Gun {
    let magazine = part_field(id, 10, "magazine").max(0.0) as u16;
    let init_speed = part_field(id, 10, "init_speed");
    let (kind, gravity, speed) = flight(part_field(id, 10, "bullet_id") as u32, init_speed);
    Gun {
        remaining: magazine,
        reload_time: part_field(id, 10, "reload_time"),
        cooldown: 0.0,
        init_speed: speed,
        kind,
        gravity,
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

fn flight(id: u32, init_speed: f32) -> (Kind, f32, f32) {
    if let Some(row) = find(BULLET_BULLETRIGID_BIN, id) {
        return (
            Kind::Rigid,
            row.data.gravity,
            speed(init_speed, row.data.max_speed_km_h),
        );
    }
    if let Some(row) = find(BULLET_BULLETENERGY_BIN, id) {
        return (
            Kind::Energy,
            0.0,
            speed(init_speed, row.data.max_speed_km_h),
        );
    }
    (Kind::Skip, 0.0, init_speed)
}

fn speed(init: f32, max_km_h: u16) -> f32 {
    if init > 0.0 {
        init
    } else {
        max_km_h as f32
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
    playback: Playback,
    /// Pose over the locomotion clip. Weapon meshes leave body-driven joints alone.
    body: bool,
    /// `(joint, bone index in the weapon skeleton)`.
    joints: Vec<(Entity, usize)>,
    skeleton: ani::Anim,
    clips: [Option<ani::Anim>; 4],
}

impl SniperStance {
    fn update(
        &mut self,
        want: Option<Hand>,
        motion: &mut Motion,
        disc: &Disc,
        pilot: &mut Pilot,
    ) {
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
fn play_stance(
    hand: Hand,
    release: bool,
    motion: &mut Motion,
    disc: &Disc,
    pilot: &Pilot,
) -> bool {
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
    /// Loads hand `a00` clips, the arm deploy clips on that side, and the rifle kick bones.
    pub fn load(&mut self, disc: &Disc, placement: &Placement, rig: &Rig, joints: &[Entity]) {
        let hands = placement_hands(placement.column);
        if hands.is_empty() {
            return;
        }
        // `arm_l` and `arm_r` share one placement (assembly_slots.csv column `arms`).
        let arm = placement.column == "arms";
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
                    if let Some(&entity) = joints.get(b) {
                        pairs.push((entity, i));
                    }
                }
            }
            if pairs.is_empty() && !arm && skeleton.bones.len() == rig.bones.len() {
                pairs = (0..rig.bones.len())
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
    fn note_recoil(&mut self, hand: Hand, controls: &[jcon::Control], rig: &Rig, joints: &[Entity]) {
        let side = match hand {
            Hand::Right => "R",
            Hand::Left => "L",
        };
        for (sniper, prefix) in [(false, "gun_"), (true, "sniper_")] {
            let name = format!("{prefix}{side}");
            let Some(control) = controls.iter().find(|c| c.name == name) else {
                continue;
            };
            for object in control.objects.iter().filter(|o| o.react_ang != 0.0 && o.react_time > 0.0) {
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
            if angle == 0.0 {
                continue;
            }
            if let Ok((mut transform, _)) = posed.get_mut(bone.entity) {
                transform.rotation *= Quat::from_rotation_x(angle.to_radians());
            }
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
fn placement_hands(column: &str) -> &'static [Hand] {
    match column {
        "armwep_r" => &[Hand::Right],
        "armwep_l" => &[Hand::Left],
        "arms" => &[Hand::Right, Hand::Left],
        _ => &[],
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
        assert!(
            g.remaining > 0 && g.init_speed > 0.0 && g.reload_time > 0.0,
            "{g:?}"
        );
        assert!(
            !g.sniper && !g.stance,
            "the starter rifle uses the gun kick, not the ready stance"
        );
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
        assert!(frames[..5].iter().all(|&a| a == 0.0), "nothing before ReactDelay");
        let (peak_at, peak) = frames
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.total_cmp(b.1))
            .unwrap();
        assert!((peak + 10.0).abs() < 1e-4 && (10..=12).contains(&peak_at), "{frames:?}");
        assert!(frames[peak_at + 15] > -6.0 && frames[peak_at + 15] < -4.0, "halfway back");
        assert_eq!(frames[59], 0.0, "settled {KICK_RETURN} frames after the kick");
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
        assert!(placement_hands("arms") == [Hand::Right, Hand::Left]);
        assert!(placement_hands("armwep_r") == [Hand::Right]);
        assert!(placement_hands("core").is_empty());
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
