//! Booster nozzles. Each mounted root (`l_boost`, `l2_boost`, `r_boost`, `r2_boost`) keeps the
//! attach pose from `sheets/assembly_slots.csv` and adds the rotation of
//! `model/ac/motion/boost/boost_a.bnd.dcx` (`sheets/booster_anim.csv`).
//!
//! The bank is `model_ac:\motion\boost\boost` (360 `0x8284e078`, called from `0x8284e9d8` with
//! the empty string and `"boost"`). `AC_MOTIONINFO.BoosterAnimID` (byte +8) selects
//! `a00_<id>.ani`. `0x82882d80` reads that byte into the anim command and stores the same start
//! time the body clip gets, plus `Booster_Frame`. Clips of 360 frames are direction wheels (a
//! key every 45 degrees, or every 5 on the high-boost clip); the others are a single pose.

use acvd_data::generated::ac_unit::{PARAM_ACANIMHOKAN_BIN, PARAM_ACMOTION_BIN};
use acvd_data::{ac_state, find};
use acvd_formats::ani;
use acvd_formats::vfs::{self, Disc};
use acvd_render::Rig;
use bevy::prelude::*;

use crate::assemble::Placement;
use crate::control::{self, Pilot};
use crate::pose::{self, Fade, Motion};

/// `a00_000` .. `a00_004`.
const CLIPS: u8 = 5;
const BINDER: &str = "model/ac/motion/boost/boost_a.bnd.dcx";

const ROOTS: [&str; 4] = ["l_boost", "l2_boost", "r_boost", "r2_boost"];

/// One mounted booster root, driven by the bone of the same name in the boost clip.
struct Nozzle {
    entity: Entity,
    bone: usize,
    /// Spawned local rotation: the attach orientation, in Bevy axes.
    base: Option<Quat>,
    /// Shown rotation when the current clip started blending in.
    from: Quat,
}

/// The boost motion bank for the shown AC.
#[derive(Resource)]
pub struct BoostPose {
    skeleton: ani::Anim,
    clips: Vec<ani::Anim>,
    nozzles: Vec<Nozzle>,
    clip: usize,
    frame: f32,
    fade_secs: f32,
    fade_t: f32,
}

impl BoostPose {
    /// Loads the bank and binds `nozzles` (entity, bone name) to its skeleton.
    pub fn load(disc: &Disc, nozzles: Vec<(Entity, String)>) -> Option<Self> {
        let skeleton = open(disc, 0).ok()?;
        let mut clips = Vec::with_capacity(CLIPS as usize);
        for id in 0..CLIPS {
            let clip = open(disc, id).ok()?;
            if clip.bones.len() != skeleton.bones.len() {
                return None;
            }
            clips.push(clip);
        }
        let nozzles = nozzles
            .into_iter()
            .filter_map(|(entity, name)| {
                let bone = skeleton
                    .bones
                    .iter()
                    .position(|b| b.rest.as_ref().is_some_and(|r| r.name == name))?;
                Some(Nozzle {
                    entity,
                    bone,
                    base: None,
                    from: Quat::IDENTITY,
                })
            })
            .collect();
        Some(Self {
            skeleton,
            clips,
            nozzles,
            clip: 0,
            frame: 0.0,
            fade_secs: 0.0,
            fade_t: 0.0,
        })
    }
}

fn open(disc: &Disc, id: u8) -> anyhow::Result<ani::Anim> {
    let path = format!("{BINDER}|a00_{id:03}.ani");
    Ok(ani::read(&vfs::open(disc, &path)?)?)
}

/// Mounted booster roots of one placement. The model carries all four bones; only the roots
/// this slot actually sits on a socket are driven.
pub fn note(placement: &Placement, rig: &Rig, joints: &[Entity], out: &mut Vec<(Entity, String)>) {
    for mount in &placement.mounts {
        if !ROOTS.contains(&mount.root) {
            continue;
        }
        let Some(i) = rig.bones.iter().position(|b| b.name == mount.root) else {
            continue;
        };
        if let Some(&entity) = joints.get(i) {
            out.push((entity, mount.root.to_string()));
        }
    }
}

