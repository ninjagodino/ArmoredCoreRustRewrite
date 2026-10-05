//! Posing: one joint hierarchy for the assembled AC, driven by the motion set its legs select
//! (`acvd_data::ac_motion`). Every part bone is a joint; bones the clip names take its rotations,
//! keeping the part's own bone lengths, so one clip drives every leg of its type. Clip bones no
//! part has (`master`, `center`, `c_center`, `k_center`) become joints of their own, and part
//! roots hanging from them (`kosi`, `core`) take the clip's translations scaled to the legs.
//! Part roots without a clip bone (weapons, boosters, shoulders) ride the bone carrying the
//! socket they attach to.
//!
//! Joints are solved in FLVER axes and mirrored on X into Bevy's, like the meshes.

use std::collections::HashMap;
use std::path::Path;

use acvd_data::generated::ac_unit::AcanimHokanparamSt;
use acvd_data::{Asset, SocketRef};
use acvd_formats::{ani, flver, vfs};
use acvd_render::{Rig, RigBone};
use anyhow::{Context, Result};
use bevy::math::{Affine3A, Mat3};
use bevy::prelude::*;

use crate::assemble::Placement;

/// `.ani` headers store no frame rate; the game's 60 Hz tick is assumed (30 fps played far too slow).
pub const FRAME_RATE: f32 = 60.0;

/// How fast the lean follows the heading, degrees per second (assumed; the 360 rate is unread).
const WHEEL_RATE: f32 = 540.0;

/// Bone-group columns of `param/acanimhokan.bin` after GeneralFrame, by skeleton bone name
/// (`sheets/anim_blend.csv`). A joint takes the column of its nearest ancestor-or-self in the
/// list, GeneralFrame otherwise; `momo` and `sune` name the `l_`/`r_` thigh and shin bones.
const HOKAN_GROUPS: [&str; 7] = ["center", "c_center", "k_center", "core", "kosi", "momo", "sune"];

/// Hokan values per second: milliseconds (at 60 Hz, 2 s idle and 20 s tank fades were far too slow;
/// ms gives 60 ms walk, ~1 s landing, `sheets/anim_blend.csv`, unverified).
const HOKAN_FRAME_RATE: f32 = 1000.0;

/// A joint the clip drives: clip bone `bone`; `absolute` joints take the clip's translation
/// (scaled to the legs), the others keep `bind` (their part's bone length) plus the clip's
/// change from rest. `group` indexes `Fade::secs`; `from` is the pose the running crossfade
/// started at, taken when `generation` falls behind the motion's.
#[derive(Component)]
pub struct Driven {
    pub bone: usize,
    pub absolute: bool,
    pub bind: Vec3,
    group: usize,
    from: Transform,
    generation: u32,
}

/// Crossfade from the pose shown at the last clip change into the new clip: per bone group
/// (GeneralFrame, then `HOKAN_GROUPS`), seconds to reach it; linear in time.
#[derive(Clone, Copy, Default)]
pub struct Fade {
    pub generation: u32,
    pub secs: [f32; 8],
    pub t: f32,
}

impl Fade {
    /// The crossfade times of one `ACANIM_HOKANPARAM_ST` row (Booster_Frame drives the booster's
    /// own clips, which are not played yet).
    pub fn of(row: &AcanimHokanparamSt) -> [f32; 8] {
        [row.general_frame, row.center_frame, row.c_center_frame, row.k_center_frame, row.core_frame, row.kosi_frame, row.momo_frame, row.sune_frame]
            .map(|f| f32::from(f.max(0)) / HOKAN_FRAME_RATE)
    }
}

/// The hokan column of clip bone `bone`: its nearest ancestor-or-self named in `HOKAN_GROUPS`.
fn hokan_group(rest: &[ani::Rest], bone: usize) -> usize {
    let mut at = Some(bone);
    while let Some(i) = at {
        let name = rest[i].name.as_str();
        let side = name.strip_prefix("l_").or_else(|| name.strip_prefix("r_"));
        let hit = HOKAN_GROUPS.iter().position(|g| name == *g || (matches!(*g, "momo" | "sune") && side.is_some_and(|s| s.starts_with(g))));
        if let Some(g) = hit {
            return g + 1;
        }
        at = rest[i].parent.map(usize::from);
    }
    0
}

