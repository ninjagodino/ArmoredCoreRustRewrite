//! Shattered enemy corpse & destructible map prop gib physics (`_b.flv`).
//!
//! Both destroyed enemies (`model/ene/{m}/{m}_m.bnd.dcx|{m}_b.flv`) and destructible map objects
//! (`model/obj/{o}/{o}_m.bnd.dcx|{o}_b.flv`) ship skinned FLVERs whose vertices are rigidly bound
//! to per-chunk bones (`break_00..`, `lpbreak_00..`, `bk01`, or detached limb/hull bones). Loading
//! `_b.flv` as an unskinned static mesh freezes every chunk in its intact bind pose; this module
//! loads `_b.flv` as a [`SkinnedMesh`] with every joint detached directly under a world-space
//! [`DebrisRoot`] and simulates each gib piece using the 360 `ACV_DESTROY_PARAM_ST` /
//! `DESTROY_FALL_PARAM_ST` (`param/destroyparam.bin`, `param/destroyfall.param`) spread, spin,
//! gravity, ground bounce, and post-settle cleanup parameters.

use acvd_data::find;
use acvd_data::generated::enemy::{PARAM_DESTROYFALL_PARAM, PARAM_DESTROYPARAM_BIN};
use acvd_formats::flver;
use acvd_formats::vfs::Disc;
use acvd_render::Packs;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::math::Affine3A;
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::mesh::{Mesh, VertexAttributeValues};
use bevy::prelude::*;

use crate::collision::Collision;
use crate::pose;

/// Marker on every spawned debris root so sortie exit / reload despawns all active gibs cleanly.
#[derive(Component)]
pub struct DebrisRoot;

/// Whether a [`DebrisRig`] came from a destroyed enemy or a broken map object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DebrisKind {
    /// Destroyed enemy (`e*_b.flv`): every bone with geometry breaks apart and falls/tumbles.
    Enemy,
    /// Destructible map object (`o*_b.flv`): `break*` / `bk*` bones fly apart while any non-break
    /// base bone (e.g. `o2108`, `o4235` `body`, `o2135`) remains as static rubble.
    Prop,
}

/// Physics state of one bone chunk inside a [`DebrisRig`].
#[derive(Clone, Debug)]
pub struct GibPiece {
    pub joint: Entity,
    pub pos: Vec3,
    pub rot: Quat,
    pub base_scale: Vec3,
    /// Geometric vertex centroid in bind-bone local space so chunks spin and land around their
    /// true center even when the FLVER bone pivot sits at `[0, 0, 0]`.
    pub local_center: Vec3,
    /// Bounding radius of the chunk in world space (used for ground clearance).
    pub radius: f32,
    pub vel: Vec3,
    pub ang_vel: Vec3,
    pub gravity: f32,
    pub restitution: f32,
    pub ground_time: f32,
    pub rest_secs: f32,
    pub age: f32,
    pub start_y: f32,
    pub dynamic: bool,
    pub done: bool,
}

impl GibPiece {
    /// Current world-space geometric center of this chunk.
    pub fn world_center(&self) -> Vec3 {
        self.pos + self.rot * (self.base_scale * self.local_center)
    }

    /// Steps this gib piece by `dt` seconds against `collision` and returns its updated transform.
    /// Returns `None` if the piece is static or already finished.
    pub fn step(&mut self, dt: f32, collision: &Collision) -> Option<Transform> {
        if !self.dynamic || self.done {
            return None;
        }
        self.age += dt;
        self.vel.y -= self.gravity * dt;
        self.pos += self.vel * dt;

        // Rotate around the chunk's true geometric center (`world_center`) rather than an
        // arbitrary `[0, 0, 0]` FLVER bone pivot.
        let ang_speed_sq = self.ang_vel.length_squared();
        if ang_speed_sq > 1e-6 {
            let dq = Quat::from_scaled_axis(self.ang_vel * dt);
            let prev_off = self.rot * (self.base_scale * self.local_center);
            self.rot = (dq * self.rot).normalize();
            let new_off = self.rot * (self.base_scale * self.local_center);
            self.pos += prev_off - new_off;
        }

        let center = self.world_center();
        let probe_up = self.radius.max(2.0);
        if let Some(ground_y) =
            collision.ground_below(Vec3::new(center.x, center.y + probe_up, center.z))
        {
            let clearance = (self.radius * 0.35).clamp(0.15, 1.25);
            let floor_y = ground_y + clearance;
            if center.y <= floor_y {
                self.pos.y += floor_y - center.y;
                if self.vel.y < -1.5 {
                    // Bounce with restitution, horizontal friction, and spin damping.
                    self.vel.y = -self.vel.y * self.restitution;
                    self.vel.x *= 0.62;
                    self.vel.z *= 0.62;
                    self.ang_vel *= 0.65;
                } else {
                    // Settle on ground and damp residual sliding / rolling.
                    self.vel.y = 0.0;
                    let damp = (1.0 - 6.0 * dt).max(0.0);
                    self.vel.x *= damp;
                    self.vel.z *= damp;
                    self.ang_vel *= damp;
                    self.ground_time += dt;
                }
            }
        }

        // Void safety or max lifetime expiration.
        let expired = center.y < self.start_y - 200.0 || self.age >= 12.0;
        let shrink_start = self.rest_secs;
        let shrink_dur = 0.6;
        let scale = if expired || self.ground_time >= shrink_start + shrink_dur {
            self.done = true;
            Vec3::ZERO
        } else if self.ground_time > shrink_start {
            let t = ((self.ground_time - shrink_start) / shrink_dur).clamp(0.0, 1.0);
            self.base_scale * (1.0 - t)
        } else {
            self.base_scale
        };

        Some(Transform {
            translation: self.pos,
            rotation: self.rot,
            scale,
        })
    }
}

