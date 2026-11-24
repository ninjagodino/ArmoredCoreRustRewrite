//! Hanger racks (`hgr0001` / `hgl0001`, `sheets/hanger_anim.csv`).
//!
//! The FLVER bind leaves `r_hg_f` / `l_hg_f` and `r_hg_b` / `l_hg_b` unrotated, so a weapon on
//! dummy 87/85 stands straight up. The rack bank poses those bones:
//! - Bones `0..=1` (`81`/`80`, `r_hg01`/`l_hg01`): stowed pitch from the racked weapon's
//!   `hanger_pose` (`sheets/ac_part_fields.csv`, arrive clip `a00_(30 + 6*pose)` held on its
//!   keyed frame + `STOW_CCW`).
//! - Bones `2..` (`*_hg02`, `*_hg_f`, `*_hg03`, `*_hg_b`): dual-prong carousel driven by
//!   `a00_011.ani` (phase `0 -> 1`, 180 frames) and `a00_021.ani` (phase `1 -> 0`, 180 frames),
//!   triggered by TAE `2000..2003` (`kind=1` part-anim at frame 0, 360 `0x82876f48` phase toggle).
//!   Frame `0..40` opens the empty prong to receive the hand weapon (seated at frame 40 via TAE
//!   `kind=101`), frame `40..90` rotates the carousel 180°, and frame `90..150` presents the
//!   previously stowed weapon for the hand to grab at frame 150.

use acvd_formats::ani;
use acvd_formats::vfs::{self, Disc};
use bevy::prelude::*;

use crate::weapons::{Hand, Shift};

const RIGHT: &str = "model/ac/parts/hanger/hgr0001/hgr0001_a.bnd.dcx";
const LEFT: &str = "model/ac/parts/hanger/hgl0001/hgl0001_a.bnd.dcx";

/// Keyed frame of each arrive clip `a00_(30 + 6*pose)` where the barrel sits ~49° above horizontal.
const STOW_FRAMES: [f32; 5] = [8.0, 8.0, 8.0, 10.0, 12.0];

struct Rack {
    hand: Hand,
    /// Current racked weapon's `hanger_pose` (`0..=4`).
    pose: u8,
    /// All five arrive clips `a00_(30 + 6*pose)` so swapping weapons updates the stow pitch.
    stowed: [Option<ani::Anim>; 5],
    /// Phase `0 -> 1` carousel clip (`a00_011.ani`, 180 frames).
    swing_01: ani::Anim,
    /// Phase `1 -> 0` carousel clip (`a00_021.ani`, 180 frames).
    swing_10: ani::Anim,
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
    /// `pose` is the initial bay weapon's `hanger_pose` byte (`sheets/ac_part_fields.csv`):
    /// `0..=4` selects arrive clip `a00_(30 + 6*pose)`.
    pub fn load(disc: &Disc, racks: Vec<(Hand, u8, Vec<(String, Entity)>)>) -> Self {
        let mut out = Vec::new();
        for (hand, pose, bones) in racks {
            let path = if hand == Hand::Right { RIGHT } else { LEFT };
            let Ok(skeleton) = open(disc, path, "a00_000.ani") else {
                continue;
            };
            let n_bones = skeleton.bones.len();
            let stowed: [Option<ani::Anim>; 5] = std::array::from_fn(|p| {
                let clip = 30 + (p as u32) * 6;
                open(disc, path, &format!("a00_{clip:03}.ani"))
                    .ok()
                    .filter(|a| a.bones.len() == n_bones)
            });
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
                stowed,
                swing_01,
                swing_10,
                joints,
                translation: Vec::new(),
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
        let side = rack.hand as usize;
        let pose_idx = usize::from(rack.pose.min(4));
        let Some(stowed_clip) = rack.stowed[pose_idx]
            .as_ref()
            .or_else(|| rack.stowed.iter().flatten().next())
        else {
            continue;
        };
        let stowed_end = STOW_FRAMES[pose_idx];

        // Determine the carousel clip and frame for bones 2.. (`*_hg02`, `*_hg_f`, `*_hg03`, `*_hg_b`).
        // During a non-purge shift, play `a00_011` (phase 0->1) or `a00_021` (phase 1->0) from
        // frame 0..180. When idle, hold phase 0 (`a00_011` frame 0) or phase 1 (`a00_011` end).
        let (carousel_clip, carousel_frame) = match shift.as_deref() {
            Some(s) => match s.hands[side].as_ref() {
                Some(hs) if !hs.purge => {
                    let clip = if hs.phase == 0 {
                        &rack.swing_01
                    } else {
                        &rack.swing_10
                    };
                    let max_f = clip.frames.saturating_sub(1) as f32;
                    (clip, hs.rack_frame.clamp(0.0, max_f))
                }
                _ => {
                    if s.phase[side] == 0 {
                        (&rack.swing_01, 0.0)
                    } else {
                        (&rack.swing_01, rack.swing_01.frames.saturating_sub(1) as f32)
                    }
                }
            },
            None => (&rack.swing_01, 0.0),
        };

        let quats: Vec<[f32; 4]> = (0..stowed_clip.bones.len())
            .map(|i| {
                let (clip, frame) = if i >= 2 {
                    (carousel_clip, carousel_frame)
                } else {
                    (stowed_clip, stowed_end)
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
