//! Ambient (non-combat) air traffic over a sortie map.
//!
//! Model/rig sources on the 360 disc:
//! - `model/ene/e2030/e2030_m.bnd.dcx|e2030.flv` + `e2030_a.bnd.dcx` (CH armed heli,
//!   `EnemyCommonSt` behavior `e2030`; the same model the AC TEST layer spawns as a target)
//! - `model/ene/e2040/e2040_m.bnd.dcx|e2040.flv` + `e2040_a.bnd.dcx` (transport heli)
//! - `model/ene/e6010/e6010_m.bnd.dcx|e6010.flv` + `e6010_a.bnd.dcx` (the online flying
//!   bomber; `EnemyCommonSt` 6010, 531 m span; e6014 is its outer wing section)
//!
//! The patrol routes are presentation choices for the rewrite, not 360 data: a 360 map's
//! ambient traffic is mission-scripted, and no ACVD map MSB places aircraft parts except the
//! e8060/e8061 dropships (m3104 `Versus` layer) and the elevated e9223 Chinooks in m3330/m3530.

use std::collections::HashMap;
use std::sync::Arc;

use acvd_formats::vfs::Disc;
use acvd_render::Packs;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::math::Affine3A;
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::prelude::*;

use crate::targets::{self, EnemyAnim, EnemyAnimBank};

/// Marker and route state for one non-combat aircraft.
#[derive(Component, Debug)]
pub struct AmbientAircraft {
    #[allow(dead_code)]
    pub model: &'static str,
    center: Vec3,
    radius: f32,
    altitude: f32,
    speed: f32,
    angle: f32,
    direction: f32,
    bob_amp: f32,
    bob_hz: f32,
    bank: f32,
}

/// One route per aircraft: `(model, radius, altitude, speed, start angle, direction)`.
const ROUTES: &[(&str, f32, f32, f32, f32, f32)] = &[
    ("e2030", 300.0, 95.0, 24.0, 0.0, 1.0),
    ("e2030", 430.0, 120.0, 20.0, std::f32::consts::PI, -1.0),
    ("e2040", 700.0, 185.0, 32.0, std::f32::consts::FRAC_PI_2, 1.0),
    ("e6010", 1600.0, 620.0, 85.0, 0.0, 1.0),
];

struct Loaded {
    meshes: Vec<(Handle<Mesh>, Handle<StandardMaterial>)>,
    inv_bindposes: Handle<SkinnedMeshInverseBindposes>,
    bank: Arc<EnemyAnimBank>,
}

/// Fills an empty sortie with the ambient patrols (the mode switch despawns them with the
/// rest of the world, and the next sortie spawns a fresh set).
#[allow(clippy::too_many_arguments)]
pub fn spawn_system(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
    garage: Res<crate::Garage>,
    scene: Res<crate::Scene>,
    start: Res<crate::map::Start>,
    existing: Query<(), With<AmbientAircraft>>,
) {
    if !scene.ambient || scene.map.is_none() || !existing.is_empty() {
        return;
    }
    spawn(
        &mut commands,
        &mut meshes,
        &mut materials,
        &mut images,
        &mut bindposes,
        &garage.disc,
        garage.flat,
        start.position,
    );
}