/// Active shattered-model physics rig driving a `_b.flv` [`SkinnedMesh`].
#[derive(Component)]
pub struct DebrisRig {
    pub label: String,
    pub kind: DebrisKind,
    pub pieces: Vec<GibPiece>,
    pub has_static_base: bool,
    pub settled: bool,
    pub elapsed: f32,
}

/// Returns `true` when a FLVER bone name represents a detachable destruction gib piece
/// (`break_00`, `break2_01`, `lpbreak_00`, `bbreak_00`, `bk01`, ...).
pub fn is_gib_bone_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("break") || lower.starts_with("bk")
}

fn xform_to_affine(xf: &flver::Xform) -> Affine3A {
    Affine3A::from_cols(
        Vec3::new(xf.m[0][0], xf.m[1][0], xf.m[2][0]).into(),
        Vec3::new(xf.m[0][1], xf.m[1][1], xf.m[2][1]).into(),
        Vec3::new(xf.m[0][2], xf.m[1][2], xf.m[2][2]).into(),
        Vec3::from(xf.t).into(),
    )
}

/// Deterministic splitmix-style hash in `[0.0, 1.0)` for per-gib variation.
fn hash01(seed: u64) -> f32 {
    let mut z = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    ((z >> 40) as f32) / ((1u32 << 24) as f32)
}

/// Resolved 360 destruction fall parameters (`param/destroyparam.bin` / `param/destroyfall.param`).
#[derive(Clone, Copy, Debug)]
struct DestroyPhysicsParams {
    rotate_deg: f32,
    rotate_rand_deg: f32,
    gravity: f32,
    gravity_rand: f32,
    spread_speed: f32,
    spread_rand: f32,
    restitution: f32,
    rest_secs: f32,
}

fn physics_params(kind: DebrisKind) -> DestroyPhysicsParams {
    match kind {
        DebrisKind::Enemy => {
            // `param/destroyparam.bin` row 1010 (`敵4メートル以下破壊挙動`), fallback row 0.
            let p = find(PARAM_DESTROYPARAM_BIN, 1010)
                .or_else(|| find(PARAM_DESTROYPARAM_BIN, 0))
                .map(|r| r.data);
            match p {
                Some(d) => DestroyPhysicsParams {
                    rotate_deg: d.rotate_revolution,
                    rotate_rand_deg: d.rotate_revolution_random_range,
                    gravity: d.fall_acceleration.max(15.0),
                    gravity_rand: d.fall_acceleration_random_range.min(25.0),
                    spread_speed: d.spread_speed.max(8.0),
                    spread_rand: d.spread_speed_random_range.min(24.0),
                    restitution: (f32::from(d.recoil_rate) / 250.0).clamp(0.20, 0.42),
                    rest_secs: if d.disp_time_on_ground > 0 {
                        (f32::from(d.disp_time_on_ground) * 0.1).clamp(2.0, 5.0)
                    } else {
                        2.5
                    },
                },
                None => DestroyPhysicsParams {
                    rotate_deg: 120.0,
                    rotate_rand_deg: 200.0,
                    gravity: 30.0,
                    gravity_rand: 20.0,
                    spread_speed: 18.0,
                    spread_rand: 20.0,
                    restitution: 0.32,
                    rest_secs: 2.5,
                },
            }
        }
        DebrisKind::Prop => {
            // `param/destroyparam.bin` row 911 (`汎用_破片中`) / `param/destroyfall.param` row 301.
            let p = find(PARAM_DESTROYPARAM_BIN, 911).map(|r| r.data);
            let f = find(PARAM_DESTROYFALL_PARAM, 301).map(|r| r.data);
            match (p, f) {
                (Some(d), _) => DestroyPhysicsParams {
                    rotate_deg: d.rotate_revolution,
                    rotate_rand_deg: d.rotate_revolution_random_range,
                    gravity: d.fall_acceleration.clamp(25.0, 55.0),
                    gravity_rand: d.fall_acceleration_random_range.min(25.0),
                    spread_speed: (d.spread_speed * 0.75).clamp(10.0, 24.0),
                    spread_rand: d.spread_speed_random_range.clamp(8.0, 18.0),
                    restitution: (f32::from(d.recoil_rate) / 220.0).clamp(0.22, 0.42),
                    rest_secs: 3.0,
                },
                (None, Some(d)) => DestroyPhysicsParams {
                    rotate_deg: d.rotate_revolution,
                    rotate_rand_deg: d.rotate_revolution_random_range,
                    gravity: d.fall_acceleration.max(25.0),
                    gravity_rand: d.fall_acceleration_random_range.min(25.0),
                    spread_speed: d.spread_speed.max(10.0),
                    spread_rand: d.spread_speed_random_range.max(8.0),
                    restitution: 0.30,
                    rest_secs: 3.0,
                },
                (None, None) => DestroyPhysicsParams {
                    rotate_deg: 60.0,
                    rotate_rand_deg: 120.0,
                    gravity: 38.0,
                    gravity_rand: 20.0,
                    spread_speed: 16.0,
                    spread_rand: 14.0,
                    restitution: 0.30,
                    rest_secs: 3.0,
                },
            }
        }
    }
}