/// The motion set playing on the shown AC.
#[derive(Resource)]
pub struct Motion {
    pub set: &'static str,
    pub skeleton: ani::Anim,
    /// Clip name prefix of the set (`a01`, or `a00` for tank and quad sets), from its skeleton clip.
    pub prefix: &'static str,
    pub clips: Vec<&'static Asset>,
    pub index: usize,
    pub clip: ani::Anim,
    pub frame: f32,
    pub playing: bool,
    /// Non-looping clips hold their last frame.
    pub looping: bool,
    /// Playback rate relative to `FRAME_RATE`.
    pub speed: f32,
    /// Direction-wheel clips (dash / air-move lean, 360 frames, a key every 45 = one of the eight
    /// directions, frame = heading in degrees clockwise from forward): the heading to pose, which
    /// replaces time playback. Cleared by select.
    pub wheel: Option<f32>,
    /// Wheel clips: 0..1 weight of the lean pose over the pose shown at the clip change (set from
    /// the AC's speed, so the lean builds up as it accelerates) instead of the timed crossfade.
    pub lean: f32,
    /// Hold the clip's root bone (`master`) at rest: its motion (the turn clips yaw it) is
    /// applied by whoever moves the AC instead.
    pub in_place: bool,
    /// Leg bone length over the clip body's, applied to absolute translations.
    pub scale: f32,
    /// Crossfade into the current clip; clip changes through `select` cut instead.
    pub fade: Fade,
    cache: HashMap<usize, ani::Anim>,
}

impl Motion {
    pub fn name(&self) -> &'static str {
        self.clips.get(self.index).map_or("", |a| a.entry)
    }

    /// Loads clip `index`; clips whose bone count differs from the skeleton's are refused.
    pub fn select(&mut self, usrdir: &Path, index: usize) -> Result<()> {
        let clip = match self.cache.remove(&index) {
            Some(clip) => clip,
            None => {
                let asset = self.clips.get(index).context("no such clip")?;
                let clip = ani::read(&vfs::open(usrdir, &asset.path())?)?;
                anyhow::ensure!(clip.bones.len() == self.skeleton.bones.len(), "{} has {} bones, the skeleton {}", asset.entry, clip.bones.len(), self.skeleton.bones.len());
                clip
            }
        };
        let old = std::mem::replace(&mut self.clip, clip);
        self.cache.insert(self.index, old);
        (self.index, self.frame, self.looping, self.speed, self.wheel) = (index, 0.0, true, 1.0, None);
        self.fade = Fade { generation: self.fade.generation.wrapping_add(1), secs: [0.0; 8], t: 0.0 };
        Ok(())
    }

    /// Plays clip `anim_id` of the set (`<prefix>_<anim_id>.ani`) from its first frame, unless
    /// it is already playing; a new clip crossfades in over `fade` seconds per bone group.
    pub fn play(&mut self, usrdir: &Path, anim_id: u16, looping: bool, speed: f32, fade: [f32; 8]) -> Result<()> {
        let entry = format!("{}_{anim_id:03}.ani", self.prefix);
        let index = self.clips.iter().position(|a| a.entry == entry).with_context(|| format!("{} has no {entry}", self.set))?;
        if index != self.index {
            self.select(usrdir, index)?;
            self.fade.secs = fade;
        }
        (self.looping, self.speed, self.playing) = (looping, speed, true);
        Ok(())
    }

    /// Whether a non-looping clip has reached its last frame.
    pub fn finished(&self) -> bool {
        !self.looping && self.frame >= self.clip.frames.saturating_sub(1) as f32
    }
}

/// A part's loaded meshes and skeleton; `index` is its placement's index in the assembly.
pub struct Loaded<'a> {
    pub index: usize,
    pub placement: &'a Placement,
    pub meshes: Vec<acvd_render::LoadedMesh>,
    pub rig: Rig,
}

struct Spec {
    parent: Option<usize>,
    bind: Affine3A,
    bone: Option<usize>,
    virtual_bone: bool,
}

