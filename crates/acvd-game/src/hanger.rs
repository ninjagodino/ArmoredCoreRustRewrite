//! Hanger racks (`hgr0001` / `hgl0001`, `sheets/hanger_anim.csv`).
//!
//! The FLVER bind leaves `r_hg_f`/`l_hg_f` (dummy 87/85) and `r_hg_b`/`l_hg_b` (dummy 88/86)
//! unrotated. The rack animation bank drives all 6 bones (`81`/`80`, `*_hg01`, `*_hg02`,
//! `*_hg_f`, `*_hg03`, `*_hg_b`) directly via the dual-prong carousel clips:
//! - `a00_011.ani` (phase `0 -> 1`, 180 frames): starts with the stowed weapon on `*_hg_b` and
//!   `*_hg_f` empty; `f0..f40` opens `*_hg_f` forward to match `r_armhand`/`l_armhand` at frame
//!   40; `f40..f120` spins the carousel 180°; `f120..f150` presents `*_hg_b` aligned with the
//!   hand for the frame-150 grab; `f150..f180` folds the now-empty `*_hg_b` back.
//! - `a00_021.ani` (phase `1 -> 0`, 180 frames): exact symmetric return from `a00_011` frame 179
//!   back to `a00_011` frame 0.

use acvd_formats::ani;
use acvd_formats::vfs::{self, Disc};
use bevy::prelude::*;

use crate::weapons::{Hand, Shift};

const RIGHT: &str = "model/ac/parts/hanger/hgr0001/hgr0001_a.bnd.dcx";
const LEFT: &str = "model/ac/parts/hanger/hgl0001/hgl0001_a.bnd.dcx";

struct Rack {
    hand: Hand,
    /// Current racked weapon's `hanger_pose` (`0..=4`).
    #[allow(dead_code)]
    pose: u8,
    /// Phase `0 -> 1` carousel clip (`a00_011.ani`, 180 frames).
    swing_01: ani::Anim,
    /// Phase `1 -> 0` carousel clip (`a00_021.ani`, 180 frames).
    swing_10: ani::Anim,
    /// Bone order of `a00_000`.
    joints: Vec<Option<Entity>>,
    /// FLVER local translation, kept while the clip replaces the rotation.
    translation: Vec<Option<Vec3>>,
    /// Initial mount rotation of root bone 0 (`81` / `80`) on the arm socket, preserved so
    /// the rack stays aligned with the arm shoulder.
    root_rotation: Option<Quat>,
}

#[derive(Resource, Default)]
pub struct HangerPose {
    racks: Vec<Rack>,
}

impl HangerPose {
    pub fn load(disc: &Disc, racks: Vec<(Hand, u8, Vec<(String, Entity)>)>) -> Self {
        let mut out = Vec::new();
        for (hand, pose, bones) in racks {
            let path = if hand == Hand::Right { RIGHT } else { LEFT };
            let Ok(skeleton) = open(disc, path, "a00_000.ani") else {
                continue;
            };
            let n_bones = skeleton.bones.len();
            let Ok(swing_01) = open(disc, path, "a00_011.ani") else {
                continue;
            };
            let Ok(swing_10) = open(disc, path, "a00_021.ani") else {
                continue;
            };
            if swing_01.bones.len() != n_bones || swing_10.bones.len() != n_bones {
                continue;
            }
            let joints = skeleton
                .bones
                .iter()
                .map(|b| {
                    let name = b.rest.as_ref().map(|r| r.name.as_str())?;
                    bones.iter().find(|(n, _)| n == name).map(|(_, e)| *e)
                })
                .collect();
            out.push(Rack {
                hand,
                pose: pose.min(4),
                swing_01,
                swing_10,
                joints,
                translation: Vec::new(),
                root_rotation: None,
            });
        }
        Self { racks: out }
    }

    /// Updates the racked weapon's `hanger_pose` (`0..=4`) after a bay shift seats a new weapon.
    pub fn set_pose(&mut self, hand: Hand, pose: u8) {
        if let Some(rack) = self.racks.iter_mut().find(|r| r.hand == hand) {
            rack.pose = pose.min(4);
        }
    }
}

fn open(disc: &Disc, binder: &str, entry: &str) -> anyhow::Result<ani::Anim> {
    ani::read(&vfs::open(disc, &format!("{binder}|{entry}"))?)
}