/// `model/obj/{o}/{o}.tpf.dcx` (or `model/ene/{e}/{e}.tpf.dcx`) holding the textures of the
/// binder behind `asset` (`..._{o}_m.bnd.dcx|{o}_b.flv`), when that pack exists on the disc.
/// Gib texture names (`net`, `o2036`, `e0010_01`, ...) collide with other models, so they are
/// resolved inside the model's own pack instead of a global name-only lookup.
fn texture_pack(disc: &Disc, asset: &str) -> Option<String> {
    let binder = asset.split('|').next().unwrap_or(asset);
    let stem = binder.strip_suffix("_m.bnd.dcx")?;
    let pack = format!("{stem}.tpf.dcx");
    disc.exists(&pack).then_some(pack)
}

/// Spawns a skinned, physics-driven [`DebrisRig`] from a broken `_b.flv` asset at `root_xf`.
///
/// Returns `None` if the asset fails to load or contains no dynamic gib bones (for instance,
/// single-bone static rubble replacements like `o2106_b.flv` / `o2107_b.flv`), so callers can
/// fall back to static rendering when appropriate.
#[allow(clippy::too_many_arguments)]
pub fn spawn(

    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    bindposes: &mut Assets<SkinnedMeshInverseBindposes>,
    disc: &Disc,
    packs: &mut Packs,
    flat: bool,
    asset: &str,
    label: &str,
    kind: DebrisKind,
    root_xf: Transform,
    initial_vel: Vec3,
    impact_dir: Vec3,
) -> Option<Entity> {
    let Ok((list, rig)) =
        acvd_render::rigged_model(disc, asset, &|_| acvd_render::RootPlace::IDENTITY)
    else {
        return None;
    };
    if list.is_empty() {
        return None;
    }

    let bone_count = rig.bones.len();
    let total_joints = bone_count + 1; // +1 for unboned vertices
    let unboned_aff = Affine3A::from_translation(Vec3::from(rig.unboned));
    let bind_worlds: Vec<Mat4> = rig
        .bones
        .iter()
        .map(|b| Mat4::from(pose::mirror(xform_to_affine(&b.bind))))
        .chain([Mat4::from(pose::mirror(unboned_aff))])
        .collect();
    let inv_binds: Vec<Mat4> = bind_worlds.iter().map(|m| m.inverse()).collect();

    // Compute per-bone vertex bounds in bind-bone local space to find each chunk's true center
    // and radius, and to detect which bones actually own geometry.
    let mut min_v = vec![Vec3::splat(f32::INFINITY); total_joints];
    let mut max_v = vec![Vec3::splat(f32::NEG_INFINITY); total_joints];
    let mut vert_counts = vec![0u32; total_joints];
    for m in &list {
        let Some(VertexAttributeValues::Float32x3(positions)) =
            m.mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            continue;
        };
        let Some(VertexAttributeValues::Uint16x4(joints)) =
            m.mesh.attribute(Mesh::ATTRIBUTE_JOINT_INDEX)
        else {
            continue;
        };
        for (pos, ji) in positions.iter().zip(joints.iter()) {
            let idx = (ji[0] as usize).min(total_joints - 1);
            let local = inv_binds[idx].transform_point3(Vec3::from(*pos));
            min_v[idx] = min_v[idx].min(local);
            max_v[idx] = max_v[idx].max(local);
            vert_counts[idx] += 1;
        }
    }

    let has_explicit_break_bones = rig
        .bones
        .iter()
        .enumerate()
        .any(|(i, b)| vert_counts[i] > 0 && is_gib_bone_name(&b.name));

    // Classify dynamic gib bones vs static base bones.
    let mut is_dynamic = vec![false; total_joints];
    let mut dynamic_count = 0usize;
    let mut has_static_base = false;
    for i in 0..total_joints {
        if vert_counts[i] == 0 {
            continue;
        }
        let bone_name = rig.bones.get(i).map_or("", |b| b.name.as_str());
        let dyn_piece = match kind {
            DebrisKind::Enemy => true,
            DebrisKind::Prop => i < bone_count && is_gib_bone_name(bone_name),
        };
        is_dynamic[i] = dyn_piece;
        if dyn_piece {
            dynamic_count += 1;
        } else {
            has_static_base = true;
        }
    }

    if dynamic_count == 0 {
        return None;
    }

    // Compute world-space explosion center as the average of all dynamic chunk centers.
    let root_mat = root_xf.to_matrix();
    let mut init_tfs = Vec::with_capacity(total_joints);
    let mut local_centers = Vec::with_capacity(total_joints);
    let mut radii = Vec::with_capacity(total_joints);
    let mut center_sum = Vec3::ZERO;
    for i in 0..total_joints {
        let tf = Transform::from_matrix(root_mat * bind_worlds[i]);
        let (lc, rad) = if vert_counts[i] > 0 {
            let c = 0.5 * (min_v[i] + max_v[i]);
            let half = 0.5 * (max_v[i] - min_v[i]);
            let scale_max = tf.scale.abs().max_element().max(0.01);
            (c, (half.length() * scale_max).clamp(0.25, 6.0))
        } else {
            (Vec3::ZERO, 0.25)
        };
        if is_dynamic[i] {
            center_sum += tf.translation + tf.rotation * (tf.scale * lc);
        }
        init_tfs.push(tf);
        local_centers.push(lc);
        radii.push(rad);
    }
    let explosion_origin = center_sum / (dynamic_count as f32);

    let params = physics_params(kind);
    let inv_bind_handle = bindposes.add(SkinnedMeshInverseBindposes::from(inv_binds));

    let root = commands
        .spawn((
            DebrisRoot,
            crate::SortieDecor,
            Name::new(format!("debris_{label}")),
            Transform::IDENTITY,
            Visibility::default(),
        ))
        .id();

    let base_seed = (root_xf.translation.x.to_bits() as u64)
        ^ ((root_xf.translation.y.to_bits() as u64).rotate_left(21))
        ^ ((root_xf.translation.z.to_bits() as u64).rotate_left(42));

    let mut joint_entities = Vec::with_capacity(total_joints);
    let mut pieces = Vec::with_capacity(total_joints);

    for i in 0..total_joints {
        let tf = init_tfs[i];
        let joint = commands.spawn((tf, ChildOf(root))).id();
        joint_entities.push(joint);

        if vert_counts[i] == 0 {
            continue;
        }

        let lc = local_centers[i];
        let world_center = tf.translation + tf.rotation * (tf.scale * lc);
        let bone_name = rig.bones.get(i).map_or("", |b| b.name.as_str());
        let s = base_seed.wrapping_add((i as u64).wrapping_mul(0x9e37_79b9));

        let (vel, ang_vel, gravity) = if is_dynamic[i] {
            // Radial outward direction from explosion center + deterministic jitter.
            let mut dir = world_center - explosion_origin;
            if dir.length_squared() < 1e-4 {
                dir = Vec3::new(
                    hash01(s + 1) * 2.0 - 1.0,
                    0.5,
                    hash01(s + 2) * 2.0 - 1.0,
                );
            }
            dir.x += (hash01(s + 3) - 0.5) * 0.45;
            dir.z += (hash01(s + 4) - 0.5) * 0.45;
            dir.y = dir.y.max(0.0) + 0.35 + hash01(s + 5) * 0.65;
            let dir = dir.normalize_or_zero();

            // Heavier hull/body pieces of an enemy get gentler spread & slower tumble than
            // small `break*` shrapnel so the wreckage reads as a heavy machine coming apart.
            let heavy = kind == DebrisKind::Enemy
                && ((has_explicit_break_bones && !is_gib_bone_name(bone_name))
                    || radii[i] > 2.2);
            let speed_scale = if heavy { 0.48 } else { 1.0 };
            let spin_scale = if heavy { 0.42 } else { 1.0 };

            let speed = (params.spread_speed + hash01(s + 6) * params.spread_rand) * speed_scale;
            let impact_push = if impact_dir.length_squared() > 1e-4 {
                impact_dir.normalize() * (speed * 0.25)
            } else {
                Vec3::ZERO
            };
            let vel = initial_vel * 0.6 + dir * speed + impact_push;

            let spin_axis = Vec3::new(
                hash01(s + 7) * 2.0 - 1.0,
                hash01(s + 8) * 2.0 - 1.0,
                hash01(s + 9) * 2.0 - 1.0,
            )
            .normalize_or_zero();
            let spin_deg =
                (params.rotate_deg + hash01(s + 10) * params.rotate_rand_deg) * spin_scale;
            let ang_vel = spin_axis * spin_deg.to_radians();
            let gravity = params.gravity + hash01(s + 11) * params.gravity_rand;
            (vel, ang_vel, gravity)
        } else {
            (Vec3::ZERO, Vec3::ZERO, 0.0)
        };

        pieces.push(GibPiece {
            joint,
            pos: tf.translation,
            rot: tf.rotation,
            base_scale: tf.scale,
            local_center: lc,
            radius: radii[i],
            vel,
            ang_vel,
            gravity,
            restitution: params.restitution,
            ground_time: 0.0,
            rest_secs: params.rest_secs + hash01(s + 12) * 0.8,
            age: 0.0,
            start_y: world_center.y,
            dynamic: is_dynamic[i],
            done: false,
        });
    }

    let mut missing = 0usize;
    let pack = texture_pack(disc, asset);
    for m in list {
        let mut mat = acvd_render::material_in_pack(
            disc,
            pack.as_deref(),
            &m,
            packs,
            images,
            flat,
            &mut missing,
        );
        let (alpha_mode, alpha_kind) = crate::map::mtd_alpha_mode(&m.mtd);
        mat.alpha_mode = alpha_mode;
        if alpha_kind != 0 {
            mat.cull_mode = None;
        }
        commands.spawn((
            Mesh3d(meshes.add(m.mesh)),
            MeshMaterial3d(materials.add(mat)),
            SkinnedMesh {
                inverse_bindposes: inv_bind_handle.clone(),
                joints: joint_entities.clone(),
            },
            NoFrustumCulling,
            Transform::default(),
            ChildOf(root),
        ));
    }

    commands.entity(root).insert(DebrisRig {
        label: label.to_string(),
        kind,
        pieces,
        has_static_base,
        settled: false,
        elapsed: 0.0,
    });
    Some(root)
}