/// The rig to spawn: joints in parent-first order and, per part, the joints its meshes skin to.
pub struct Built {
    joints: Vec<Spec>,
    /// Per loaded part: its first joint (bone 0) and the extra unboned joint's bind.
    parts: Vec<(usize, Affine3A)>,
    /// Hokan bone group per clip bone.
    groups: Vec<usize>,
    pub motion: Option<Motion>,
}

pub fn affine(x: &flver::Xform) -> Affine3A {
    let m = &x.m;
    Affine3A::from_mat3_translation(Mat3::from_cols(Vec3::new(m[0][0], m[1][0], m[2][0]), Vec3::new(m[0][1], m[1][1], m[2][1]), Vec3::new(m[0][2], m[1][2], m[2][2])), Vec3::from(x.t))
}

fn mirror(a: Affine3A) -> Affine3A {
    let m = Affine3A::from_scale(Vec3::new(-1.0, 1.0, 1.0));
    m * a * m
}

fn to_transform(t: Vec3, r: Quat, s: Vec3) -> Transform {
    Transform { translation: Vec3::new(-t.x, t.y, t.z), rotation: Quat::from_xyzw(r.x, -r.y, -r.z, r.w), scale: s }
}

/// Opens the motion set the placements select (the first that has one) and its skeleton clip.
fn motion(usrdir: &Path, parts: &[Loaded]) -> Result<Option<Motion>> {
    let Some(m) = parts.iter().find_map(|p| acvd_data::ac_motion(p.placement.category, p.placement.part)) else { return Ok(None) };
    let skeleton = ani::read(&vfs::open(usrdir, &format!("{}|{}", m.set, m.skeleton))?).with_context(|| format!("{}|{}", m.set, m.skeleton))?;
    let mut clips: Vec<&'static Asset> = acvd_data::generated::assets::ALL.iter().flat_map(|g| g.iter()).filter(|a| a.binder == m.set && a.ext == ".ani").collect();
    clips.sort_by_key(|a| a.entry);
    let index = clips.iter().position(|a| a.entry == m.skeleton).unwrap_or(0);
    let clip = skeleton.clone();
    let prefix = m.skeleton.split('_').next().unwrap_or_default();
    Ok(Some(Motion { set: m.set, skeleton, prefix, clips, index, clip, frame: 0.0, playing: true, looping: true, speed: 1.0, wheel: None, lean: 0.0, in_place: false, scale: 1.0, fade: Fade::default(), cache: HashMap::new() }))
}

