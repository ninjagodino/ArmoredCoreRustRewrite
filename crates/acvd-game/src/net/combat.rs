//! Client multiplayer combat presentation.
//!
//! The client never rolls damage or hits: it spawns deterministic visual rounds from server
//! `FireEvent`s (deduped by projectile id), applies capped homing corrections, and consumes
//! `DamageEvent`/`ProjectileHit`/`ProjectileExpire`/`KillEvent`/`RespawnDrop` from the server.

use std::collections::{HashMap, HashSet};

use acvd_proto::{DamageEvent, Loadout};
use acvd_sim::weapons as simw;
use bevy::audio::AudioSource;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::damage::PlayerHealth;
use crate::hud::Status;
use crate::net::session::{NetRuntime, OnlineSession};
use crate::net::view::{NetView, RemotePlayer};
use crate::sfx::Sfx;
use crate::sound::{self, Cues};
use crate::weapons::{self, NetProjectile, Tracers};

/// Presentation hook for the helicopter/boost drop-in spline. A later presentation task can read
/// this component and animate the AC from `drop_point + approach * height` down to `drop_point`.
#[derive(Component, Clone, Copy, Debug)]
#[allow(dead_code)]
pub struct RespawnDropIn {
    pub drop_point: Vec3,
    pub approach_yaw: f32,
    pub start_tick: u32,
}

/// Dedup and per-shooter armament cache for visual FireEvents.
#[derive(Resource, Default)]
pub struct NetFxState {
    seen_projectiles: HashSet<u32>,
    armaments: HashMap<u8, (Loadout, simw::Armament)>,
    /// Newest `DamageEvent`/`KillEvent` tick applied per target. `COMBAT` is reliable-unordered,
    /// so a late event from before a respawn must never damage the new life.
    last_damage_tick: HashMap<acvd_proto::NetId, u32>,
}

impl NetFxState {
    fn gun_for(
        &mut self,
        session: &OnlineSession,
        shooter: acvd_proto::NetId,
        slot: u8,
    ) -> Option<simw::Gun> {
        let slot = acvd_sim::world::weapon_slot_of(slot);
        if session.local_id == Some(shooter) {
            let pred = session.prediction.as_ref()?;
            return pred.ac.active_gun(slot).copied();
        }
        let loadout = *session.loadouts.get(&shooter)?;
        let stale = self
            .armaments
            .get(&shooter.get())
            .is_none_or(|(cached, _)| *cached != loadout);
        if stale {
            let design = acvd_sim::stats::design_from_loadout(&loadout);
            self.armaments.insert(
                shooter.get(),
                (loadout, simw::Armament::from_design(&design)),
            );
        }
        let mut arm = self.armaments.get(&shooter.get())?.1.clone();
        if let Some(view) = session.remote_view(shooter) {
            arm.apply_combat_state(view.combat_flags, [0; 2]);
        }
        match slot {
            simw::WeaponSlot::RightHand => Some(*arm.active_gun(0)),
            simw::WeaponSlot::LeftHand => Some(*arm.active_gun(1)),
            simw::WeaponSlot::Shoulder => Some(arm.shoulder),
            simw::WeaponSlot::Overed => None,
        }
    }
}

/// Spawns visual projectiles/tracers and muzzle FFX/audio for each new server `FireEvent`.
pub fn spawn_fire_events(
    mut runtime: ResMut<NetRuntime>,
    mut fx: ResMut<NetFxState>,
    assets: Res<Tracers>,
    mut sounds: Sounds,
    mut commands: Commands,
) {
    let events = runtime.session.take_fires();
    for event in events {
        if fx.seen_projectiles.contains(&event.projectile_id) {
            continue;
        }
        let Some(gun) = fx.gun_for(&runtime.session, event.shooter, event.weapon_slot) else {
            continue;
        };
        if gun.is_silent() {
            continue;
        }
        weapons::spawn_net_projectile(
            &mut commands,
            &assets,
            &mut sounds.cues,
            &mut sounds.sources,
            &event,
            &gun,
        );
        fx.seen_projectiles.insert(event.projectile_id);
        if fx.seen_projectiles.len() > 8192 {
            fx.seen_projectiles.clear();
        }
    }
}

