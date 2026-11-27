//! `NetView`: spawns real assembled AC rigs for remote players and drives their transforms,
//! locomotion clips, pose clips, bay/rack swaps, and booster/glide trail markers from snapshots.

use std::collections::{HashMap, HashSet};

use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::prelude::*;

use acvd_data::generated::ac_unit::PARAM_ACMOTION_BIN;
use acvd_data::{ac_state, find};
use acvd_proto::ac_flags;
use acvd_proto::NetId;
use acvd_render::Packs;
use acvd_sim::world::pose_state;

use super::session::NetRuntime;
use crate::pose::NpcMotion;
use crate::{paint, sfx, targets, Garage};

/// Remote AC view bookkeeping.
#[derive(Resource, Default)]
pub struct NetView {
    pub acs: HashMap<u8, Entity>,
    /// Remote ACs waiting for their respawn drop; hidden and not driven.
    pub dead: HashSet<u8>,
    packs: Packs,
}

/// Marks a remote AC root.
#[derive(Component)]
pub struct RemotePlayer {
    pub net_id: u8,
}

/// Per-frame remote LOD gate: `active` is false for far/off-screen rigs so
/// [`crate::pose::animate_npcs`] skips their skeletal updates.
#[derive(Component)]
pub struct RemoteLod {
    pub active: bool,
}

/// Simple non-particle trail marker pacing for remote boosters.
#[derive(Component, Default)]
pub struct RemoteFx {
    timer: f32,
    last_pose: u8,
}

/// Hand/rack weapon mesh entities per side, plus the replicated bay state.
#[derive(Component, Default)]
pub struct RemoteRack {
    /// `[side][0]` hand weapon meshes, `[side][1]` hanger/bay weapon meshes.
    slots: [[Vec<Entity>; 2]; 2],
    active: [bool; 2],
    swapping: [bool; 2],
    /// Remaining seconds of the shoulder bay-shift animation.
    anim: f32,
}