/// A booster root its core has no socket for (`Placement::hidden`). Zero scale collapses its
/// skinned vertices, children and effect points, as the 360's draw-bit clear does.
#[derive(Component)]
pub struct Folded;

/// Marks `placement`'s hidden roots.
pub fn fold(commands: &mut Commands, placement: &Placement, rig: &Rig, joints: &[Entity]) {
    for root in &placement.hidden {
        let Some(i) = rig.bones.iter().position(|b| b.name == *root) else {
            continue;
        };
        if let Some(&entity) = joints.get(i) {
            commands.entity(entity).insert(Folded);
        }
    }
}

pub fn hide(mut joints: Query<&mut Transform, With<Folded>>) {
    for mut transform in &mut joints {
        transform.scale = Vec3::ZERO;
    }
}

/// Parent-space change from the clip's rest pose, mirrored into Bevy and applied on top of the
/// mounted rotation. The named clip's keys are that rest pose, so an idle nozzle does not move.
pub(crate) fn nozzle_rotation(base: Quat, rest_euler: [f32; 3], clip_q: [f32; 4]) -> Quat {
    let rest = Quat::from_array(ani::euler_quat(rest_euler));
    let delta = (Quat::from_array(clip_q) * rest.inverse()).normalize();
    let mirrored = Quat::from_xyzw(delta.x, -delta.y, -delta.z, delta.w);
    mirrored * base
}

fn approach(frame: f32, target: f32, step: f32) -> f32 {
    let diff = (target - frame + 540.0).rem_euclid(360.0) - 180.0;
    (frame + diff.clamp(-step, step)).rem_euclid(360.0)
}

/// Poses the nozzles from the playing `acmotion` row's `BoosterAnimID`.
pub fn pose(
    time: Res<Time>,
    pilots: Query<&Pilot>,
    motion: Option<Res<Motion>>,
    boost: Option<ResMut<BoostPose>>,
    mut joints: Query<&mut Transform>,
) {
    let Some(mut boost) = boost else { return };
    let Ok(pilot) = pilots.single() else { return };
    let (state, hokan_base) = pilot.pose();
    let (id, fade) = booster_clip(state, hokan_base);
    let id = id.min(boost.clips.len().saturating_sub(1));
    let changed = id != boost.clip;
    if changed {
        boost.clip = id;
        boost.fade_secs = fade;
        boost.fade_t = 0.0;
    }
    let dt = time.delta_secs();
    boost.fade_t += dt;
    let wheeled = boost.clips[id].frames == 360;
    if wheeled {
        if motion.as_ref().is_some_and(|m| m.wheel.is_some()) {
            boost.frame = motion.as_ref().map(|m| m.frame).unwrap_or(boost.frame);
        } else if pilot.velocity.with_y(0.0).length() > 0.02 {
            let target = control::lean_heading(pilot.yaw, pilot.velocity, Vec2::ZERO);
            boost.frame = approach(boost.frame, target, pose::WHEEL_RATE * dt);
        }
    } else {
        boost.frame = 0.0;
    }
    let frame = boost.frame;
    let w = if boost.fade_secs > 0.0 {
        (boost.fade_t / boost.fade_secs).min(1.0)
    } else {
        1.0
    };
    let samples: Vec<([f32; 3], [f32; 4])> = (0..boost.skeleton.bones.len())
        .map(|i| {
            let rest = boost.skeleton.bones[i]
                .rest
                .as_ref()
                .map(|r| r.euler)
                .unwrap_or([0.0; 3]);
            let clip_q = boost.clips[id].bones[i]
                .track
                .rotation(frame)
                .unwrap_or_else(|| ani::euler_quat(rest));
            (rest, clip_q)
        })
        .collect();
    for nozzle in &mut boost.nozzles {
        let Ok(mut transform) = joints.get_mut(nozzle.entity) else {
            continue;
        };
        if changed {
            nozzle.from = transform.rotation;
        }
        let base = *nozzle.base.get_or_insert(transform.rotation);
        let (rest, clip_q) = samples[nozzle.bone];
        let target = nozzle_rotation(base, rest, clip_q);
        transform.rotation = if w >= 1.0 {
            target
        } else {
            nozzle.from.slerp(target, w)
        };
    }
}