pub fn build(usrdir: &Path, parts: &[Loaded]) -> Result<Built> {
    let mut motion = motion(usrdir, parts)?;
    let rest: Vec<ani::Rest> = motion.as_ref().map(|m| m.skeleton.bones.iter().filter_map(|b| b.rest.clone()).collect()).unwrap_or_default();

    // Part bones first, one joint each.
    let mut joints: Vec<Spec> = Vec::new();
    let mut first = Vec::new();
    for p in parts {
        first.push(joints.len());
        joints.extend(p.rig.bones.iter().map(|b| Spec { parent: None, bind: affine(&b.bind), bone: None, virtual_bone: false }));
    }
    let name = |j: usize| -> &str {
        let pi = first.iter().rposition(|&f| f <= j).unwrap_or(0);
        &parts[pi].rig.bones[j - first[pi]].name
    };

    // Each clip bone claims the first part bone of its name, or becomes a joint of its own.
    let part_joints = joints.len();
    let mut of_bone: Vec<usize> = Vec::with_capacity(rest.len());
    for (i, r) in rest.iter().enumerate() {
        match (0..part_joints).find(|&j| joints[j].bone.is_none() && name(j) == r.name) {
            Some(j) => {
                joints[j].bone = Some(i);
                of_bone.push(j);
            }
            None => {
                of_bone.push(joints.len());
                joints.push(Spec { parent: None, bind: Affine3A::IDENTITY, bone: Some(i), virtual_bone: true });
            }
        }
    }

    // Parents: FLVER parents inside a part; clip parents for clip bones; socket carriers for
    // the remaining part roots.
    for (pi, p) in parts.iter().enumerate() {
        for (b, bone) in p.rig.bones.iter().enumerate() {
            let j = first[pi] + b;
            joints[j].parent = match bone.parent {
                Some(parent) => Some(first[pi] + parent),
                None => match joints[j].bone.and_then(|i| rest[i].parent) {
                    Some(cp) => of_bone.get(cp as usize).copied(),
                    None => carrier(parts, &first, p.placement, bone),
                },
            };
        }
    }
    for (i, r) in rest.iter().enumerate() {
        let j = of_bone[i];
        if joints[j].virtual_bone {
            joints[j].parent = r.parent.and_then(|cp| of_bone.get(cp as usize).copied());
        }
    }

    // Leg scale: the legs' bone lengths over the clip body's, below the legs' clip-driven root.
    if let Some(m) = motion.as_mut() {
        let (mut ours, mut theirs) = (0.0, 0.0);
        if let Some(pi) = parts.iter().position(|p| acvd_data::ac_motion(p.placement.category, p.placement.part).is_some()) {
            for b in 0..parts[pi].rig.bones.len() {
                let j = first[pi] + b;
                let (Some(i), Some(parent)) = (joints[j].bone, parts[pi].rig.bones[b].parent) else { continue };
                ours += (joints[first[pi] + parent].bind.inverse() * joints[j].bind).translation.length();
                theirs += Vec3::from(rest[i].translation).length();
            }
        }
        m.scale = if theirs > 0.0 { ours / theirs } else { 1.0 };
    }

    // Clip-only joints rest where the clip's rest pose puts them.
    let scale = motion.as_ref().map_or(1.0, |m| m.scale);
    for (i, r) in rest.iter().enumerate() {
        let j = of_bone[i];
        if joints[j].virtual_bone {
            let parent = joints[j].parent.map_or(Affine3A::IDENTITY, |p| joints[p].bind);
            let local = Affine3A::from_scale_rotation_translation(Vec3::from(r.scale), Quat::from_array(ani::euler_quat(r.euler)), Vec3::from(r.translation) * scale);
            joints[j].bind = parent * local;
        }
    }

    let parts_out = parts.iter().zip(&first).map(|(p, &f)| (f, Affine3A::from_translation(Vec3::from(p.rig.unboned)))).collect();
    let groups = (0..rest.len()).map(|i| hokan_group(&rest, i)).collect();
    Ok(Built { joints, parts: parts_out, groups, motion })
}

/// The joint carrying the socket `bone` (a part root) attaches to, if its slot has a parent.
fn carrier(parts: &[Loaded], first: &[usize], placement: &Placement, bone: &RigBone) -> Option<usize> {
    let &(_, parent, socket) = placement.attach.iter().find(|(root, _, _)| *root == bone.name)?;
    let p = parts.iter().position(|l| l.index == parent)?;
    let rig = &parts[p].rig;
    let b = match socket {
        SocketRef::Dummy(id) => rig.sockets.iter().find(|(s, _)| *s == id)?.1?,
        SocketRef::Bone(name) => rig.bones.iter().position(|x| x.name == name)?,
        SocketRef::None => return None,
    };
    Some(first[p] + b)
}

/// Spawns the joints under `ac` and returns, per loaded part, its skin: joint entities in bone
/// order plus the unboned joint, and their inverse bind poses (Bevy axes).
pub fn spawn(commands: &mut Commands, ac: Entity, built: &Built, parts: &[Loaded]) -> Vec<(Vec<Entity>, Vec<Mat4>)> {
    let entities: Vec<Entity> = built.joints.iter().map(|_| commands.spawn((Transform::default(), Visibility::default())).id()).collect();
    for (j, spec) in built.joints.iter().enumerate() {
        let parent_bind = spec.parent.map_or(Affine3A::IDENTITY, |p| built.joints[p].bind);
        let local = parent_bind.inverse() * spec.bind;
        let (s, r, t) = local.to_scale_rotation_translation();
        let mut e = commands.entity(entities[j]);
        let transform = to_transform(t, r, s);
        e.insert((transform, ChildOf(spec.parent.map_or(ac, |p| entities[p]))));
        if let Some(bone) = spec.bone {
            let absolute = spec.parent.is_none_or(|p| built.joints[p].virtual_bone);
            e.insert(Driven { bone, absolute, bind: t, group: built.groups.get(bone).copied().unwrap_or(0), from: transform, generation: 0 });
        }
    }
    parts
        .iter()
        .zip(&built.parts)
        .map(|(p, &(f, unboned))| {
            let n = p.rig.bones.len();
            let extra = commands.spawn((to_transform(unboned.translation.into(), Quat::IDENTITY, Vec3::ONE), Visibility::default(), ChildOf(ac))).id();
            let joints = entities[f..f + n].iter().copied().chain([extra]).collect();
            let binds = built.joints[f..f + n].iter().map(|s| s.bind).chain([unboned]).map(|b| Mat4::from(mirror(b).inverse())).collect();
            (joints, binds)
        })
        .collect()
}

