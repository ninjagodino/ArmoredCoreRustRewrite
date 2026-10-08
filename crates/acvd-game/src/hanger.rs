//! Hanger racks. The FLVER bind leaves `r_hg_f` / `l_hg_f` unrotated, so a weapon on dummy
//! 87/85 stands straight up. The rack bank poses those bones (`sheets/hanger_anim.csv`).
//!
//! The stowed pitch is the weapon's `hanger_pose` (`sheets/ac_part_fields.csv`): arrive clip
//! `a00_(30 + 6*pose)`, held on the keyed frame where the barrel is about 49° above horizontal.
//! `a00_011` spins the prong only after the hand weapon has been seated on the rack
//! (`Shift.swapped`). Until then the rack holds the stow.
//! The clip rotation replaces the FLVER local rotation. It is not a delta from the clip rest:
//! `a00_000`'s keys match its own rest, and that rest is not the FLVER bind. Unnamed clips
//! share `a00_000`'s bone order.

use acvd_formats::ani;
use acvd_formats::vfs::{self, Disc};
use bevy::prelude::*;

use crate::weapons::{Hand, Shift};

const RIGHT: &str = "model/ac/parts/hanger/hgr0001/hgr0001_a.bnd.dcx";
const LEFT: &str = "model/ac/parts/hanger/hgl0001/hgl0001_a.bnd.dcx";

struct Rack {
    hand: Hand,
    stowed: ani::Anim,
    swing: ani::Anim,
    /// Keyed frame of the arrive clip where the barrel is about 49° above horizontal.
    stow_frame: f32,
    /// Bone order of `a00_000`.
    joints: Vec<Option<Entity>>,
    /// FLVER local translation, kept while the clip replaces the rotation.
    translation: Vec<Option<Vec3>>,
}

#[derive(Resource, Default)]
pub struct HangerPose {
    racks: Vec<Rack>,
}

impl HangerPose {
    /// `pose` is the weapon's `hanger_pose` byte (`sheets/ac_part_fields.csv`): 0..4 selects
    /// arrive clip `a00_(30 + 6*pose)`.
    pub fn load(disc: &Disc, racks: Vec<(Hand, u8, Vec<(String, Entity)>)>) -> Self {
        let mut out = Vec::new();
        for (hand, pose, bones) in racks {
            let path = if hand == Hand::Right { RIGHT } else { LEFT };
            let Ok(skeleton) = open(disc, path, "a00_000.ani") else {
                continue;
            };
            let pose = pose.min(4);
            let clip = 30 + u32::from(pose) * 6;
            let Ok(stowed) = open(disc, path, &format!("a00_{clip:03}.ani")) else {
                continue;
            };
            // Each arrive clip lifts the barrel from horizontal through the garage rest
            // (~49° above horizontal) and on to a steeper or flatter settle. The photographed
            // rest is the keyed frame on the way up, not the final frame.
            let stow_frame = [8.0, 8.0, 8.0, 10.0, 12.0][usize::from(pose)];
            let Ok(swing) = open(disc, path, "a00_011.ani") else {
                continue;
            };
            if stowed.bones.len() != skeleton.bones.len()
                || swing.bones.len() != skeleton.bones.len()
            {
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
                stowed,
                swing,
                stow_frame,
                joints,
                translation: Vec::new(),
            });
        }
        Self { racks: out }
    }
}

fn open(disc: &Disc, binder: &str, entry: &str) -> anyhow::Result<ani::Anim> {
    ani::read(&vfs::open(disc, &format!("{binder}|{entry}"))?)
}

/// Mirror a clip quaternion into Bevy's right-handed frame (X flip), as `pose::to_transform`.
fn bevy_quat(q: [f32; 4]) -> Quat {
    Quat::from_xyzw(q[0], -q[1], -q[2], q[3])
}

/// Extra shoulder pitch past the arrive clip. In the side view (forward to the right) this is
/// counterclockwise: the muzzle swings from up, through back, to down.
const STOW_CCW: f32 = 100.0_f32.to_radians();

pub fn pose(
    shift: Option<Res<Shift>>,
    mut hanger: Option<ResMut<HangerPose>>,
    mut joints: Query<&mut Transform>,
) {
    let Some(hanger) = hanger.as_mut() else {
        return;
    };
    for rack in &mut hanger.racks {
        let stowed_end = rack.stow_frame;
        // The arm carries the equipped weapon up to the rack. The rack itself stays on the
        // stow until that weapon is parented on (`Shift.swapped`, frame 40). Then `a00_011`
        // turns the prong and comes back to its first frame, which is the stowed prong.
        // `present` goes 0→1 while the arm comes back up, then 1→0 after the grab.
        // Frame 90 of `a00_011` is the prong swung forward.
        let spin = shift
            .as_ref()
            .filter(|s| s.hand == rack.hand && s.swapped && !s.purge)
            .map(|s| s.present * 90.0);
        let quats: Vec<[f32; 4]> = (0..rack.stowed.bones.len())
            .map(|i| {
                let (clip, frame) = if i >= 2 {
                    if let Some(spin) = spin {
                        (&rack.swing, spin)
                    } else {
                        (&rack.stowed, stowed_end)
                    }
                } else {
                    (&rack.stowed, stowed_end)
                };
                clip.bones
                    .get(i)
                    .and_then(|b| b.track.rotation(frame))
                    .unwrap_or([0.0, 0.0, 0.0, 1.0])
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
            let mut rotation = bevy_quat(quats[i]);
            // Bone 1 is `r_hg01` / `l_hg01`, the shoulder of the rack. Parent X is the AC's
            // lateral axis, so this pitch is the same CCW on both sides.
            if i == 1 {
                rotation = Quat::from_rotation_x(STOW_CCW) * rotation;
            }
            transform.rotation = rotation;
        }
    }
}
