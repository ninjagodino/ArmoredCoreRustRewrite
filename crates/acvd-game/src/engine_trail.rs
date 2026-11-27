//! Flying-wing engine plumes.
//!
//! The blue-white multi-engine plume belongs to the flying-wing bomber `e6010` (the online
//! bomber model). Its enemy TAE `tae/enemy/e6010/own000100.tae` animation `0x64` (the
//! flight clip `100`) fires `KIND_FFX` events on the eight engine effect points:
//!
//! - `5320` on dummies 200/201/204/205 (left wing engines),
//! - `5321` on dummies 202/203/206/207 (right wing engines),
//! - `5322` on dummies 200/201/204/205 for the boost stage.
//!
//! The dummy ids are FLVER dummy `color[1]` (`acvd-render` `Rig::effects`), and the FFX ids
//! live in `sfx/acv_commoneffects.ffxbnd` (model `s1011`, textures `s1037`/`s1038`, the
//! same authored flame the enemy AC main booster uses).
//!
//! The map bombers `o9341`/`o9344` contain **no** `KIND_FFX` events in their
//! `tae/obj/<model>/own000100.tae`; their authored look is the static `Ene_Diffuse_Glow`
//! submesh (rendered with its additive emission), so no plume is invented for them.

use bevy::prelude::*;

use acvd_formats::tae;
use acvd_formats::vfs::{self, Disc};

use crate::{pose, sfx, targets::EnemyAnim};

/// The flying-wing bomber model whose TAE authors the engine plumes.
pub const BOMBER_MODEL: &str = "e6010";
/// TAE animation `0x64` is the flight clip `100` timeline in `tae/enemy/e6010`.
pub const BOMBER_TIMELINE: u32 = 0x64;
/// ANI clip the TAE timeline runs alongside (`a00_100.ani`, 2600 frames).
pub const BOMBER_CLIP: u16 = 100;
/// Frame count of `a00_100.ani`, used to loop the TAE timeline.
pub const BOMBER_CLIP_FRAMES: f32 = 2600.0;

/// One effect point of an enemy model, in FLVER axes. `joint` indexes the instance skeleton.
#[derive(Clone, Debug)]
pub struct EngineSocket {
    pub id: u8,
    pub joint: usize,
    pub position: [f32; 3],
    pub forward: [f32; 3],
}

/// TAE `KIND_FFX` timeline and the engine sockets it addresses.
#[derive(Component, Clone)]
pub struct EnginePlumeFx {
    pub timeline: tae::Animation,
    pub sockets: Vec<EngineSocket>,
    pub frame: f32,
    pub frames: f32,
    pub looping: bool,
}

impl EnginePlumeFx {
    /// Loads `tae/enemy/<model>/own000100.tae` animation `timeline_id` and maps every
    /// `Rig::effects` dummy to its joint index. Returns `None` when the model has no such
    /// timeline (the `o9341`/`o9344` map bombers are the known no-FFX cases).
    pub fn load(
        disc: &Disc,
        model: &str,
        rig: &acvd_render::Rig,
        timeline_id: u32,
        frames: f32,
    ) -> Option<Self> {
        let bytes = vfs::open(disc, &format!("tae/enemy/{model}/own000100.tae")).ok()?;
        let tae = tae::read(&bytes).ok()?;
        let timeline = tae.animations.into_iter().find(|a| a.id == timeline_id)?;
        if !timeline.events.iter().any(|e| e.kind == tae::KIND_FFX) {
            return None;
        }
        let sockets = rig
            .effects
            .iter()
            .filter_map(|e| {
                Some(EngineSocket {
                    id: e.id,
                    joint: e.bone?,
                    position: e.position,
                    forward: e.forward,
                })
            })
            .collect();
        Some(Self {
            timeline,
            sockets,
            frame: 0.0,
            frames: frames.max(1.0),
            looping: true,
        })
    }
}