/// Steps every active [`DebrisRig`] each sortie frame: integrates gib ballistic motion, bounces
/// chunks off [`Collision`], shrinks settled gibs away, and despawns finished rigs that have no
/// static rubble base.
pub fn step(
    time: Res<Time>,
    collision: Res<Collision>,
    mut commands: Commands,
    mut rigs: Query<(Entity, &mut DebrisRig)>,
    mut transforms: Query<&mut Transform>,
) {
    let dt = time.delta_secs().min(0.05);
    if dt <= 0.0 {
        return;
    }
    for (root, mut rig) in &mut rigs {
        if rig.settled {
            continue;
        }
        rig.elapsed += dt;
        let mut any_active = false;
        for piece in &mut rig.pieces {
            if !piece.dynamic || piece.done {
                continue;
            }
            if let Some(tf) = piece.step(dt, &collision) {
                if let Ok(mut joint_tf) = transforms.get_mut(piece.joint) {
                    *joint_tf = tf;
                }
            }
            if !piece.done {
                any_active = true;
            }
        }
        if !any_active {
            if rig.has_static_base {
                rig.settled = true;
            } else {
                commands.entity(root).try_despawn();
            }
        }
    }
}

/// Logs active debris state on `--shot` telemetry frames (`SHOT debris: ...`).
pub fn probe_shot(mark: Option<Res<acvd_render::app::ShotMark>>, rigs: Query<&DebrisRig>) {
    let Some(_path) = mark.as_ref().and_then(|m| m.path.as_ref()) else {
        return;
    };
    let mut rig_count = 0usize;
    let mut active_gibs = 0usize;
    let mut settled_rigs = 0usize;
    for rig in &rigs {
        rig_count += 1;
        if rig.settled {
            settled_rigs += 1;
        }
        active_gibs += rig.pieces.iter().filter(|p| p.dynamic && !p.done).count();
    }
    if rig_count > 0 {
        let sample = rigs
            .iter()
            .next()
            .map(|r| format!("{}({:?})", r.label, r.kind))
            .unwrap_or_default();
        println!(
            "SHOT debris: rigs={rig_count} active_gibs={active_gibs} settled_rigs={settled_rigs} first={sample}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acvd_formats::vfs;
    use crate::collision::{Collision, Layer, Plane};

    #[test]
    fn gib_piece_falls_bounces_settles_and_shrinks() {
        let col = Collision::with_hits(
            vec![Plane {
                y: 0.0,
                layer: Layer::Ground,
            }],
            Vec::new(),
        );

        let mut gib = GibPiece {
            joint: Entity::PLACEHOLDER,
            pos: Vec3::new(0.0, 10.0, 0.0),
            rot: Quat::IDENTITY,
            base_scale: Vec3::ONE,
            local_center: Vec3::new(0.5, 0.5, 0.0),
            radius: 1.0,
            vel: Vec3::new(6.0, 8.0, -4.0),
            ang_vel: Vec3::new(2.0, 3.0, 1.0),
            gravity: 30.0,
            restitution: 0.35,
            ground_time: 0.0,
            rest_secs: 1.0,
            age: 0.0,
            start_y: 10.5,
            dynamic: true,
            done: false,
        };

        let init_pos = gib.pos;
        let init_rot = gib.rot;
        let mut max_angle = 0.0f32;
        let mut bounced = false;
        let mut prev_vy = gib.vel.y;
        let mut last_tf = Transform::IDENTITY;

        for _ in 0..300 {
            if let Some(tf) = gib.step(1.0 / 60.0, &col) {
                last_tf = tf;
                max_angle = max_angle.max(init_rot.angle_between(gib.rot));
                if prev_vy < -1.5 && gib.vel.y > 0.0 {
                    bounced = true;
                }
                prev_vy = gib.vel.y;
            }
            if gib.done {
                break;
            }
        }

        assert!(gib.pos.distance(init_pos) > 3.0, "gib must travel away from spawn");
        assert!(max_angle > 0.5, "gib must tumble (max_angle={max_angle})");
        assert!(bounced, "gib must bounce on ground collision");
        assert!(gib.done, "gib must finish shrinking after settling on ground");
        assert_eq!(last_tf.scale, Vec3::ZERO);
    }

    #[test]
    fn gib_textures_resolve_inside_the_model_pack() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let Ok(disc) = Disc::open(&vfs::default_disc(&root)) else {
            return;
        };
        let mut packs = Packs::default();
        let cases = [
            (
                "model/ene/e0010/e0010_m.bnd.dcx|e0010_b.flv",
                "model/ene/e0010/e0010.tpf.dcx",
                "e0010_01",
                "e0010_01_n",
            ),
            (
                "model/obj/o2070/o2070_m.bnd.dcx|o2070_b.flv",
                "model/obj/o2070/o2070.tpf.dcx",
                "o2070_01",
                "o2070_01_s",
            ),
        ];
        for (asset, expected, diffuse, bump) in cases {
            let pack = texture_pack(&disc, asset).expect("gib model TPF exists on disc");
            assert_eq!(pack, expected, "{asset} should resolve to its own model TPF");
            assert!(
                packs.find(&disc, &pack, diffuse).unwrap().is_some(),
                "diffuse `{diffuse}` missing from {pack}"
            );
            assert!(
                packs.find(&disc, &pack, bump).unwrap().is_some(),
                "bump/specular `{bump}` missing from {pack}"
            );
            // The whole point of the pack-aware lookup: this name exists in several packs, and a
            // bare name-only lookup picks a different (brighter) one than the model's own pack.
            let global = acvd_render::best_texture_ref(diffuse).map(|t| t.pack);
            assert_ne!(
                global.as_deref(),
                Some(pack.as_str()),
                "`{diffuse}` must be a genuine cross-pack collision to exercise the fix"
            );
        }
    }

    #[test]
    fn enemy_and_destructible_prop_broken_flvers_spawn_moving_gibs() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let Ok(disc) = Disc::open(&vfs::default_disc(&root)) else {
            return;
        };

        let mut app = App::new();
        app.insert_resource(Assets::<Mesh>::default());
        app.insert_resource(Assets::<StandardMaterial>::default());
        app.insert_resource(Assets::<Image>::default());
        app.insert_resource(Assets::<SkinnedMeshInverseBindposes>::default());
        let col = Collision::with_hits(
            vec![Plane {
                y: 0.0,
                layer: Layer::Ground,
            }],
            Vec::new(),
        );

        let mut packs = Packs::default();
        let cases = [
            ("e0010", "model/ene/e0010/e0010_m.bnd.dcx|e0010_b.flv", DebrisKind::Enemy, false),
            ("e1020", "model/ene/e1020/e1020_m.bnd.dcx|e1020_b.flv", DebrisKind::Enemy, false),
            ("e2030", "model/ene/e2030/e2030_m.bnd.dcx|e2030_b.flv", DebrisKind::Enemy, false),
            ("o2070", "model/obj/o2070/o2070_m.bnd.dcx|o2070_b.flv", DebrisKind::Prop, false),
            ("o2108", "model/obj/o2108/o2108_m.bnd.dcx|o2108_b.flv", DebrisKind::Prop, true),
            ("o4235", "model/obj/o4235/o4235_m.bnd.dcx|o4235_b.flv", DebrisKind::Prop, true),
        ];

        for (label, asset, kind, expect_static_base) in cases {
            let spawned = {
                let world = app.world_mut();
                world.resource_scope(|world, mut meshes: Mut<Assets<Mesh>>| {
                    world.resource_scope(|world, mut materials: Mut<Assets<StandardMaterial>>| {
                        world.resource_scope(|world, mut images: Mut<Assets<Image>>| {
                            world.resource_scope(
                                |world, mut bindposes: Mut<Assets<SkinnedMeshInverseBindposes>>| {
                                    let mut cmds = world.commands();
                                    let ent = spawn(
                                        &mut cmds,
                                        &mut meshes,
                                        &mut materials,
                                        &mut images,
                                        &mut bindposes,
                                        &disc,
                                        &mut packs,
                                        true,
                                        asset,
                                        label,
                                        kind,
                                        Transform::from_xyz(0.0, 5.0, 0.0),
                                        Vec3::ZERO,
                                        Vec3::NEG_Z,
                                    );
                                    world.flush();
                                    ent
                                },
                            )
                        })
                    })
                })
            };

            let root_ent = spawned.unwrap_or_else(|| panic!("{label} should spawn DebrisRig"));
            let rig = app.world().get::<DebrisRig>(root_ent).unwrap();
            assert_eq!(
                rig.has_static_base, expect_static_base,
                "{label} static base mismatch"
            );
            let dyn_count = rig.pieces.iter().filter(|p| p.dynamic).count();
            assert!(dyn_count >= 5, "{label} should have >=5 dynamic gibs, got {dyn_count}");

            // Step 30 frames (0.5 s) and verify dynamic gibs actually move & rotate (never freeze).
            let init_positions: Vec<Vec3> = rig
                .pieces
                .iter()
                .filter(|p| p.dynamic)
                .map(|p| p.pos)
                .collect();
            let mut rig_mut = app.world_mut().get_mut::<DebrisRig>(root_ent).unwrap();
            for _ in 0..30 {
                for p in &mut rig_mut.pieces {
                    p.step(1.0 / 60.0, &col);
                }
            }
            let moved = rig_mut
                .pieces
                .iter()
                .filter(|p| p.dynamic)
                .zip(init_positions.iter())
                .filter(|(p, &p0)| p.pos.distance(p0) > 0.5)
                .count();
            assert_eq!(
                moved, dyn_count,
                "{label}: all {dyn_count} dynamic gibs must move after spawn, only {moved} moved"
            );
        }

        // Verify pure static rubble replacement (`o2106_b.flv`, 1 bone, no break* bones) returns None.
        let static_only = {
            let world = app.world_mut();
            world.resource_scope(|world, mut meshes: Mut<Assets<Mesh>>| {
                world.resource_scope(|world, mut materials: Mut<Assets<StandardMaterial>>| {
                    world.resource_scope(|world, mut images: Mut<Assets<Image>>| {
                        world.resource_scope(
                            |world, mut bindposes: Mut<Assets<SkinnedMeshInverseBindposes>>| {
                                let mut cmds = world.commands();
                                let ent = spawn(
                                    &mut cmds,
                                    &mut meshes,
                                    &mut materials,
                                    &mut images,
                                    &mut bindposes,
                                    &disc,
                                    &mut packs,
                                    true,
                                    "model/obj/o2106/o2106_m.bnd.dcx|o2106_b.flv",
                                    "o2106",
                                    DebrisKind::Prop,
                                    Transform::IDENTITY,
                                    Vec3::ZERO,
                                    Vec3::ZERO,
                                );
                                world.flush();
                                ent
                            },
                        )
                    })
                })
            })
        };
        assert!(
            static_only.is_none(),
            "o2106_b.flv has no break* bones and should fall back to static broken_mesh"
        );
    }

    #[test]
    fn step_system_updates_joint_transforms_and_cleans_up_finished_rigs() {
        let mut app = App::new();
        app.insert_resource(Time::<()>::default());
        app.insert_resource(Collision::with_hits(
            vec![Plane {
                y: 0.0,
                layer: Layer::Ground,
            }],
            Vec::new(),
        ));
        app.add_systems(Update, step);

        // 1. Enemy-style rig (no static base): should move joints then despawn when finished.
        let enemy_joint = app.world_mut().spawn(Transform::from_xyz(0.0, 2.0, 0.0)).id();
        let enemy_root = app
            .world_mut()
            .spawn((
                DebrisRoot,
                DebrisRig {
                    label: "test_enemy".into(),
                    kind: DebrisKind::Enemy,
                    pieces: vec![GibPiece {
                        joint: enemy_joint,
                        pos: Vec3::new(0.0, 2.0, 0.0),
                        rot: Quat::IDENTITY,
                        base_scale: Vec3::ONE,
                        local_center: Vec3::ZERO,
                        radius: 0.5,
                        vel: Vec3::new(5.0, 4.0, 0.0),
                        ang_vel: Vec3::new(1.5, 2.0, 0.5),
                        gravity: 30.0,
                        restitution: 0.25,
                        ground_time: 0.0,
                        rest_secs: 0.2,
                        age: 0.0,
                        start_y: 2.0,
                        dynamic: true,
                        done: false,
                    }],
                    has_static_base: false,
                    settled: false,
                    elapsed: 0.0,
                },
            ))
            .id();

        // 2. Prop-style rig with static base: dynamic gib moves and shrinks, static base stays.
        let base_joint = app.world_mut().spawn(Transform::from_xyz(10.0, 0.0, 0.0)).id();
        let gib_joint = app.world_mut().spawn(Transform::from_xyz(10.0, 3.0, 0.0)).id();
        let prop_root = app
            .world_mut()
            .spawn((
                DebrisRoot,
                DebrisRig {
                    label: "test_prop".into(),
                    kind: DebrisKind::Prop,
                    pieces: vec![
                        GibPiece {
                            joint: base_joint,
                            pos: Vec3::new(10.0, 0.0, 0.0),
                            rot: Quat::IDENTITY,
                            base_scale: Vec3::ONE,
                            local_center: Vec3::ZERO,
                            radius: 1.0,
                            vel: Vec3::ZERO,
                            ang_vel: Vec3::ZERO,
                            gravity: 0.0,
                            restitution: 0.0,
                            ground_time: 0.0,
                            rest_secs: 0.0,
                            age: 0.0,
                            start_y: 0.0,
                            dynamic: false,
                            done: false,
                        },
                        GibPiece {
                            joint: gib_joint,
                            pos: Vec3::new(10.0, 3.0, 0.0),
                            rot: Quat::IDENTITY,
                            base_scale: Vec3::ONE,
                            local_center: Vec3::ZERO,
                            radius: 0.5,
                            vel: Vec3::new(-4.0, 5.0, 2.0),
                            ang_vel: Vec3::new(2.0, 1.0, 1.5),
                            gravity: 30.0,
                            restitution: 0.25,
                            ground_time: 0.0,
                            rest_secs: 0.2,
                            age: 0.0,
                            start_y: 3.0,
                            dynamic: true,
                            done: false,
                        },
                    ],
                    has_static_base: true,
                    settled: false,
                    elapsed: 0.0,
                },
            ))
            .id();

        // Step 15 frames (0.25 s): both dynamic joints must have moved in ECS Transform.
        for _ in 0..15 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(1.0 / 60.0));
            app.update();
        }
        let enemy_tf = *app.world().get::<Transform>(enemy_joint).unwrap();
        let gib_tf = *app.world().get::<Transform>(gib_joint).unwrap();
        assert!(
            enemy_tf.translation.distance(Vec3::new(0.0, 2.0, 0.0)) > 0.5,
            "enemy joint Transform must move via debris::step"
        );
        assert!(
            gib_tf.translation.distance(Vec3::new(10.0, 3.0, 0.0)) > 0.5,
            "prop gib joint Transform must move via debris::step"
        );

        // Step remaining frames until settled and shrunk.
        for _ in 0..240 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(1.0 / 60.0));
            app.update();
        }
        assert!(
            app.world().get_entity(enemy_root).is_err(),
            "enemy debris rig with no static base must despawn once gibs finish"
        );
        let prop_rig = app.world().get::<DebrisRig>(prop_root).unwrap();
        assert!(prop_rig.settled, "prop rig with static base must settle");
        assert_eq!(
            app.world().get::<Transform>(base_joint).unwrap().scale,
            Vec3::ONE,
            "static base joint must stay intact"
        );
        assert_eq!(
            app.world().get::<Transform>(gib_joint).unwrap().scale,
            Vec3::ZERO,
            "finished prop gib joint must shrink to zero"
        );
    }
}