/// Advances the clip and poses every driven joint, crossfading from the pose shown when the clip
/// changed.
pub fn animate(time: Res<Time>, motion: Option<ResMut<Motion>>, mut joints: Query<(&mut Driven, &mut Transform)>) {
    let Some(mut m) = motion else { return };
    let frames = m.clip.frames.max(1) as f32;
    if let Some(target) = m.wheel {
        let diff = (target - m.frame + 540.0).rem_euclid(360.0) - 180.0;
        let step = WHEEL_RATE * time.delta_secs();
        m.frame = (m.frame + diff.clamp(-step, step)).rem_euclid(360.0);
    } else if m.playing {
        let next = m.frame + time.delta_secs() * FRAME_RATE * m.speed;
        m.frame = if m.looping { next % frames } else { next.min(frames - 1.0) };
    }
    m.fade.t += time.delta_secs();
    let (f, scale, fade) = (m.frame, m.scale, m.fade);
    let (wheel, lean) = (m.wheel.is_some(), m.lean);
    for (mut d, mut transform) in &mut joints {
        if d.generation != fade.generation {
            (d.from, d.generation) = (*transform, fade.generation);
        }
        let (Some(rest), Some(bone)) = (m.skeleton.bones.get(d.bone).and_then(|b| b.rest.as_ref()), m.clip.bones.get(d.bone)) else { continue };
        let bone = if m.in_place && rest.parent.is_none() { &m.skeleton.bones[d.bone] } else { bone };
        let rotation = Quat::from_array(bone.track.rotation(f).unwrap_or_else(|| ani::euler_quat(rest.euler)));
        let t = Vec3::from(bone.track.translation(f).unwrap_or(rest.translation));
        let s = Vec3::from(bone.track.scale(f).unwrap_or(rest.scale));
        let translation = if d.absolute { t * scale } else { d.bind + (t - Vec3::from(rest.translation)) * scale };
        let target = to_transform(translation, rotation, s);
        let secs = fade.secs[d.group];
        let w = if wheel { lean } else if secs > 0.0 { (fade.t / secs).min(1.0) } else { 1.0 };
        *transform = if w >= 1.0 {
            target
        } else {
            Transform {
                translation: d.from.translation.lerp(target.translation, w),
                rotation: d.from.rotation.slerp(target.rotation, w),
                scale: d.from.scale.lerp(target.scale, w),
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hokan_groups_follow_the_nearest_named_ancestor() {
        let names = [("master", None), ("center", Some(0)), ("c_center", Some(1)), ("core", Some(2)), ("l_kata", Some(3)), ("k_center", Some(1)), ("kosi", Some(5)), ("l_momo_01", Some(6)), ("l_hiza", Some(7)), ("r_sune", Some(8)), ("r_asik_01", Some(9))];
        let rest: Vec<ani::Rest> = names
            .iter()
            .map(|&(name, parent)| ani::Rest { name: name.into(), kind: 0, parent, translation: [0.0; 3], euler: [0.0; 3], scale: [1.0; 3] })
            .collect();
        let groups: Vec<usize> = (0..rest.len()).map(|i| hokan_group(&rest, i)).collect();
        assert_eq!(groups, [0, 1, 2, 4, 4, 3, 5, 6, 6, 7, 7]);
    }

    #[test]
    fn fade_converts_hokan_frames_to_seconds() {
        let walk = find_hokan(2);
        assert_eq!(Fade::of(&walk)[0], 0.06);
        assert_eq!(Fade::of(&walk)[3], 0.3);
    }

    fn find_hokan(id: u32) -> AcanimHokanparamSt {
        acvd_data::find(acvd_data::generated::ac_unit::PARAM_ACANIMHOKAN_BIN, id).expect("hokan row").data
    }
}