/// Spawns the ambient aircraft around the sortie start point. Returns the number spawned.
#[allow(clippy::too_many_arguments)]
fn spawn(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    bindposes: &mut Assets<SkinnedMeshInverseBindposes>,
    disc: &Disc,
    flat: bool,
    start: Vec3,
) -> usize {
    let mut packs = Packs::default();
    let mut missing = 0usize;
    let mut cache: HashMap<&'static str, Option<Loaded>> = HashMap::new();
    let mut spawned = 0usize;

    for &(model, radius, altitude, speed, angle, direction) in ROUTES {
        let loaded = cache.entry(model).or_insert_with(|| {
            let asset = targets::enemy_asset(model);
            match acvd_render::rigged_model(disc, &asset, &|_| acvd_render::RootPlace::IDENTITY) {
                Ok((list, rig)) if !list.is_empty() => {
                    let bank = EnemyAnimBank::load(disc, model, &rig);
                    let unboned = Affine3A::from_translation(Vec3::from(rig.unboned));
                    let binds: Vec<Mat4> = rig
                        .bones
                        .iter()
                        .map(|b| targets::xform_to_affine(&b.bind))
                        .chain([unboned])
                        .map(|b| Mat4::from(crate::pose::mirror(b).inverse()))
                        .collect();
                    let inv_bindposes = bindposes.add(SkinnedMeshInverseBindposes::from(binds));
                    let meshes_mats = list
                        .into_iter()
                        .map(|m| {
                            let mat = acvd_render::material(
                                disc,
                                &m,
                                &mut packs,
                                images,
                                flat,
                                &mut missing,
                            );
                            (meshes.add(m.mesh), materials.add(mat))
                        })
                        .collect();
                    Some(Loaded {
                        meshes: meshes_mats,
                        inv_bindposes,
                        bank,
                    })
                }
                Ok(_) => None,
                Err(e) => {
                    warn!("ambient {model}: {e:#}");
                    None
                }
            }
        });
        let Some(loaded) = loaded.as_ref() else {
            continue;
        };

        let center = Vec3::new(start.x, 0.0, start.z);
        let pos = Vec3::new(
            center.x + angle.cos() * radius,
            altitude,
            center.z + angle.sin() * radius,
        );
        let tangent = Vec3::new(-angle.sin(), 0.0, angle.cos()) * direction;
        let yaw = crate::ai::yaw_from_dir(tangent).unwrap_or(0.0);

        let root = commands
            .spawn((
                AmbientAircraft {
                    model,
                    center,
                    radius,
                    altitude,
                    speed,
                    angle,
                    direction,
                    bob_amp: if model == "e6010" { 0.0 } else { 4.0 },
                    bob_hz: 0.12,
                    bank: 0.18 * direction,
                },
                Name::new(format!("ambient {model}")),
                Transform::from_translation(pos).with_rotation(Quat::from_rotation_y(yaw)),
                Visibility::default(),
            ))
            .id();

        let mut joint_entities: Vec<Entity> = Vec::with_capacity(loaded.bank.rest_locals.len());
        for _ in 0..loaded.bank.rest_locals.len() {
            joint_entities.push(
                commands
                    .spawn((Transform::default(), Visibility::default()))
                    .id(),
            );
        }
        for (i, &ent) in joint_entities.iter().enumerate() {
            let parent_ent = loaded.bank.parents[i].map_or(root, |p| joint_entities[p]);
            commands
                .entity(ent)
                .insert((loaded.bank.rest_locals[i], ChildOf(parent_ent)));
        }
        let extra_unboned = commands
            .spawn((Transform::default(), Visibility::default(), ChildOf(root)))
            .id();
        let mut skin_joints = joint_entities.clone();
        skin_joints.push(extra_unboned);

        for (mesh, mat) in &loaded.meshes {
            commands.spawn((
                Mesh3d(mesh.clone()),
                MeshMaterial3d(mat.clone()),
                SkinnedMesh {
                    inverse_bindposes: loaded.inv_bindposes.clone(),
                    joints: skin_joints.clone(),
                },
                NoFrustumCulling,
                Transform::default(),
                ChildOf(root),
            ));
        }

        commands
            .entity(root)
            .insert(EnemyAnim::new(loaded.bank.clone(), joint_entities, yaw));
        eprintln!(
            "AMBIENT {model} at ({:.0},{:.0},{:.0}) radius {radius:.0}",
            pos.x, pos.y, pos.z
        );
        spawned += 1;
    }

    if missing > 0 {
        warn!("ambient: {missing} textures missing");
    }
    eprintln!("AMBIENT {spawned} aircraft ({})", ROUTES.iter().map(|r| r.0).collect::<Vec<_>>().join(", "));
    info!("ambient: {spawned} aircraft spawned");
    spawned
}

/// Moves each aircraft along its circular route and banks it into the turn.
pub fn step(time: Res<Time>, mut aircraft: Query<(&mut AmbientAircraft, &mut Transform)>) {
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    for (mut air, mut xf) in &mut aircraft {
        air.angle += air.direction * air.speed / air.radius * dt;
        let bob = (air.angle * air.bob_hz * air.radius / air.speed).sin() * air.bob_amp;
        let pos = Vec3::new(
            air.center.x + air.angle.cos() * air.radius,
            air.altitude + bob,
            air.center.z + air.angle.sin() * air.radius,
        );
        let tangent =
            Vec3::new(-air.angle.sin(), 0.0, air.angle.cos()) * air.direction;
        let yaw = crate::ai::yaw_from_dir(tangent).unwrap_or(0.0);
        xf.translation = pos;
        xf.rotation = Quat::from_rotation_y(yaw) * Quat::from_rotation_z(air.bank);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acvd_formats::vfs::Disc;

    #[test]
    fn ambient_models_resolve_on_disc() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let Ok(disc) = Disc::open(&acvd_formats::vfs::default_disc(&root)) else {
            return;
        };
        for &(model, ..) in ROUTES {
            let asset = targets::enemy_asset(model);
            assert!(
                disc.exists(&asset),
                "ambient model {model} missing at {asset}"
            );
            assert!(
                disc.exists(&targets::enemy_anim_binder(model)),
                "ambient animation {model} missing"
            );
        }
    }
}
