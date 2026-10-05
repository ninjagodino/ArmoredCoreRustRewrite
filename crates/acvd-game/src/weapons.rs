//! Hand-weapon fire: each arm slot reads its `acvparts.bin` firing fields
//! (`sheets/ac_part_fields.csv` category 10) and spawns a tracer that flies with the matching
//! `bullet/bulletrigid.bin` or `bullet/bulletenergy.bin` row. Blades and missiles are not fired
//! yet. Shots leave the weapon-root joint along `Pilot::aim_direction` (no upper-body aim).
//! Effects come from the `param/acweaponsfxparam.bin` row named by the part's `hit_id` (row 1
//! when it has none): the muzzle flash at the weapon's effect point 101, the bullet effect on
//! the shot, and the `param/bullethitsfxparam.bin` `default2` effect where it hits the ground
//! (collision triangles keep no material).

use acvd_data::generated::ac_unit::AcAssemblyDesignSt;
use acvd_data::generated::bullet::{BULLET_BULLETENERGY_BIN, BULLET_BULLETRIGID_BIN, PARAM_BULLETHITSFXPARAM_BIN};
use acvd_data::generated::sfx::PARAM_ACWEAPONSFXPARAM_BIN;
use acvd_data::{find, part_field};
use bevy::prelude::*;

use crate::assemble::Placement;
use crate::collision::Collision;
use crate::control::{Held, Pilot, Piloting};
use crate::sfx::{EffectPoint, Sfx};

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
        Self { hands: [gun(design.armwep_r as i64), gun(design.armwep_l as i64)] }
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

pub fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>) {
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

pub fn fire(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    held: Res<Held>,
    piloting: Res<Piloting>,
    assets: Res<Tracers>,
    collision: Res<Collision>,
    pads: Query<&Gamepad>,
    mut commands: Commands,
    mut acs: Query<(&Pilot, &mut Armament, &Transform)>,
    hardpoints: Query<(&Hardpoint, &GlobalTransform)>,
    points: Query<(Entity, &EffectPoint)>,
    mut shots: Query<(Entity, &mut Projectile, &mut Transform), Without<Armament>>,
) {
    if !piloting.0 {
        return;
    }
    let dt = time.delta_secs();
    let Ok((pilot, mut arms, ac_tf)) = acs.single_mut() else {
        return;
    };
    let down = |k: KeyCode| keys.pressed(k) || held.0.contains(&k);
    let mut want = [down(KeyCode::KeyF) || mouse.pressed(MouseButton::Left), down(KeyCode::KeyC) || mouse.pressed(MouseButton::Right)];
    // Manual (lang/en/text/menu/manual.fmg): R2 right arm weapon, L2 left.
    for pad in &pads {
        want[0] |= pad.pressed(GamepadButton::RightTrigger2);
        want[1] |= pad.pressed(GamepadButton::LeftTrigger2);
    }
    let aim = pilot.aim_direction();
    for (i, gun) in arms.hands.iter_mut().enumerate() {
        gun.cooldown = (gun.cooldown - dt * TICK_RATE).max(0.0);
        if !want[i] || gun.kind == Kind::Skip || gun.remaining == 0 || gun.cooldown > 0.0 {
            continue;
        }
        let hand = if i == 0 { Hand::Right } else { Hand::Left };
        let origin = hardpoints.iter().find(|(h, _)| h.hand == hand).map_or(ac_tf.translation, |(_, t)| t.translation());
        let velocity = aim * gun.init_speed.max(1.0) / KMH_PER_MS;
        gun.remaining = gun.remaining.saturating_sub(1);
        gun.cooldown = gun.reload_time.max(1.0);
        let column = if hand == Hand::Right { "armwep_r" } else { "armwep_l" };
        if gun.fx.muzzle > 0 {
            match points.iter().find(|(_, p)| p.column == column && p.id == MUZZLE_POINT) {
                Some((point, _)) => commands.spawn((Sfx::new(gun.fx.muzzle), Transform::from_scale(Vec3::splat(MUZZLE_SCALE)), Visibility::default(), ChildOf(point))),
                None => commands.spawn((Sfx::new(gun.fx.muzzle), Transform::from_translation(origin).looking_to(-aim, Vec3::Y).with_scale(Vec3::splat(MUZZLE_SCALE)))),
            };
        }
        let shot = (Transform::from_translation(origin).looking_to(aim, Vec3::Y), Projectile { velocity, gravity: gun.gravity, life: MAX_LIFE, hit: gun.fx.hit });
        if gun.fx.bullet > 0 {
            commands.spawn((shot, Visibility::default())).with_child((Sfx::new(gun.fx.bullet), Transform::default()));
        } else {
            let material = match gun.kind {
                Kind::Energy => assets.energy.clone(),
                _ => assets.rigid.clone(),
            };
            commands.spawn((Mesh3d(assets.mesh.clone()), MeshMaterial3d(material), shot));
        }
    }
    for (e, mut shot, mut transform) in &mut shots {
        shot.life -= dt;
        let next = transform.translation + shot.velocity * dt;
        shot.velocity.y -= shot.gravity * TICK_RATE * dt;
        let ground = collision.ground_below(transform.translation).filter(|&g| shot.velocity.y < 0.0 && next.y <= g);
        if shot.life <= 0.0 || ground.is_some() {
            if let Some(y) = ground.filter(|_| shot.hit > 0) {
                let at = Vec3::new(next.x, y, next.z);
                commands.spawn((Sfx::new(shot.hit), Transform::from_translation(at).with_rotation(Quat::from_rotation_arc(Vec3::Z, Vec3::Y))));
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
    Gun { remaining: magazine, reload_time: part_field(id, 10, "reload_time"), cooldown: 0.0, init_speed: speed, kind, gravity, fx: fx(id) }
}

fn fx(id: i64) -> Fx {
    let hit_id = part_field(id, 10, "hit_id") as u32;
    let Some(row) = find(PARAM_ACWEAPONSFXPARAM_BIN, hit_id).or_else(|| find(PARAM_ACWEAPONSFXPARAM_BIN, 1)).map(|r| &r.data) else {
        return Fx::default();
    };
    let hit = u32::try_from(row.hit_sfx_type).ok().and_then(|t| find(PARAM_BULLETHITSFXPARAM_BIN, t)).map_or(0, |r| r.data.default2);
    Fx { bullet: row.bullet_sfx_id.into(), muzzle: row.muzzle_sfx_id.into(), hit: hit.into() }
}

fn flight(id: u32, init_speed: f32) -> (Kind, f32, f32) {
    if let Some(row) = find(BULLET_BULLETRIGID_BIN, id) {
        return (Kind::Rigid, row.data.gravity, speed(init_speed, row.data.max_speed_km_h));
    }
    if let Some(row) = find(BULLET_BULLETENERGY_BIN, id) {
        return (Kind::Energy, 0.0, speed(init_speed, row.data.max_speed_km_h));
    }
    (Kind::Skip, 0.0, init_speed)
}

fn speed(init: f32, max_km_h: u16) -> f32 {
    if init > 0.0 { init } else { max_km_h as f32 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_rifle_is_rigid() {
        let g = gun(1720);
        assert_eq!(g.kind, Kind::Rigid, "part 1720 bullet_id should be rigid 10002");
        assert!(g.remaining > 0 && g.init_speed > 0.0 && g.reload_time > 0.0, "{g:?}");
    }
}