/// One system parameter keeping the combat systems inside Bevy's parameter limit.
#[derive(SystemParam)]
pub struct Sounds<'w> {
    pub cues: ResMut<'w, Cues>,
    pub sources: ResMut<'w, Assets<AudioSource>>,
}

/// Applies combat feedback. Idempotent: damage after a local kill is ignored, and late reliable
/// `COMBAT` messages can never resurrect or re-damage a dead target.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn apply_combat(
    mut runtime: ResMut<NetRuntime>,
    mut view: ResMut<NetView>,
    mut fx: ResMut<NetFxState>,
    mut commands: Commands,
    mut sounds: Sounds,
    mut rounds: Query<(Entity, &mut NetProjectile)>,
    mut players: Query<(Entity, &mut PlayerHealth, Option<&mut Status>, &Transform)>,
    remotes: Query<(Entity, &RemotePlayer, &Transform)>,
) {
    let session = &mut runtime.session;

    // Capped homing-missile corrections from snapshots.
    for correction in session.take_corrections() {
        let position = Vec3::new(
            acvd_proto::dequantize_coord(correction.pos[0]),
            acvd_proto::dequantize_coord(correction.pos[1]),
            acvd_proto::dequantize_coord(correction.pos[2]),
        );
        let velocity = Vec3::new(
            acvd_proto::dequantize_coord(correction.vel[0]),
            acvd_proto::dequantize_coord(correction.vel[1]),
            acvd_proto::dequantize_coord(correction.vel[2]),
        );
        if let Some((_, mut round)) = rounds
            .iter_mut()
            .find(|(_, round)| round.id == correction.projectile_id)
        {
            if matches!(round.flight, weapons::NetFlight::Missile(_)) {
                round.flight.set_state(position, velocity);
            }
        }
    }

    // Projectile hits/expires remove the visual round and place the impact FX/sound.
    let hits = session.take_hits();
    let mut hit_ids = HashSet::new();
    for hit in &hits {
        hit_ids.insert(hit.projectile_id);
        let at = Vec3::new(
            acvd_proto::dequantize_coord(hit.hit_pos[0]),
            acvd_proto::dequantize_coord(hit.hit_pos[1]),
            acvd_proto::dequantize_coord(hit.hit_pos[2]),
        );
        if let Some((_, round)) = rounds
            .iter()
            .find(|(_, round)| round.id == hit.projectile_id)
        {
            let effect = if round.hit_fx > 0 {
                round.hit_fx
            } else if round.hit_hi_fx > 0 {
                round.hit_hi_fx
            } else {
                round.hit_low_fx
            };
            if effect > 0 {
                commands.spawn((Sfx::new(effect), Transform::from_translation(at)));
            }
            if round.hit_se > 0 {
                sound::cue_at(
                    &mut commands,
                    &mut sounds.cues,
                    &mut sounds.sources,
                    &format!("w{:08}", round.hit_se),
                    at,
                );
            }
        }
    }
    let expires = session.take_expires();
    let mut expire_ids = HashSet::new();
    for expire in &expires {
        expire_ids.insert(expire.projectile_id);
    }
    for (entity, round) in &rounds {
        if expire_ids.contains(&round.id) {
            if round.delete_fx > 0 {
                let at = round.flight.position();
                commands.spawn((Sfx::new(round.delete_fx), Transform::from_translation(at)));
            }
            commands.entity(entity).despawn();
        } else if hit_ids.contains(&round.id) {
            commands.entity(entity).despawn();
        }
    }

    // Authoritative damage to the local AC. The server amount is already mitigated; do not roll
    // the 360 defense formula a second time.
    for damage in session.take_damage() {
        if session.local_id != Some(damage.target) {
            continue;
        }
        let last = fx
            .last_damage_tick
            .get(&damage.target)
            .copied()
            .unwrap_or(0);
        if last != 0 && damage.tick <= last {
            continue;
        }
        fx.last_damage_tick.insert(damage.target, damage.tick);
        if let Ok((_, mut health, status, _)) = players.single_mut() {
            apply_authoritative_damage(&mut health, status, &damage);
        }
    }

    for kill in session.take_kills() {
        let last = fx.last_damage_tick.get(&kill.victim).copied().unwrap_or(0);
        fx.last_damage_tick.insert(kill.victim, last.max(kill.tick));
        if session.local_id == Some(kill.victim) {
            if let Ok((_, mut health, status, transform)) = players.single_mut() {
                if !health.destroyed {
                    health.ap = 0.0;
                    health.destroyed = true;
                    health.just_destroyed = true;
                    if let Some(mut status) = status {
                        status.ap = 0;
                    }
                }
                commands.spawn((
                    Sfx::new(4041),
                    Transform::from_translation(transform.translation + Vec3::Y * 4.0),
                ));
                sound::cue_at(
                    &mut commands,
                    &mut sounds.cues,
                    &mut sounds.sources,
                    "w00009600",
                    transform.translation + Vec3::Y * 4.0,
                );
            }
        } else if let Some((root, player, transform)) = remotes
            .iter()
            .find(|(_, player, _)| player.net_id == kill.victim.get())
        {
            if view.dead.insert(player.net_id) {
                commands.entity(root).insert(Visibility::Hidden);
                commands.spawn((
                    Sfx::new(4041),
                    Transform::from_translation(transform.translation + Vec3::Y * 4.0),
                ));
                sound::cue_at(
                    &mut commands,
                    &mut sounds.cues,
                    &mut sounds.sources,
                    "w00009601",
                    transform.translation + Vec3::Y * 4.0,
                );
            }
        } else {
            view.dead.insert(kill.victim.get());
        }
    }

    for drop in session.take_respawns() {
        let drop_point = Vec3::from_array(drop.drop_point);
        fx.last_damage_tick.insert(drop.net_id, drop.start_tick);
        if session.local_id == Some(drop.net_id) {
            if let Ok((entity, mut health, status, _)) = players.single_mut() {
                // Water/fallback respawns reposition without healing; only a death respawn
                // restores AP.
                if health.destroyed {
                    health.reset();
                }
                if let Some(mut status) = status {
                    status.ap = health.ap_u32();
                }
                commands.entity(entity).insert(RespawnDropIn {
                    drop_point,
                    approach_yaw: drop.approach_yaw,
                    start_tick: drop.start_tick,
                });
            }
            commands.spawn((
                Sfx::new(305),
                Transform::from_translation(drop_point + Vec3::Y * 12.0),
            ));
        } else {
            view.dead.remove(&drop.net_id.get());
            if let Some((entity, _, _)) = remotes
                .iter()
                .find(|(_, player, _)| player.net_id == drop.net_id.get())
            {
                commands.entity(entity).insert(Visibility::Visible);
                commands.entity(entity).insert(RespawnDropIn {
                    drop_point,
                    approach_yaw: drop.approach_yaw,
                    start_tick: drop.start_tick,
                });
            }
            commands.spawn((
                Sfx::new(305),
                Transform::from_translation(drop_point + Vec3::Y * 12.0),
            ));
        }
    }
}

fn apply_authoritative_damage(
    health: &mut PlayerHealth,
    status: Option<Mut<Status>>,
    damage: &DamageEvent,
) {
    if health.destroyed {
        return;
    }
    health.hits_taken = health.hits_taken.saturating_add(1);
    health.ap = (health.ap - f32::from(damage.amount)).max(0.0);
    health.hit_shake = Some((health.hit_shake.map_or(0.0, |(r, _)| r).max(1.0), false));
    if health.ap <= 0.0 || damage.lethal {
        health.ap = 0.0;
        health.destroyed = true;
        health.just_destroyed = true;
    }
    if let Some(mut status) = status {
        status.ap = health.ap_u32();
    }
}
