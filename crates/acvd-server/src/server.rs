//! Fixed-tick authoritative server.
//!
//! The runtime owns a `SimWorld`, the map collision, and one `InputQueue` per joined client. It
//! drains inputs, steps the simulation at 60 Hz with validated per-player input, sends snapshots
//! through interest management at 30/10/2 Hz, and keeps late joiners fed through the reliable
//! `BULK` channel with the full loadout/world/destructible state.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use acvd_formats::vfs::Disc;
use acvd_net::{
    ClientId, NetMetrics, NetServer, ServerEvent, BULK, COMBAT, COMMAND, HOT, RELIABLE,
};
use acvd_proto::{
    command_kind, quantize_coord, quantize_dir3, ClientCommandPacket, CombatPacket, DamageEvent,
    DestructibleBreak, FireEvent, HotMessage, InputPacket, JoinAccepted, KillEvent, Loadout,
    LoadoutUpdate, MatchState, NetId, PlayerJoined, PlayerLeft, ProjectileExpire, ProjectileHit,
    ReliableMessage, RespawnDrop, Wire, PROTOCOL_VERSION,
};
use acvd_sim::collision::{self, Collision, Hit, Layer, Plane};
use acvd_sim::events::SimEvent;
use acvd_sim::ids::{NetId as SimNetId, ProjectileId};
use acvd_sim::input::InputFrame as SimInput;
use acvd_sim::map::{self, Start, WaterReturnPoints};
use acvd_sim::rng::Pcg32;
use acvd_sim::stats;
use acvd_sim::tick::Tick;
use acvd_sim::weapons::HITSCAN_MAX_RANGE;
use acvd_sim::world::{wire_id, SimAc, SimWorld};
use anyhow::{Context, Result};
use glam::Vec3;

use crate::config::ServerConfig;
use crate::input_queue::{self, InputQueue};
use crate::interest::{self, InterestManager};
use crate::lagcomp::{self, HitboxHistory, HitscanOutcome};
use crate::snapshot::{self, RecordSet};

/// One joined player's server-side state.
struct ClientState {
    net_id: NetId,
    team: u8,
    loadout: Loadout,
    queue: InputQueue,
    /// Send the next snapshot with every AC, regardless of interest/cadence (join/late join).
    full_snapshot: bool,
    last_lock_target: SimNetId,
    joined_tick: u32,
    /// Scheduled automatic respawn release tick.
    respawn_at: Option<u32>,
    /// Tick the AC died (deterministic drop point seed).
    died_tick: u32,
    /// Smoothed transport quality used by the lag-compensation rewind.
    rtt_ms: f64,
    jitter_ms: f64,
}

/// The running server, split from the clock loop so tests can step it deterministically.
pub struct Server {
    config: ServerConfig,
    net: NetServer,
    tick: u32,
    snapshot_seq: u16,
    clients: HashMap<ClientId, ClientState>,
    world: SimWorld,
    collision: Collision,
    start: Start,
    water_return: Option<WaterReturnPoints>,
    interest: InterestManager,
    history: HitboxHistory,
    dropped_records: u64,
    dropped_corrections: u64,
    /// Coalesced per-target damage for the current tick (`COMBAT` channel).
    combat_damages: BTreeMap<SimNetId, DamageEvent>,
    combat_hits: Vec<ProjectileHit>,
    combat_expires: Vec<ProjectileExpire>,
    /// Unreliable FireEvents repeated for the next two ticks (deduped by projectile id).
    pending_fires: Vec<(bytes::Bytes, u8)>,
    hot_fire_events: u64,
    combat_packets: u64,
    kills: u64,
    respawns: u64,
}