/// Advances each engine TAE timeline and spawns every crossed `KIND_FFX` event on its
/// model dummy, parented to the dummy joint so the flame follows the wing.
pub fn step_plumes(
    time: Res<Time>,
    mut commands: Commands,
    mut plumes: Query<(&mut EnginePlumeFx, &EnemyAnim)>,
) {
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    for (mut fx, anim) in &mut plumes {
        let prev = fx.frame;
        let next = fx.frame + dt * pose::FRAME_RATE;
        fx.frame = if fx.looping {
            next % fx.frames
        } else {
            next.min(fx.frames - 1.0)
        };
        for fired in pose::crossed_events(&fx.timeline, prev, fx.frame, fx.frames, fx.looping) {
            let pose::TaeFired::Ffx {
                ffx_id, dummy_id, ..
            } = fired
            else {
                continue;
            };
            let Some(sock) = fx.sockets.iter().find(|s| s.id == dummy_id) else {
                continue;
            };
            let Some(&joint) = anim.joints.get(sock.joint) else {
                continue;
            };
            // acvd-render mirrors FLVER X into Bevy; the FFX model is authored +Z.
            let position = Vec3::new(-sock.position[0], sock.position[1], sock.position[2]);
            let forward =
                Vec3::new(-sock.forward[0], sock.forward[1], sock.forward[2]).normalize_or(Vec3::Z);
            commands.spawn((
                sfx::Sfx::new(ffx_id),
                Transform::from_translation(position)
                    .with_rotation(Quat::from_rotation_arc(Vec3::Z, forward)),
                Visibility::default(),
                ChildOf(joint),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disc() -> Option<Disc> {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        Disc::open(&vfs::default_disc(&root)).ok()
    }

    #[test]
    fn e6010_tae_authors_the_eight_engine_plumes() {
        let Some(disc) = disc() else {
            return;
        };
        let bytes =
            vfs::open(&disc, "tae/enemy/e6010/own000100.tae").expect("e6010 enemy TAE exists");
        let tae = tae::read(&bytes).unwrap();
        let anim = tae
            .animations
            .iter()
            .find(|a| a.id == BOMBER_TIMELINE)
            .expect("e6010 anim 0x64");
        let events: Vec<(i32, u8, u8)> = anim
            .events
            .iter()
            .filter_map(|e| {
                let (id, dummy) = e.as_ffx()?;
                Some((id, dummy, e.params.get(8).copied().unwrap_or(0)))
            })
            .collect();
        for (ffx, dummy) in [
            (5320, 200),
            (5320, 201),
            (5320, 204),
            (5320, 205),
            (5321, 202),
            (5321, 203),
            (5321, 206),
            (5321, 207),
        ] {
            assert!(
                events.iter().any(|&(id, d, _)| id == ffx && d == dummy),
                "e6010 TAE must fire ffx {ffx} on engine dummy {dummy}: {events:?}"
            );
        }
        for dummy in [200, 201, 204, 205] {
            assert!(
                events.iter().any(|&(id, d, _)| id == 5322 && d == dummy),
                "e6010 boost stage ffx 5322 on dummy {dummy}: {events:?}"
            );
        }
    }

    #[test]
    fn e6010_model_has_the_eight_engine_effect_points() {
        let Some(disc) = disc() else {
            return;
        };
        let (_, rig) =
            acvd_render::rigged_model(&disc, "model/ene/e6010/e6010_m.bnd.dcx|e6010.flv", &|_| {
                acvd_render::RootPlace::IDENTITY
            })
            .unwrap();
        let ids: Vec<u8> = rig.effects.iter().map(|e| e.id).collect();
        for id in 200..=207 {
            assert!(
                ids.contains(&id),
                "e6010 effect point {id} missing: {ids:?}"
            );
        }
        let engines: Vec<_> = rig
            .effects
            .iter()
            .filter(|e| (200..=207).contains(&e.id))
            .collect();
        assert_eq!(engines.len(), 8);
        for e in engines {
            assert!(
                e.forward[2] > 0.99 && e.forward[0].abs() < 0.02 && e.forward[1].abs() < 0.02,
                "engine {} forward {:?} must point aft (+Z), not into the wing",
                e.id,
                e.forward
            );
        }
    }
}