impl RemoteRack {
    fn new(parts: &[(&'static str, Entity)], flags: u8) -> Self {
        let mut rack = Self::default();
        for (column, entity) in parts {
            let slot = match *column {
                "armwep_r" => Some((0, 0)),
                "hanger_r" => Some((0, 1)),
                "armwep_l" => Some((1, 0)),
                "hanger_l" => Some((1, 1)),
                _ => None,
            };
            if let Some((side, slot)) = slot {
                rack.slots[side][slot].push(*entity);
            }
        }
        rack.active = [
            flags & acvd_proto::combat_flags::RIGHT_BAY_ACTIVE != 0,
            flags & acvd_proto::combat_flags::LEFT_BAY_ACTIVE != 0,
        ];
        rack
    }

    fn apply_visibility(&self, commands: &mut Commands) {
        for side in 0..2 {
            let active = self.active[side];
            for entity in &self.slots[side][0] {
                commands.entity(*entity).try_insert(if active {
                    Visibility::Hidden
                } else {
                    Visibility::Visible
                });
            }
            for entity in &self.slots[side][1] {
                commands.entity(*entity).try_insert(if active {
                    Visibility::Visible
                } else {
                    Visibility::Hidden
                });
            }
        }
    }

    fn set_active(&mut self, side: usize, active: bool, commands: &mut Commands) {
        if self.active[side] == active {
            return;
        }
        self.active[side] = active;
        for entity in &self.slots[side][0] {
            commands.entity(*entity).try_insert(if active {
                Visibility::Hidden
            } else {
                Visibility::Visible
            });
        }
        for entity in &self.slots[side][1] {
            commands.entity(*entity).try_insert(if active {
                Visibility::Visible
            } else {
                Visibility::Hidden
            });
        }
    }

    /// Applies replicated bay bits and plays the 150-frame shoulder bay-shift clip on a rising
    /// swap bit. Returns true while the swap clip owns the body animation.
    fn sync_flags(
        &mut self,
        flags: u8,
        commands: &mut Commands,
        npc: &mut NpcMotion,
        disc: &acvd_formats::vfs::Disc,
        dt: f32,
    ) -> bool {
        let swapping = [
            flags & acvd_proto::combat_flags::RIGHT_SWAPPING != 0,
            flags & acvd_proto::combat_flags::LEFT_SWAPPING != 0,
        ];
        for side in 0..2 {
            self.set_active(
                side,
                flags
                    & if side == 0 {
                        acvd_proto::combat_flags::RIGHT_BAY_ACTIVE
                    } else {
                        acvd_proto::combat_flags::LEFT_BAY_ACTIVE
                    }
                    != 0,
                commands,
            );
            if swapping[side] && !self.swapping[side] {
                let name = if side == 0 {
                    "bay_shift_r"
                } else {
                    "bay_shift_l"
                };
                if let Some(row) =
                    ac_state(name, None).and_then(|s| find(PARAM_ACMOTION_BIN, s.row))
                {
                    let _ = npc
                        .motion
                        .play(disc, row.data.anim_id, false, 1.0, [0.12; 8]);
                    self.anim = 2.5;
                }
            }
        }
        self.swapping = swapping;
        if self.anim > 0.0 {
            self.anim = (self.anim - dt).max(0.0);
            npc.step_clock(dt);
            true
        } else {
            false
        }
    }
}

/// Spawns/despawns remote rigs and updates their transforms and animation each frame.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn sync(
    time: Res<Time>,
    garage: Res<Garage>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut paint_materials: ResMut<Assets<paint::AcPaintMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
    runtime: Res<NetRuntime>,
    mut view: ResMut<NetView>,
    mut rig_cache: ResMut<targets::AcRigCache>,
    cams: Query<
        (&Camera, &GlobalTransform),
        (With<Camera3d>, Without<crate::env::WaterReflectionCamera>),
    >,
    mut remotes: Query<(
        Entity,
        &RemotePlayer,
        &mut Transform,
        Option<&mut NpcMotion>,
        Option<&mut RemoteFx>,
        Option<&mut RemoteRack>,
        Option<&mut RemoteLod>,
    )>,
) {
    let session = &runtime.session;
    let mut packs = std::mem::take(&mut view.packs);
    let mut missing = 0usize;

    // Despawn ACs that left or whose loadout vanished.
    let stale: Vec<(u8, Entity)> = view
        .acs
        .iter()
        .filter(|(id, _)| {
            !session
                .loadouts
                .keys()
                .any(|net_id| net_id.get() == **id && Some(*net_id) != session.local_id)
        })
        .map(|(id, entity)| (*id, *entity))
        .collect();
    for (id, entity) in stale {
        commands.entity(entity).try_despawn();
        view.acs.remove(&id);
        view.dead.remove(&id);
    }

    // Spawn rigs for remote players once their loadout and first position are known.
    let remote_loadouts: Vec<(NetId, acvd_proto::Loadout)> = session
        .loadouts
        .iter()
        .filter(|(id, _)| Some(**id) != session.local_id)
        .map(|(id, loadout)| (*id, *loadout))
        .collect();
    for (net_id, loadout) in remote_loadouts {
        if view.acs.contains_key(&net_id.get()) {
            continue;
        }
        let Some(sample) = session.remote_view(net_id) else {
            continue;
        };
        let design = acvd_sim::stats::design_from_loadout(&loadout);
        let xf = Transform::from_translation(sample.pos);
        let Some(rig) = targets::spawn_ac_rig(
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut paint_materials,
            &mut images,
            &mut bindposes,
            &garage.disc,
            &mut packs,
            &mut missing,
            &mut rig_cache,
            &design,
            &format!("remote AC {net_id}"),
            xf,
            garage.flat,
        ) else {
            warn!("net: could not assemble remote AC {net_id}");
            continue;
        };
        commands.entity(rig.root).try_insert((
            RemotePlayer {
                net_id: net_id.get(),
            },
            RemoteFx::default(),
            RemoteLod { active: true },
        ));
        let rack = RemoteRack::new(&rig.parts, sample.combat_flags);
        rack.apply_visibility(&mut commands);
        commands.entity(rig.root).try_insert(rack);
        if let Some(npc) = rig.npc {
            commands.entity(rig.root).try_insert(npc);
        }
        view.acs.insert(net_id.get(), rig.root);
    }

    // Drive remote transforms, locomotion, bay swaps, and booster/glide effects. Rigs farther
    // than 250 m or outside the camera frustum are hidden and their skeletal animation is
    // skipped; `pose::animate_npcs` also checks [`RemoteLod`] so far rigs cost only a transform
    // write per frame.
    let dt = time.delta_secs();
    let cam = cams.iter().next();
    let cam_pos = cam.map_or(Vec3::ZERO, |(_, tf)| tf.translation());
    let remote_active = |pos: Vec3| -> bool {
        if cam_pos.distance_squared(pos) > 250.0 * 250.0 {
            return false;
        }
        match cam {
            Some((camera, tf)) => {
                if (pos - cam_pos).dot(tf.forward().as_vec3()) <= 0.0 {
                    return false;
                }
                camera.world_to_ndc(tf, pos).is_some_and(|p| {
                    p.x.abs() <= 1.25 && p.y.abs() <= 1.25 && (0.0..=1.0).contains(&p.z)
                })
            }
            None => true,
        }
    };
    let mut fx: Vec<(Vec3, i32)> = Vec::new();
    for (root, player, mut transform, npc, marker, rack, lod) in &mut remotes {
        let id = NetId::new(player.net_id).ok();
        let Some(view_sample) = id.and_then(|id| session.remote_view(id)) else {
            continue;
        };
        let dead =
            view.dead.contains(&player.net_id) || view_sample.flags & ac_flags::DESTROYED != 0;
        if dead {
            if let Some(mut lod) = lod {
                lod.active = false;
            }
            commands.entity(root).try_insert(Visibility::Hidden);
            continue;
        }
        transform.translation = view_sample.pos;
        transform.rotation = Quat::from_rotation_y(view_sample.yaw);
        let active = remote_active(view_sample.pos);
        if let Some(mut lod) = lod {
            lod.active = active;
        }
        if !active {
            commands.entity(root).try_insert(Visibility::Hidden);
            continue;
        }
        commands.entity(root).try_insert(Visibility::Visible);
        let speed_ms = view_sample.vel.length() * 60.0;
        let walk_max_ms = id
            .and_then(|id| session.loadouts.get(&id))
            .map(|loadout| {
                acvd_sim::stats::stats_from_loadout(loadout)
                    .ctrl
                    .walk_max_tick
                    .max(0.02)
                    * 60.0
            })
            .unwrap_or(9.0);
        match (npc, rack) {
            (Some(mut npc), Some(mut rack)) => {
                let rack_animating = rack.sync_flags(
                    view_sample.combat_flags,
                    &mut commands,
                    &mut npc,
                    &garage.disc,
                    dt,
                );
                if !rack_animating {
                    drive_animation(&garage, &mut npc, &view_sample, speed_ms, walk_max_ms, dt);
                }
            }
            (Some(mut npc), None) => {
                drive_animation(&garage, &mut npc, &view_sample, speed_ms, walk_max_ms, dt);
            }
            _ => {}
        }
        if let Some(mut marker) = marker {
            marker.timer -= dt;
            if view_sample.flags & ac_flags::GLIDE != 0 && speed_ms > 3.0 && marker.timer <= 0.0 {
                marker.timer = 0.2;
                fx.push((view_sample.pos + Vec3::Y * 2.0, 131));
            }
            if view_sample.pose == pose_state::QB && marker.last_pose != pose_state::QB {
                fx.push((view_sample.pos + Vec3::Y * 2.0, 305));
            }
            marker.last_pose = view_sample.pose;
        }
    }
    view.packs = packs;
    for (position, effect) in fx {
        commands.spawn((sfx::Sfx::new(effect), Transform::from_translation(position)));
    }

    let _ = &session;
}

fn drive_animation(
    garage: &Garage,
    npc: &mut NpcMotion,
    sample: &super::interpolation::SampledView,
    speed_ms: f32,
    walk_max_ms: f32,
    _dt: f32,
) {
    let local_vel = Quat::from_rotation_y(-sample.yaw).mul_vec3(sample.vel * 60.0);
    if let Some((row, looping)) = pose_row(sample.pose) {
        if let Some(clip) = find(PARAM_ACMOTION_BIN, row) {
            let _ = npc
                .motion
                .play(&garage.disc, clip.data.anim_id, looping, 1.0, [0.12; 8]);
            npc.motion.wheel = None;
        }
        return;
    }
    npc.set_locomotion(
        &garage.disc,
        local_vel,
        walk_max_ms.max(2.0),
        Some((0.0, sample.pitch)),
    );
    let _ = speed_ms;
}

/// Extra pose clips the simulation pose ids select, returning the `param/acmotion.bin` row.
fn pose_row(pose: u8) -> Option<(u32, bool)> {
    match pose {
        pose_state::QB => Some((285, false)),
        pose_state::WALL_JUMP => Some((232, false)),
        pose_state::KICK => Some((243, false)),
        pose_state::BLADE => Some((100, false)),
        pose_state::GLIDE => Some((223, true)),
        pose_state::GLIDE_AIR => Some((222, true)),
        pose_state::JUMP => Some((59, false)),
        pose_state::LAND => Some((60, false)),
        pose_state::RISE => Some((62, true)),
        pose_state::FALL => Some((65, true)),
        pose_state::AIR_MOVE => Some((68, true)),
        // Locomotion picks from the actual velocity; boosters do not need a special body clip.
        _ => None,
    }
}