impl Server {
    /// Validates data and binds the UDP server. With `allow_missing_disc` (tests only) the server
    /// uses a small synthetic arena instead of the disc map.
    pub fn bind(config: ServerConfig) -> Result<Self> {
        config.validate_data()?;
        let net = NetServer::bind(config.bind, config.max_clients, config.protocol_id)
            .context("bind multiplayer server")?;
        let mut config = config;
        let (collision, destructibles, start, water_return) = if config.allow_missing_disc {
            let (collision, start) = test_arena();
            (collision, Default::default(), start, None)
        } else {
            load_map_data(&config)?
        };
        let (pos_min, pos_max) = collision
            .bounds()
            .map(|(min, max)| interest::padded_aabb(min, max))
            .unwrap_or((config.quantizer.min, config.quantizer.max));
        config.quantizer = acvd_proto::PosQuantizer::new(pos_min, pos_max);
        let mut world = SimWorld::new();
        world.destructibles = destructibles;
        world.match_seed = config.match_seed;
        world.friendly_fire = config.friendly_fire;
        Ok(Self {
            config,
            net,
            tick: 0,
            snapshot_seq: 0,
            clients: HashMap::new(),
            world,
            collision,
            start,
            water_return,
            interest: InterestManager::new(),
            history: HitboxHistory::new(),
            dropped_records: 0,
            dropped_corrections: 0,
            combat_damages: BTreeMap::new(),
            combat_hits: Vec::new(),
            combat_expires: Vec::new(),
            pending_fires: Vec::new(),
            hot_fire_events: 0,
            combat_packets: 0,
            kills: 0,
            respawns: 0,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.net.local_addr()
    }

    pub fn tick(&self) -> u32 {
        self.tick
    }

    pub fn net(&self) -> &NetServer {
        &self.net
    }

    pub fn world(&self) -> &SimWorld {
        &self.world
    }

    /// Test/bot hook: direct authoritative world access.
    pub fn world_mut(&mut self) -> &mut SimWorld {
        &mut self.world
    }

    pub fn hitbox_history(&self) -> &HitboxHistory {
        &self.history
    }

    pub fn hot_fire_events_sent(&self) -> u64 {
        self.hot_fire_events
    }

    pub fn combat_packets_sent(&self) -> u64 {
        self.combat_packets
    }

    pub fn kill_count(&self) -> u64 {
        self.kills
    }

    pub fn respawn_count(&self) -> u64 {
        self.respawns
    }

    pub fn collision(&self) -> &Collision {
        &self.collision
    }

    pub fn players(&self) -> Vec<(ClientId, NetId)> {
        self.clients
            .iter()
            .map(|(client, state)| (*client, state.net_id))
            .collect()
    }

    pub fn player_count(&self) -> usize {
        self.clients.len()
    }

    pub fn net_id_for(&self, client: ClientId) -> Option<NetId> {
        self.clients.get(&client).map(|state| state.net_id)
    }

    pub fn last_input(&self, net_id: NetId) -> Option<acvd_proto::InputFrame> {
        self.clients
            .values()
            .find(|state| state.net_id == net_id)
            .and_then(|state| state.queue.last_frame())
    }

    pub fn acked_input_seq(&self, net_id: NetId) -> Option<u16> {
        self.clients
            .values()
            .find(|state| state.net_id == net_id)
            .map(|state| state.queue.last_acked())
    }

    pub fn queue_stats(&self, net_id: NetId) -> Option<input_queue::QueueStats> {
        self.clients
            .values()
            .find(|state| state.net_id == net_id)
            .map(|state| state.queue.stats())
    }

    pub fn metrics(&self) -> Vec<(ClientId, NetMetrics)> {
        self.net
            .clients()
            .into_iter()
            .map(|client| (client, self.net.metrics(client)))
            .collect()
    }

    pub fn dropped_snapshot_records(&self) -> u64 {
        self.dropped_records
    }

    pub fn dropped_missile_corrections(&self) -> u64 {
        self.dropped_corrections
    }

    /// One fixed 60 Hz iteration: transport/events, commands, inputs, `SimWorld::step`, events,
    /// snapshots.
    pub fn step(&mut self, dt: Duration) -> Result<()> {
        let events = self.net.poll(dt)?;
        for event in events {
            match event {
                ServerEvent::ClientConnected { client_id } => {
                    log::info!("client {client_id} connected");
                }
                ServerEvent::ClientDisconnected { client_id, reason } => {
                    log::info!("client {client_id} disconnected: {reason:?}");
                    self.despawn_client(client_id);
                }
            }
        }

        self.update_network_quality();
        self.receive_commands();
        self.receive_inputs();
        self.resend_fires();
        self.step_world();
        self.flush_combat();
        if self.config.snapshot_every > 0 && self.tick % self.config.snapshot_every == 0 {
            self.send_snapshots(false);
        }
        self.net.send_packets();
        Ok(())
    }

    /// Disconnects all clients, flushing the transport.
    pub fn shutdown(&mut self) {
        self.net.shutdown();
    }

    fn receive_commands(&mut self) {
        let clients = self.net.clients();
        for client in clients {
            while let Some(bytes) = self.net.receive(client, COMMAND) {
                let packet = match ClientCommandPacket::from_bytes(&bytes) {
                    Ok(packet) => packet,
                    Err(err) => {
                        log::warn!("client {client}: bad command: {err}");
                        continue;
                    }
                };
                if packet.protocol != PROTOCOL_VERSION {
                    log::warn!(
                        "client {client}: protocol {} != {}; disconnecting",
                        packet.protocol,
                        PROTOCOL_VERSION
                    );
                    self.net.disconnect(client);
                    continue;
                }
                match packet.kind {
                    command_kind::JOIN => self.handle_join(client, packet.loadout),
                    command_kind::LOADOUT => self.handle_loadout(client, packet.loadout),
                    command_kind::RESPAWN => self.handle_respawn(client),
                    command_kind::TEAM => self.handle_team(client, packet.seq),
                    _ => {}
                }
            }
        }
    }

    fn receive_inputs(&mut self) {
        let clients = self.net.clients();
        for client in clients {
            while let Some(bytes) = self.net.receive(client, HOT) {
                let packet = match InputPacket::from_bytes(&bytes) {
                    Ok(packet) => packet,
                    Err(err) => {
                        log::warn!("client {client}: bad input: {err}");
                        continue;
                    }
                };
                if let Some(state) = self.clients.get_mut(&client) {
                    state.queue.accept(packet.start_seq, &packet.frames);
                    if let Some(frame) = packet.frames.last() {
                        state.last_lock_target = acvd_sim::ids::NetId(frame.lock_target);
                    }
                }
            }
        }
    }

    /// Steps every AC once. Each queue yields at most one queued input per server tick
    /// (`InputQueue` policy); the newest queued frame is simulated for this server tick.
    fn step_world(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        let mut inputs: Vec<(SimNetId, SimInput)> = Vec::with_capacity(self.clients.len());
        for state in self.clients.values_mut() {
            let Some(ac) = self.world.acs.get(&sim_id(state.net_id)) else {
                continue;
            };
            if !state.queue.has_input() {
                // Do not integrate an AC before its first input: this keeps its authoritative
                // history aligned with the client's prediction from tick 1.
                continue;
            }
            let sampled = state.queue.next_tick(ac);
            let sim = input_queue::to_sim(&sampled.frame, ac);
            state.last_lock_target = sim.lock_target;
            inputs.push((sim_id(state.net_id), sim));
        }
        let events = self.world.step_with_water(
            &inputs,
            &mut self.collision,
            self.water_return.as_ref(),
            Some(self.start),
        );
        self.world.tick = Tick(self.tick);
        self.history.push_world(&self.world);
        for event in events {
            match event {
                SimEvent::Fired {
                    shooter,
                    projectile,
                    slot,
                    muzzle,
                    dir,
                    tick,
                    hitscan,
                } => self.handle_fired(shooter, projectile, slot, muzzle, dir, tick, hitscan),
                other => self.record_event(other),
            }
        }
        self.process_respawns();
    }

    fn update_network_quality(&mut self) {
        let samples: Vec<(ClientId, f64)> = self
            .net
            .clients()
            .into_iter()
            .map(|client| {
                let metrics = self.net.metrics(client);
                (client, metrics.rtt_ms)
            })
            .collect();
        for (client, rtt) in samples {
            if let Some(state) = self.clients.get_mut(&client) {
                let jitter = (rtt - state.rtt_ms).abs();
                state.jitter_ms = state.jitter_ms * 0.8 + jitter * 0.2;
                state.rtt_ms = if state.rtt_ms <= 0.0 {
                    rtt
                } else {
                    state.rtt_ms * 0.8 + rtt * 0.2
                };
            }
        }
    }

    fn client_quality(&self, net_id: SimNetId) -> (f64, f64) {
        self.clients
            .values()
            .find(|state| sim_id(state.net_id) == net_id)
            .map_or((0.0, 0.0), |state| (state.rtt_ms, state.jitter_ms))
    }

    /// Sends a `FireEvent` on `HOT` now and queues it for two repeat ticks.
    fn broadcast_fire(&mut self, event: FireEvent) {
        let bytes = HotMessage::Fire(event).to_bytes();
        self.broadcast(HOT, bytes.clone());
        self.pending_fires.push((bytes, 2));
        self.hot_fire_events = self.hot_fire_events.saturating_add(1);
    }

    fn resend_fires(&mut self) {
        let mut pending = std::mem::take(&mut self.pending_fires);
        for (bytes, ticks) in &mut pending {
            self.broadcast(HOT, bytes.clone());
            *ticks = ticks.saturating_sub(1);
        }
        pending.retain(|(_, ticks)| *ticks > 0);
        self.pending_fires = pending;
    }

    /// Handles one `Fired` event: broadcasts the FireEvent and validates hitscan/fast rounds
    /// against rewound hit spheres. Slow rounds are forward-simulated by `acvd-sim`.
    #[allow(clippy::too_many_arguments)]
    fn handle_fired(
        &mut self,
        shooter: SimNetId,
        projectile: ProjectileId,
        slot: acvd_sim::events::WeaponSlot,
        muzzle: Vec3,
        dir: Vec3,
        tick: u32,
        hitscan: bool,
    ) {
        let fire = FireEvent {
            shooter: wire_id(shooter),
            projectile_id: projectile.0,
            weapon_slot: slot as u8,
            muzzle_pos: [
                quantize_coord(muzzle.x),
                quantize_coord(muzzle.y),
                quantize_coord(muzzle.z),
            ],
            aim_dir: quantize_dir3(dir),
            fire_tick: tick,
            seed: (projectile.0 & 0xff) as u8,
        };
        self.broadcast_fire(fire);
        if !hitscan {
            return;
        }
        let Some(ac) = self.world.acs.get(&shooter) else {
            return;
        };
        let Some(gun) = ac.active_gun(slot).copied() else {
            return;
        };
        let dir = dir.normalize_or_zero();
        if dir == Vec3::ZERO {
            return;
        }
        let (rtt_ms, jitter_ms) = self.client_quality(shooter);
        let rewind = lagcomp::rewind_ticks(rtt_ms, jitter_ms);
        let outcome = lagcomp::resolve_hitscan(
            &self.history,
            &self.world,
            &self.collision,
            shooter,
            muzzle,
            dir,
            HITSCAN_MAX_RANGE,
            tick,
            rewind,
        );
        let this_tick = Tick(self.tick);
        match outcome {
            HitscanOutcome::Achit { target, position } => {
                let mut extra = Vec::new();
                let _ = self.world.apply_hit(
                    target,
                    shooter,
                    gun.damage_type,
                    gun.damage_power,
                    gun.damage_recoil,
                    position,
                    this_tick,
                    &mut extra,
                );
                self.combat_hits.push(ProjectileHit {
                    projectile_id: projectile.0,
                    target: wire_id(target),
                    hit_pos: [
                        quantize_coord(position.x),
                        quantize_coord(position.y),
                        quantize_coord(position.z),
                    ],
                    tick: this_tick.0,
                });
                for event in extra {
                    self.record_event(event);
                }
            }
            HitscanOutcome::Prop { prop_id, position } => {
                if self
                    .world
                    .destructibles
                    .apply_damage(acvd_sim::ids::PropId(prop_id), gun.damage_power.max(1.0))
                {
                    self.record_event(SimEvent::PropBroken {
                        prop: acvd_sim::ids::PropId(prop_id),
                        tick: this_tick.0,
                    });
                }
                self.combat_expires.push(ProjectileExpire {
                    projectile_id: projectile.0,
                    tick: this_tick.0,
                });
                let _ = position;
            }
            HitscanOutcome::Expire { position } => {
                self.combat_expires.push(ProjectileExpire {
                    projectile_id: projectile.0,
                    tick: this_tick.0,
                });
                let _ = position;
            }
        }
    }

    /// Turns one simulation event into channel traffic: reliable lifecycle/world events, coalesced
    /// `COMBAT` feedback, and deterministic respawn scheduling.
    fn record_event(&mut self, event: SimEvent) {
        match event {
            SimEvent::Fired { .. } => {}
            SimEvent::PropBroken { prop, tick } => {
                self.broadcast(
                    RELIABLE,
                    ReliableMessage::DestructibleBreak(DestructibleBreak {
                        prop_id: prop.0,
                        tick,
                    })
                    .to_bytes(),
                );
            }
            SimEvent::ProjectileHit {
                projectile,
                target,
                position,
            } => self.combat_hits.push(ProjectileHit {
                projectile_id: projectile.0,
                target: wire_id(target),
                hit_pos: [
                    quantize_coord(position.x),
                    quantize_coord(position.y),
                    quantize_coord(position.z),
                ],
                tick: self.tick,
            }),
            SimEvent::ProjectileExpire {
                projectile,
                position: _,
            } => self.combat_expires.push(ProjectileExpire {
                projectile_id: projectile.0,
                tick: self.tick,
            }),
            SimEvent::Damaged {
                target,
                attacker,
                amount,
                attribute,
                effective,
                lethal,
                position,
                tick,
                ..
            } => {
                let amount = amount.round().clamp(0.0, u16::MAX as f32) as u16;
                let hit_pos = [
                    quantize_coord(position.x),
                    quantize_coord(position.y),
                    quantize_coord(position.z),
                ];
                match self.combat_damages.get_mut(&target) {
                    Some(entry) => {
                        entry.amount = entry.amount.saturating_add(amount);
                        entry.effective = entry.effective.max(u8::from(effective));
                        entry.lethal |= lethal;
                        entry.hit_pos = hit_pos;
                        entry.attacker = wire_id(attacker);
                        entry.tick = tick;
                    }
                    None => {
                        self.combat_damages.insert(
                            target,
                            DamageEvent {
                                target: wire_id(target),
                                attacker: wire_id(attacker),
                                amount,
                                attribute,
                                effective: u8::from(effective),
                                lethal,
                                hit_pos,
                                tick,
                            },
                        );
                    }
                }
            }
            SimEvent::Killed {
                target,
                attacker,
                tick,
            } => {
                self.kills = self.kills.saturating_add(1);
                let message = ReliableMessage::KillEvent(KillEvent {
                    killer: wire_id(attacker),
                    victim: wire_id(target),
                    weapon: 0,
                    tick,
                });
                self.broadcast(RELIABLE, message.to_bytes());
                if let Some(state) = self
                    .clients
                    .values_mut()
                    .find(|state| sim_id(state.net_id) == target)
                {
                    state.respawn_at = Some(self.tick.wrapping_add(self.config.respawn_ticks));
                    state.died_tick = self.tick;
                }
            }
            SimEvent::Respawned {
                target,
                position,
                yaw,
                tick,
            } => {
                self.broadcast(
                    RELIABLE,
                    ReliableMessage::RespawnDrop(RespawnDrop {
                        net_id: wire_id(target),
                        drop_point: position.to_array(),
                        approach_yaw: yaw,
                        start_tick: tick,
                    })
                    .to_bytes(),
                );
            }
        }
    }

    fn flush_combat(&mut self) {
        if self.combat_damages.is_empty()
            && self.combat_hits.is_empty()
            && self.combat_expires.is_empty()
        {
            return;
        }
        let packet = CombatPacket {
            damages: std::mem::take(&mut self.combat_damages)
                .into_values()
                .collect(),
            hits: std::mem::take(&mut self.combat_hits),
            expires: std::mem::take(&mut self.combat_expires),
        };
        self.broadcast(COMBAT, packet.to_bytes());
        self.combat_packets = self.combat_packets.saturating_add(1);
    }

    fn process_respawns(&mut self) {
        let due: Vec<(ClientId, NetId, u8, u32)> = self
            .clients
            .iter()
            .filter_map(|(client, state)| {
                state.respawn_at.and_then(|at| {
                    (at <= self.tick).then_some((
                        *client,
                        state.net_id,
                        state.team,
                        state.died_tick,
                    ))
                })
            })
            .collect();
        for (client, net_id, team, died_tick) in due {
            let position = self.respawn_point(net_id, team, died_tick);
            let yaw = self.start.yaw;
            if let Some(ac) = self.world.acs.get_mut(&sim_id(net_id)) {
                ac.respawn(position, yaw);
            }
            if let Some(state) = self.clients.get_mut(&client) {
                state.queue.reset();
                state.full_snapshot = true;
                state.respawn_at = None;
            }
            self.respawns = self.respawns.saturating_add(1);
            self.broadcast(
                RELIABLE,
                ReliableMessage::RespawnDrop(RespawnDrop {
                    net_id,
                    drop_point: position.to_array(),
                    approach_yaw: yaw,
                    start_tick: self.tick,
                })
                .to_bytes(),
            );
        }
    }

    /// Deterministic drop point from the team spawn lane plus a seeded offset.
    fn respawn_point(&self, net_id: NetId, team: u8, died_tick: u32) -> Vec3 {
        let base = self.spawn_position(net_id, team);
        let mut rng = Pcg32::for_tick(self.config.match_seed, Tick(died_tick), sim_id(net_id));
        let jitter = Vec3::new(rng.range_f32(-4.0, 4.0), 0.0, rng.range_f32(-4.0, 4.0));
        let target = base + jitter;
        let ground = self
            .collision
            .ground_below(target + Vec3::Y * 20.0)
            .unwrap_or(base.y);
        Vec3::new(target.x, ground, target.z)
    }

    fn handle_join(&mut self, client: ClientId, blob: Vec<u8>) {
        if let Some(state) = self.clients.get_mut(&client) {
            let (net_id, loadout) = (state.net_id, state.loadout);
            state.full_snapshot = true;
            self.send_join_accepted(client, net_id, &loadout);
            self.send_bulk_state(client);
            self.send_snapshot_to(client, true);
            return;
        }
        let Some(net_id) = self.allocate_net_id() else {
            log::warn!("client {client}: server full");
            self.net.disconnect(client);
            return;
        };
        let loadout = Loadout::from_blob(&blob).unwrap_or_else(|_| default_loadout());
        let team = (self.clients.len() % 2) as u8;
        let position = self.spawn_position(net_id, team);
        let yaw = self.start.yaw;
        let mut ac = SimAc::new(sim_id(net_id), loadout, position, yaw, team);
        ac.motion.wall_jumps = 0;
        self.world.insert(ac);
        let state = ClientState {
            net_id,
            team,
            loadout,
            queue: InputQueue::new(),
            full_snapshot: true,
            last_lock_target: SimNetId::NONE,
            joined_tick: self.tick,
            respawn_at: None,
            died_tick: self.tick,
            rtt_ms: 0.0,
            jitter_ms: 0.0,
        };
        self.clients.insert(client, state);

        self.send_join_accepted(client, net_id, &loadout);
        self.send_bulk_state(client);
        self.net.send(
            client,
            RELIABLE,
            ReliableMessage::MatchState(MatchState {
                state: 1,
                score_a: 0,
                score_b: 0,
                time_left_ticks: 0,
            })
            .to_bytes(),
        );
        let joined = ReliableMessage::PlayerJoined(PlayerJoined { net_id, team }).to_bytes();
        self.broadcast_except(client, RELIABLE, joined);
        // Everyone needs the newcomer's part ids to assemble the remote rig.
        let update = ReliableMessage::LoadoutUpdate(LoadoutUpdate {
            net_id,
            loadout: loadout.to_blob(),
        })
        .to_bytes();
        self.broadcast(RELIABLE, update);
        log::info!("client {client} joined as net id {net_id} (team {team})");

        // Every client receives a full snapshot at the join tick, so the newcomer's AC and the
        // existing ACs are all at the same authoritative tick.
        self.interest.rebuild(&self.world);
        let records = RecordSet::build(&self.world, &self.config.quantizer);
        let recipients: Vec<ClientId> = self.clients.keys().copied().collect();
        for recipient in recipients {
            if self.clients.contains_key(&recipient) {
                self.send_snapshot_to_with(recipient, true, &records);
            }
        }
        self.snapshot_seq = self.snapshot_seq.wrapping_add(1);
    }

    fn send_join_accepted(&mut self, client: ClientId, net_id: NetId, loadout: &Loadout) {
        let join = JoinAccepted {
            protocol: PROTOCOL_VERSION,
            net_id,
            server_tick: self.tick,
            map_id: self.config.map_id(),
            pos_min: self.config.quantizer.min.to_array(),
            pos_max: self.config.quantizer.max.to_array(),
            loadout: loadout.to_blob(),
        };
        self.net.send(client, BULK, join.to_bytes());
    }

    /// Every loadout and the destructible state for a late joiner (and on re-join).
    fn send_bulk_state(&mut self, client: ClientId) {
        let mut states: Vec<(NetId, Loadout)> = self
            .clients
            .values()
            .map(|state| (state.net_id, state.loadout))
            .collect();
        states.sort_by_key(|(id, _)| *id);
        for (net_id, loadout) in states {
            let update = ReliableMessage::LoadoutUpdate(LoadoutUpdate {
                net_id,
                loadout: loadout.to_blob(),
            });
            self.net.send(client, BULK, update.to_bytes());
        }
        let world = ReliableMessage::WorldState(self.world.world_state());
        self.net.send(client, BULK, world.to_bytes());
    }

    fn handle_loadout(&mut self, client: ClientId, blob: Vec<u8>) {
        let Some(state) = self.clients.get_mut(&client) else {
            log::warn!("client {client}: loadout before join");
            return;
        };
        let loadout = match Loadout::from_blob(&blob) {
            Ok(loadout) => loadout,
            Err(err) => {
                log::warn!("client {client}: bad loadout: {err}");
                return;
            }
        };
        state.loadout = loadout;
        let stats = stats::stats_from_loadout(&loadout);
        let design = stats::design_from_loadout(&loadout);
        let armament = acvd_sim::weapons::Armament::from_design(&design);
        if let Some(ac) = self.world.acs.get_mut(&sim_id(state.net_id)) {
            ac.loadout = loadout;
            ac.stats = stats;
            ac.max_ap = ac.stats.ap_max;
            ac.ap = ac.ap.min(ac.max_ap);
            ac.armament = armament;
            ac.motion.ctrl = ac.stats.ctrl.clone();
            ac.motion.en = ac.motion.en.min(ac.motion.ctrl.en_capacity.max(0.0));
            ac.motion.en_max = ac.motion.ctrl.en_capacity.max(0.0);
        }
        let update = ReliableMessage::LoadoutUpdate(LoadoutUpdate {
            net_id: state.net_id,
            loadout: blob,
        });
        self.broadcast(RELIABLE, update.to_bytes());
    }

    fn handle_respawn(&mut self, client: ClientId) {
        let Some(state) = self.clients.get(&client) else {
            return;
        };
        // A requested respawn only schedules the deterministic drop; the tick loop releases it.
        let net_id = state.net_id;
        let team = state.team;
        if let Some(state) = self.clients.get_mut(&client) {
            if state.respawn_at.is_none() {
                state.died_tick = self.tick;
                state.respawn_at = Some(self.tick);
            }
        }
        let position = self.respawn_point(net_id, team, self.tick);
        let yaw = self.start.yaw;
        if let Some(ac) = self.world.acs.get_mut(&sim_id(net_id)) {
            if !ac.alive {
                ac.respawn(position, yaw);
            }
        }
        if let Some(state) = self.clients.get_mut(&client) {
            state.queue.reset();
            state.full_snapshot = true;
            state.respawn_at = None;
        }
        let drop = ReliableMessage::RespawnDrop(RespawnDrop {
            net_id,
            drop_point: position.to_array(),
            approach_yaw: yaw,
            start_tick: self.tick,
        });
        self.broadcast(RELIABLE, drop.to_bytes());
    }

    fn handle_team(&mut self, client: ClientId, seq: u16) {
        let team = (seq & 1) as u8;
        let Some(state) = self.clients.get_mut(&client) else {
            return;
        };
        state.team = team;
        let net_id = state.net_id;
        if let Some(ac) = self.world.acs.get_mut(&sim_id(net_id)) {
            ac.team = team;
        }
        let update = ReliableMessage::PlayerJoined(PlayerJoined { net_id, team });
        self.broadcast(RELIABLE, update.to_bytes());
    }

    fn send_snapshots(&mut self, force_full: bool) {
        self.interest.rebuild(&self.world);
        let records = RecordSet::build(&self.world, &self.config.quantizer);
        let mut recipients: Vec<ClientId> = self.clients.keys().copied().collect();
        recipients.sort_unstable();
        for client in recipients {
            let full = force_full
                || self
                    .clients
                    .get(&client)
                    .is_some_and(|state| state.full_snapshot);
            if let Some(state) = self.clients.get_mut(&client) {
                state.full_snapshot = false;
            }
            self.send_snapshot_to_with(client, full, &records);
        }
        self.snapshot_seq = self.snapshot_seq.wrapping_add(1);
    }

    fn send_snapshot_to(&mut self, client: ClientId, force_full: bool) {
        self.interest.rebuild(&self.world);
        let records = RecordSet::build(&self.world, &self.config.quantizer);
        self.send_snapshot_to_with(client, force_full, &records);
    }

    fn send_snapshot_to_with(&mut self, client: ClientId, force_full: bool, records: &RecordSet) {
        let Some(state) = self.clients.get(&client) else {
            return;
        };
        let packet = snapshot::build(
            &self.world,
            &self.interest,
            records,
            sim_id(state.net_id),
            state.last_lock_target,
            state.queue.last_acked(),
            self.snapshot_seq,
            force_full,
        );
        let split = snapshot::split(&packet);
        self.dropped_records += split.dropped_records as u64;
        self.dropped_corrections += split.dropped_corrections as u64;
        for packet in split.packets {
            self.net.send(client, HOT, packet.to_bytes());
        }
    }

    fn despawn_client(&mut self, client: ClientId) {
        let Some(state) = self.clients.remove(&client) else {
            return;
        };
        self.world.remove(sim_id(state.net_id));
        self.history.remove(sim_id(state.net_id));
        let left = ReliableMessage::PlayerLeft(PlayerLeft {
            net_id: state.net_id,
            reason: 0,
        })
        .to_bytes();
        self.broadcast(RELIABLE, left);
        log::info!(
            "client {client} (net id {}) left after {} ticks",
            state.net_id,
            self.tick.wrapping_sub(state.joined_tick)
        );
    }

    fn spawn_position(&self, net_id: NetId, team: u8) -> Vec3 {
        if self.config.allow_missing_disc {
            return interest::arena_spawn(team, sim_id(net_id));
        }
        let lane = f32::from(net_id.get().saturating_sub(1));
        let side = if team == 0 { -1.0 } else { 1.0 };
        let base = self.start.position;
        let position = base + Vec3::new(side * (6.0 + lane * 1.5), 2.0, lane * 2.0);
        let ground = self
            .collision
            .ground_below(position + Vec3::Y * 20.0)
            .unwrap_or(base.y);
        Vec3::new(position.x, ground, position.z)
    }

    fn allocate_net_id(&self) -> Option<NetId> {
        for id in acvd_proto::NetId::MIN..=acvd_proto::NetId::MAX {
            if self.clients.values().all(|state| state.net_id.get() != id) {
                return acvd_proto::NetId::new(id).ok();
            }
        }
        None
    }

    fn broadcast(&mut self, channel: u8, bytes: bytes::Bytes) {
        for client in self.net.clients() {
            self.net.send(client, channel, bytes.clone());
        }
    }

    fn broadcast_except(&mut self, except: ClientId, channel: u8, bytes: bytes::Bytes) {
        for client in self.net.clients() {
            if client != except {
                self.net.send(client, channel, bytes.clone());
            }
        }
    }
}

fn sim_id(id: NetId) -> SimNetId {
    SimNetId(id.get())
}

fn default_loadout() -> Loadout {
    stats::default_loadout()
}

/// Loads the disc map into simulation data, failing fast when the map cannot be read.
fn load_map_data(
    config: &ServerConfig,
) -> Result<(
    Collision,
    acvd_sim::map::DestructibleSet,
    Start,
    Option<WaterReturnPoints>,
)> {
    let path = config
        .disc
        .clone()
        .or_else(|| std::env::var("ACVD_DISC").ok().map(Into::into))
        .or_else(|| {
            let candidate = Path::new("private/x360/vd/ACV2.pe");
            candidate.is_file().then(|| candidate.to_path_buf())
        })
        .ok_or_else(|| anyhow::anyhow!("no game disc: pass --disc <path to ACV2.pe>"))?;
    let disc: Disc = Disc::open(&path).with_context(|| format!("open disc {}", path.display()))?;
    let collision = collision::load_map(&disc, &config.map)
        .with_context(|| format!("load map collision {}", config.map))?;
    let destructibles = map::load_destructibles(&disc, &config.map)
        .with_context(|| format!("load map destructibles {}", config.map))?;
    let start = map::start(&disc, &config.map, None)
        .with_context(|| format!("load map start {}", config.map))?
        .unwrap_or(Start {
            position: Vec3::ZERO,
            yaw: 0.0,
        });
    let water_return = map::WaterReturnPoints::load(&disc, &config.map, None, None).ok();
    Ok((collision, destructibles, start, water_return))
}

/// Small deterministic arena for disc-free tests: ground plane plus a wall at `z = 2`, 6 m in
/// front of the spawn, for wall-jump scripts.
pub fn test_arena() -> (Collision, Start) {
    let ground = Plane {
        y: 0.0,
        layer: Layer::Ground,
    };
    let wall = [
        Hit::solid(
            Vec3::new(-10.0, 0.0, 2.0),
            Vec3::new(10.0, 0.0, 2.0),
            Vec3::new(10.0, 16.0, 2.0),
        ),
        Hit::solid(
            Vec3::new(-10.0, 0.0, 2.0),
            Vec3::new(10.0, 16.0, 2.0),
            Vec3::new(-10.0, 16.0, 2.0),
        ),
    ];
    (
        Collision::with_hits(vec![ground], wall.to_vec()),
        Start {
            position: Vec3::new(0.0, 0.0, 8.0),
            yaw: 0.0,
        },
    )
}