/// `(clip index, blend seconds)` for the playing state. No row plays the rest clip.
fn booster_clip(state: Option<(&str, Option<u8>)>, hokan_base: u32) -> (usize, f32) {
    let Some((name, direction)) = state else {
        return (0, 0.0);
    };
    let Some(row) = ac_state(name, direction).and_then(|s| find(PARAM_ACMOTION_BIN, s.row)) else {
        return (0, 0.0);
    };
    let fade = find(
        PARAM_ACANIMHOKAN_BIN,
        hokan_base + u32::from(row.data.interpolate_id),
    )
    .map(|h| Fade::booster(&h.data))
    .unwrap_or(0.0);
    (usize::from(row.data.booster_anim_id), fade)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rest_pose_leaves_the_mounted_rotation() {
        let rest = [-0.69813174, 0.0, -0.17453296];
        let base = Quat::from_rotation_y(0.4);
        let out = nozzle_rotation(base, rest, ani::euler_quat(rest));
        assert!(
            out.angle_between(base) < 1e-3,
            "idle delta should be zero, got {} deg",
            out.angle_between(base).to_degrees()
        );
    }

    #[test]
    fn wheel_takes_the_short_way_around() {
        let next = approach(10.0, 350.0, 5.0);
        assert!((next - 5.0).abs() < 1e-3, "{next}");
    }

    #[test]
    fn heading_matches_the_body_lean() {
        assert!(control::lean_heading(0.0, Vec3::NEG_Z, Vec2::ZERO).abs() < 1e-3);
        let left = control::lean_heading(0.0, Vec3::NEG_X, Vec2::ZERO);
        assert!((left - 90.0).abs() < 1e-3, "{left}");
    }

    #[test]
    fn states_pick_the_booster_clip() {
        let id = |state, dir| booster_clip(Some((state, dir)), 0).0;
        assert_eq!(id("idle", None), 0);
        assert_eq!(id("dash", Some(0)), 1);
        assert_eq!(id("air_move", Some(0)), 1);
        assert_eq!(id("jump", None), 2);
        assert_eq!(id("quick_boost", Some(0)), 4);
        assert_eq!(id("glide", None), 1);
        assert_eq!(id("glide_air_in", Some(0)), 1);
        assert_eq!(id("glide_air_in", Some(2)), 1);
        assert_eq!(id("glide_air", None), 3);
        assert_eq!(booster_clip(Some(("dash", Some(0))), 0).1, 0.015);
    }

    #[test]
    fn boost_bank_wheels_and_rest() {
        let iso = acvd_formats::vfs::repo_root().join(acvd_formats::vfs::X360_ISO);
        let Ok(disc) = Disc::open(&iso) else { return };
        let skeleton = open(&disc, 0).unwrap();
        assert!(skeleton.named());
        assert_eq!(skeleton.frames, 1);
        let names: Vec<_> = skeleton
            .bones
            .iter()
            .filter_map(|b| b.rest.as_ref().map(|r| r.name.as_str()))
            .collect();
        assert_eq!(names, ["r_boost", "l_boost", "r2_boost", "l2_boost"]);
        for (i, bone) in skeleton.bones.iter().enumerate() {
            let rest = bone.rest.as_ref().unwrap();
            let q = bone.track.rotation(0.0).unwrap();
            let delta = nozzle_rotation(Quat::IDENTITY, rest.euler, q);
            assert!(
                delta.angle_between(Quat::IDENTITY) < 1e-3,
                "a00_000 bone {i} is not the rest pose"
            );
        }
        let dash = open(&disc, 1).unwrap();
        let quick = open(&disc, 4).unwrap();
        assert_eq!(dash.frames, 360);
        assert_eq!(quick.frames, 360);
        assert_eq!(open(&disc, 2).unwrap().frames, 1);
        let moved = dash.bones[0].track.rotation(0.0).unwrap();
        let rest = skeleton.bones[0].rest.as_ref().unwrap();
        let pitched = nozzle_rotation(Quat::IDENTITY, rest.euler, moved);
        assert!(
            pitched.angle_between(Quat::IDENTITY).to_degrees() > 15.0,
            "forward boost should pitch the nozzle"
        );
    }
}