/// Mirror a clip quaternion into Bevy's right-handed frame (X flip), as `pose::to_transform`.
fn bevy_quat(q: [f32; 4]) -> Quat {
    Quat::from_xyzw(q[0], -q[1], -q[2], q[3])
}

/// Idle stowed shoulder pitch on `*_hg01` (bone 1): `a00_011`/`a00_021` f=0 starts with the
/// stowed prong at ~23° down-back (`r_hg_b` -151° X vs `a00_000` rest -121° X). Adding +30°
/// around parent X settles the stowed weapon at ~53° down-back behind the shoulder (matching
/// `a00_000` and retail 360 Xenia captures symmetrically on both left and right racks), and
/// blends smoothly to 0° during `f=20..150` so `f=40` (seat) and `f=150` (grab) align with
/// `*_armhand`.
const STOW_PITCH_X: f32 = 30.0_f32.to_radians();

pub fn pose(
    shift: Option<Res<Shift>>,
    mut hanger: Option<ResMut<HangerPose>>,
    mut joints: Query<&mut Transform>,
) {
    let Some(hanger) = hanger.as_mut() else {
        return;
    };
    for rack in &mut hanger.racks {
        let side = rack.hand as usize;
        let (carousel_clip, carousel_frame, shift_blend) = match shift.as_deref() {
            Some(s) => match s.hands[side].as_ref() {
                Some(hs) if !hs.purge => {
                    let clip = if hs.phase == 0 {
                        &rack.swing_01
                    } else {
                        &rack.swing_10
                    };
                    let max_f = clip.frames.saturating_sub(1) as f32;
                    let f = hs.rack_frame.clamp(0.0, max_f);
                    let w = if f < 20.0 {
                        f / 20.0
                    } else if f <= 150.0 {
                        1.0
                    } else {
                        ((180.0 - f) / 30.0).clamp(0.0, 1.0)
                    };
                    (clip, f, w)
                }
                _ => {
                    if s.phase[side] == 0 {
                        (&rack.swing_01, 0.0, 0.0)
                    } else {
                        (
                            &rack.swing_01,
                            rack.swing_01.frames.saturating_sub(1) as f32,
                            0.0,
                        )
                    }
                }
            },
            None => (&rack.swing_01, 0.0, 0.0),
        };

        let quats: Vec<Quat> = (0..carousel_clip.bones.len())
            .map(|i| {
                let q = bevy_quat(
                    carousel_clip
                        .bones
                        .get(i)
                        .and_then(|b| b.track.rotation(carousel_frame))
                        .unwrap_or([0.0, 0.0, 0.0, 1.0]),
                );
                if i == 1 {
                    let stow_q = Quat::from_rotation_x(STOW_PITCH_X * (1.0 - shift_blend));
                    return stow_q * q;
                }
                q
            })
            .collect();
        if rack.translation.len() != rack.joints.len() {
            rack.translation.resize(rack.joints.len(), None);
        }
        for (i, joint) in rack.joints.iter().enumerate() {
            let Some(entity) = *joint else { continue };
            let Ok(mut transform) = joints.get_mut(entity) else {
                continue;
            };
            if i >= quats.len() {
                continue;
            }
            let saved = *rack.translation[i].get_or_insert(transform.translation);
            transform.translation = saved;
            if i == 0 {
                let base_r = *rack.root_rotation.get_or_insert(transform.rotation);
                transform.rotation = (base_r * quats[0]).normalize();
            } else {
                transform.rotation = quats[i];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acvd_formats::{ani, vfs};

    /// Evaluate local-to-world matrix of a bone chain from an `.ani` clip at frame `f`
    /// using the exact `pose::to_transform` / `hanger::bevy_quat` convention.
    fn eval_ani_world(
        skeleton: &ani::Anim,
        clip: &ani::Anim,
        f: f32,
        bone_name: &str,
    ) -> (Vec3, Quat) {
        let idx = skeleton
            .bones
            .iter()
            .position(|b| b.rest.as_ref().is_some_and(|r| r.name == bone_name))
            .unwrap_or_else(|| panic!("no bone {bone_name}"));
        let mut chain = Vec::new();
        let mut cur = idx;
        loop {
            chain.push(cur);
            let Some(p) = skeleton.bones[cur].rest.as_ref().and_then(|r| r.parent) else {
                break;
            };
            if p as usize == cur {
                break;
            }
            cur = p as usize;
        }
        chain.reverse();
        let mut pos = Vec3::ZERO;
        let mut rot = Quat::IDENTITY;
        for b in chain {
            let rest = skeleton.bones[b].rest.as_ref().unwrap();
            let raw_t = clip
                .bones
                .get(b)
                .and_then(|cb| cb.track.translation(f))
                .unwrap_or(rest.translation);
            let raw_r = clip
                .bones
                .get(b)
                .and_then(|cb| cb.track.rotation(f))
                .unwrap_or_else(|| ani::euler_quat(rest.euler));
            let t = Vec3::new(-raw_t[0], raw_t[1], raw_t[2]);
            let r = bevy_quat(raw_r);
            pos += rot * t;
            rot = (rot * r).normalize();
        }
        (pos, rot)
    }

    #[test]
    fn carousel_clips_alternate_prongs_continuously() {
        let Ok(disc) = vfs::Disc::default_for(std::path::Path::new("../..")) else {
            return;
        };
        let c01 = open(&disc, RIGHT, "a00_011.ani").unwrap();
        let c10 = open(&disc, RIGHT, "a00_021.ani").unwrap();
        assert_eq!(c01.frames, 180);
        assert_eq!(c10.frames, 180);
        // End of a00_011 (phase 1 idle) must match start of a00_021 seamlessly on all 6 bones.
        for bi in 0..6 {
            let end_01 = Quat::from_array(c01.bones[bi].track.rotation(179.0).unwrap());
            let start_10 = Quat::from_array(c10.bones[bi].track.rotation(0.0).unwrap());
            assert!(
                end_01.angle_between(start_10).to_degrees() < 1.0,
                "bone {bi} discontinuous between a00_011 end and a00_021 start"
            );
            let end_10 = Quat::from_array(c10.bones[bi].track.rotation(179.0).unwrap());
            let start_01 = Quat::from_array(c01.bones[bi].track.rotation(0.0).unwrap());
            assert!(
                end_10.angle_between(start_01).to_degrees() < 1.0,
                "bone {bi} discontinuous between a00_021 end and a00_011 start"
            );
        }
        // Verify pure a00_011 aligns r_hg_f +Z/+Y with r_armhand +Z/+Y at f=40 (seat)
        // and r_hg_b +Z/+Y with r_armhand +Z/+Y at f=150 (grab).
        let design = acvd_data::generated::ac_unit::PARAM_ACASSEMBLYDRAWING_BIN
            .iter()
            .find(|r| r.id == 5001)
            .unwrap();
        let m = acvd_data::ac_motion(3, design.data.legs as u16).unwrap();
        let skel = open(&disc, m.set, m.skeleton).unwrap();
        let prefix = m.skeleton.split('_').next().unwrap_or("a00");
        let shift_r = open(&disc, m.set, &format!("{prefix}_301.ani")).unwrap();

        for (f, is_front) in [(40.0, true), (150.0, false)] {
            let (_, hand_r) = eval_ani_world(&skel, &shift_r, f, "r_armhand");
            let (_, sh_r) = eval_ani_world(&skel, &shift_r, f, "r_kata");
            let mut q = [Quat::IDENTITY; 6];
            for i in 0..6 {
                q[i] = bevy_quat(c01.bones[i].track.rotation(f).unwrap_or([0.0, 0.0, 0.0, 1.0]));
            }
            let prong_r = if is_front {
                sh_r * q[0] * q[1] * q[2] * q[3]
            } else {
                sh_r * q[0] * q[1] * q[4] * q[5]
            };
            let barrel_dot = (hand_r * Vec3::Z).dot(prong_r * Vec3::Z);
            assert!(
                barrel_dot > 0.9,
                "f={f}: prong +Z should match r_armhand +Z (dot={barrel_dot:.3})"
            );
        }
    }

    #[test]
    fn rest_pose_stows_barrel_down_back_matching_360() {
        let Ok(disc) = vfs::Disc::default_for(std::path::Path::new("../..")) else {
            return;
        };
        let design = acvd_data::generated::ac_unit::PARAM_ACASSEMBLYDRAWING_BIN
            .iter()
            .find(|r| r.id == 5001)
            .unwrap();
        let built = crate::assemble::assemble(
            acvd_data::generated::assembly::AC_ASSEMBLY_DESIGN_ST_SLOTS,
            &design.data,
        );
        let mut flvers = acvd_render::FlverCache::default();
        let mut loaded = Vec::new();
        let mut known = vec![None; built.placements.len()];
        for (index, p) in built.placements.iter().enumerate() {
            if !matches!(p.column, "legs" | "core" | "arms" | "rack_r" | "rack_l") {
                continue;
            }
            let places = crate::assemble::root_places(p, &built.placements, &known);
            let (meshes, rig) = flvers
                .rigged_model(&disc, p.model.path, &|root| {
                    crate::assemble::place_of(&places, root)
                })
                .unwrap();
            known[index] = Some(crate::assemble::Stored {
                places,
                frames: rig.frames.clone(),
            });
            loaded.push(crate::pose::Loaded {
                index,
                placement: p,
                meshes,
                rig,
            });
        }
        let rig = crate::pose::build(&disc, &loaded).unwrap();
        for (pi, p) in loaded.iter().enumerate() {
            if p.placement.column == "rack_r" || p.placement.column == "rack_l" {
                let (_, tf) = rig.mount_local(pi, 0).unwrap();
                let (ex, ey, ez) = tf.rotation.to_euler(EulerRot::XYZ);
                eprintln!(
                    "  {} mount_local b0: t=({:+.2},{:+.2},{:+.2}) euler=({:+6.1},{:+6.1},{:+6.1})",
                    p.placement.column,
                    tf.translation.x, tf.translation.y, tf.translation.z,
                    ex.to_degrees(), ey.to_degrees(), ez.to_degrees()
                );
            }
        }
        let m = acvd_data::ac_motion(3, design.data.legs as u16).unwrap();
        let ac_skel = open(&disc, m.set, m.skeleton).unwrap();
        for (side, col, path, kata) in [
            ("R", "rack_r", RIGHT, "r_kata"),
            ("L", "rack_l", LEFT, "l_kata"),
        ] {
            let pi = loaded.iter().position(|p| p.placement.column == col).unwrap();
            let (_, tf0) = rig.mount_local(pi, 0).unwrap();
            let (_, sh_r) = eval_ani_world(&ac_skel, &ac_skel, 0.0, kata);
            let c01 = open(&disc, path, "a00_011.ani").unwrap();
            let stow_q = Quat::from_rotation_x(STOW_PITCH_X);
            let q0: [Quat; 6] = std::array::from_fn(|i| {
                bevy_quat(c01.bones[i].track.rotation(0.0).unwrap_or([0.0, 0.0, 0.0, 1.0]))
            });
            let end_f = c01.frames.saturating_sub(1) as f32;
            let q1: [Quat; 6] = std::array::from_fn(|i| {
                bevy_quat(c01.bones[i].track.rotation(end_f).unwrap_or([0.0, 0.0, 0.0, 1.0]))
            });
            let r0 = sh_r * tf0.rotation * q0[0] * (stow_q * q0[1]) * q0[4] * q0[5];
            let r1 = sh_r * tf0.rotation * q1[0] * (stow_q * q1[1]) * q1[2] * q1[3];
            let b0 = r0 * (-Vec3::Z); // weapon FLVER muzzle is -Z
            let b1 = r1 * (-Vec3::Z);
            let u0 = r0 * Vec3::Y;
            eprintln!(
                "  {side} stowed phase0: muzzle(-Z)=({:+.2},{:+.2},{:+.2}) top(+Y)=({:+.2},{:+.2},{:+.2})",
                b0.x, b0.y, b0.z, u0.x, u0.y, u0.z
            );
            eprintln!("  {side} stowed phase1: muzzle(-Z)=({:+.2},{:+.2},{:+.2})", b1.x, b1.y, b1.z);
            assert!(b0.y < -0.65 && b0.z > 0.45, "{side}: muzzle must point ~53 deg down-back (got {b0:?})");
            assert!(b0.dot(b1) > 0.99, "{side}: phase 0 and phase 1 stowed angles should match");
        }
    }
}
