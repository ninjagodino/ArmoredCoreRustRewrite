//! Network session: transport, join/late-join bulk state, input sampling/prediction, and
//! snapshot storage for remote interpolation.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use acvd_net::netem::{self, NetemConfig, NetemHandle};
use acvd_net::{NetClient, NetMetrics, BULK, COMBAT, COMMAND, HOT, RELIABLE};
use acvd_proto::NetId;
use acvd_proto::{
    ac_flags, command_kind, ClientCommandPacket, CombatPacket, DamageEvent, FireEvent, HotMessage,
    InputPacket, JoinAccepted, KillEvent, Loadout, MissileCorrection, PosQuantizer,
    ProjectileExpire, ProjectileHit, ReliableMessage, RespawnDrop, SnapshotPacket, Wire,
    WorldState, NETCODE_PROTOCOL_ID, PROTOCOL_VERSION,
};
use acvd_server::ServerConfig;
use acvd_sim::collision::Collision;
use anyhow::{Context, Result};
use bevy::prelude::*;

use super::input_sampler::{self, InputRing, Pacer, SampledInput};
use super::interpolation::{self, RemoteBuffer, SampledView};
use super::prediction::{Prediction, Reconcile};

/// Connection mode requested on the command line.
#[derive(Clone, Debug, Default)]
pub struct NetConfig {
    pub listen: bool,
    pub listen_port: u16,
    pub connect: Option<SocketAddr>,
    pub netem: Option<NetemConfig>,
    /// Live bots hosted by `--listen` (they fight through the authoritative server).
    pub bots: usize,
    pub max_clients: usize,
    pub disc: Option<std::path::PathBuf>,
    pub map: String,
    pub loadout: Loadout,
}

impl NetConfig {
    pub fn enabled(&self) -> bool {
        self.listen || self.connect.is_some()
    }
}

/// Latest sampled input from the game controller, published by `control::pilot`.
#[derive(Clone, Copy, Debug)]
pub struct NetInput {
    pub frame: acvd_sim::input::InputFrame,
    pub aim_yaw: f32,
    pub aim_pitch: f32,
}

impl Default for NetInput {
    fn default() -> Self {
        Self {
            frame: acvd_sim::input::InputFrame::default(),
            aim_yaw: 0.0,
            aim_pitch: 0.0,
        }
    }
}

/// Everything the local client knows about the match.
pub struct OnlineSession {
    pub client: NetClient,
    pub local_id: Option<NetId>,
    pub loadout: Loadout,
    pub quantizer: PosQuantizer,
    pub prediction: Option<Prediction>,
    pub ring: InputRing,
    pub pacer: Pacer,
    pub next_seq: u16,
    pub acked_seq: u16,
    pub remotes: HashMap<NetId, RemoteBuffer>,
    pub loadouts: HashMap<NetId, Loadout>,
    pub pending_local_state: Option<acvd_proto::LocalPlayerState>,
    pub pending_local_tick: u32,
    pub pending_breaks: Vec<u32>,
    pub pending_world: Option<WorldState>,
    /// Server-driven combat feedback, consumed by `net::combat` once per frame.
    pub pending_fires: Vec<FireEvent>,
    pub pending_damage: Vec<DamageEvent>,
    pub pending_hits: Vec<ProjectileHit>,
    pub pending_expires: Vec<ProjectileExpire>,
    pub pending_kills: Vec<KillEvent>,
    pub pending_respawns: Vec<RespawnDrop>,
    pub pending_corrections: Vec<MissileCorrection>,
    /// Local AC is dead; prediction is paused until the server respawns it.
    pub local_dead: bool,
    pub respawn_start_tick: Option<u32>,
    pub last_server_tick: u32,
    pub last_snapshot_at: Instant,
    pub joined: bool,
    pub join_retry: f32,
    pub stats: SessionStats,
    pub last_metrics_log: f32,
}

/// Counters for logs/tests.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SessionStats {
    pub joins: u64,
    pub snapshots: u64,
    pub records: u64,
    pub breaks: u64,
    pub reconciles: u64,
    pub smoothed: u64,
    pub snaps: u64,
    pub inputs_sent: u64,
}

impl OnlineSession {
    pub fn new(client: NetClient, loadout: Loadout) -> Self {
        Self {
            client,
            local_id: None,
            loadout,
            quantizer: PosQuantizer::default(),
            prediction: None,
            ring: InputRing::new(),
            pacer: Pacer::default(),
            next_seq: 1,
            acked_seq: 0,
            remotes: HashMap::new(),
            loadouts: HashMap::new(),
            pending_local_state: None,
            pending_local_tick: 0,
            pending_breaks: Vec::new(),
            pending_world: None,
            pending_fires: Vec::new(),
            pending_damage: Vec::new(),
            pending_hits: Vec::new(),
            pending_expires: Vec::new(),
            pending_kills: Vec::new(),
            pending_respawns: Vec::new(),
            pending_corrections: Vec::new(),
            local_dead: false,
            respawn_start_tick: None,
            last_server_tick: 0,
            last_snapshot_at: Instant::now(),
            joined: false,
            join_retry: 0.0,
            stats: SessionStats::default(),
            last_metrics_log: 0.0,
        }
    }

    pub fn metrics(&self) -> NetMetrics {
        self.client.metrics()
    }

    /// Estimated authoritative tick now.
    pub fn estimated_server_tick(&self) -> u32 {
        let elapsed = self.last_snapshot_at.elapsed().as_secs_f32();
        interpolation::estimated_tick(self.last_server_tick, elapsed)
    }

    /// Render tick for remote interpolation (4 ticks / 66 ms behind).
    pub fn render_tick(&self) -> u32 {
        interpolation::render_tick(self.estimated_server_tick())
    }

    pub fn remote_view(&self, id: NetId) -> Option<SampledView> {
        self.remotes
            .get(&id)
            .and_then(|buffer| buffer.sample(self.render_tick()))
    }

    /// Sends the join command and retries until accepted.
    fn send_join(&mut self) {
        let packet = ClientCommandPacket {
            seq: 1,
            kind: command_kind::JOIN,
            protocol: PROTOCOL_VERSION,
            loadout: self.loadout.to_blob(),
        };
        self.client.send(COMMAND, packet.to_bytes());
    }

    /// One transport/frame update. Receives snapshots and lifecycle state.
    pub fn update(&mut self, dt: Duration) {
        if let Err(err) = self.client.update(dt) {
            log::warn!("net client update: {err:#}");
        }
        self.join_retry -= dt.as_secs_f32();
        if self.client.is_connected() && !self.joined && self.join_retry <= 0.0 {
            self.send_join();
            self.join_retry = 0.25;
        }

        while let Some(bytes) = self.client.receive(BULK) {
            // `JoinAccepted` is a bare BULK message; the rest of BULK is tagged lifecycle state.
            if let Ok(join) = JoinAccepted::from_bytes(&bytes) {
                self.handle_join_accepted(&join);
            } else {
                self.handle_reliable(&bytes);
            }
        }
        while let Some(bytes) = self.client.receive(RELIABLE) {
            self.handle_reliable(&bytes);
        }
        while let Some(bytes) = self.client.receive(HOT) {
            match HotMessage::from_bytes(&bytes) {
                Ok(HotMessage::Snapshot(packet)) => self.handle_snapshot(packet),
                Ok(HotMessage::Fire(event)) => self.pending_fires.push(event),
                Err(err) => log::warn!("bad hot message: {err}"),
            }
        }
        while let Some(bytes) = self.client.receive(COMBAT) {
            match CombatPacket::from_bytes(&bytes) {
                Ok(packet) => {
                    self.pending_damage.extend(packet.damages);
                    self.pending_hits.extend(packet.hits);
                    self.pending_expires.extend(packet.expires);
                }
                Err(err) => log::warn!("bad combat packet: {err}"),
            }
        }
        let _ = self.client.send_packets();
        self.log_metrics(dt);
    }

    fn handle_reliable(&mut self, bytes: &[u8]) {
        match ReliableMessage::from_bytes(bytes) {
            Ok(message) => self.handle_message(message),
            Err(err) => log::warn!("bad reliable message: {err}"),
        }
    }

    fn handle_message(&mut self, message: ReliableMessage) {
        match message {
            ReliableMessage::LoadoutUpdate(update) => {
                if let Ok(loadout) = Loadout::from_blob(&update.loadout) {
                    if self.local_id == Some(update.net_id) {
                        self.loadout = loadout;
                        if let Some(pred) = self.prediction.as_mut() {
                            pred.set_loadout(loadout);
                        }
                    }
                    self.loadouts.insert(update.net_id, loadout);
                }
            }
            ReliableMessage::PlayerLeft(left) => {
                self.remotes.remove(&left.net_id);
                self.loadouts.remove(&left.net_id);
            }
            ReliableMessage::DestructibleBreak(broke) => {
                self.pending_breaks.push(broke.prop_id);
                self.stats.breaks += 1;
            }
            ReliableMessage::WorldState(world) => {
                self.pending_world = Some(world);
            }
            ReliableMessage::RespawnDrop(drop) => {
                if self.local_id == Some(drop.net_id) {
                    // The server reset its input stream; wait for the first alive snapshot at or
                    // after `start_tick`, then start a fresh prediction from it.
                    self.ring.clear();
                    self.prediction = None;
                    self.pending_local_state = None;
                    self.local_dead = true;
                    self.respawn_start_tick = Some(drop.start_tick);
                }
                self.pending_respawns.push(drop);
            }
            ReliableMessage::KillEvent(kill) => {
                if self.local_id == Some(kill.victim) {
                    self.local_dead = true;
                    self.ring.clear();
                    self.prediction = None;
                    self.pending_local_state = None;
                }
                self.pending_kills.push(kill);
            }
            ReliableMessage::MatchState(_) | ReliableMessage::PlayerJoined(_) => {}
        }
    }

    fn handle_snapshot(&mut self, packet: SnapshotPacket) {
        self.stats.snapshots += 1;
        if packet.server_tick >= self.last_server_tick {
            self.last_server_tick = packet.server_tick;
            self.last_snapshot_at = Instant::now();
        }
        // Packet 2 repeats the ack; applying it here keeps the ring lean even if packet 1 is lost.
        if self.pending_local_state.is_none() {
            self.acked_seq = packet.acked_input_seq;
            self.ring.ack(packet.acked_input_seq);
        }
        self.pending_corrections
            .extend(packet.corrections.iter().copied());
        if self.pending_corrections.len() > 128 {
            let drop = self.pending_corrections.len() - 128;
            self.pending_corrections.drain(..drop);
        }
        for record in &packet.records {
            self.stats.records += 1;
            let sample = interpolation::RemoteSample::from_record(record, &self.quantizer);
            self.remotes
                .entry(record.net_id)
                .or_insert_with(RemoteBuffer::new)
                .insert(packet.server_tick, sample);
        }
        let newest = self.last_server_tick.saturating_sub(120);
        for buffer in self.remotes.values_mut() {
            buffer.prune(newest);
        }
        if let Some(local) = packet.local {
            let destroyed = local.flags & ac_flags::DESTROYED != 0;
            if self.local_dead {
                // Require both the reliable RespawnDrop and an alive authoritative state at or
                // after its release tick. A pre-death snapshot racing the KillEvent can never
                // resurrect the local AC.
                let respawn_ready = self
                    .respawn_start_tick
                    .is_some_and(|start| packet.server_tick >= start)
                    && !destroyed;
                if !respawn_ready {
                    return;
                }
                self.local_dead = false;
                self.respawn_start_tick = None;
            }
            if packet.server_tick >= self.pending_local_tick {
                self.pending_local_tick = packet.server_tick;
                self.acked_seq = local.last_acked_seq;
                self.pending_local_state = Some(local);
            }
        }
    }

    fn log_metrics(&mut self, dt: Duration) {
        self.last_metrics_log += dt.as_secs_f32();
        if self.last_metrics_log >= 5.0 {
            self.last_metrics_log = 0.0;
            let m = self.metrics();
            log::info!(
                "net: rtt {:.0} ms loss {:.1}% down {:.0} B/s up {:.0} B/s server tick {}",
                m.rtt_ms,
                m.packet_loss * 100.0,
                m.bytes_received_per_second,
                m.bytes_sent_per_second,
                self.last_server_tick
            );
        }
    }

    /// Number of simulation ticks to run this frame (0..=2) with send-ahead pacing.
    pub fn steps_due(&mut self, dt: f32) -> u32 {
        let lead = i32::from(self.next_seq.wrapping_sub(self.acked_seq).min(127));
        let metrics = self.client.metrics();
        let rtt_ticks = (metrics.rtt_ms / 1000.0 * 60.0).round() as i32;
        let ideal = (rtt_ticks / 2 + 1).clamp(1, 8);
        self.pacer.ticks(dt, lead, ideal)
    }

    /// Runs `steps` 60 Hz prediction ticks, records them in the ring, and sends the newest three
    /// unacked frames on `HOT`.
    pub fn step_ticks(
        &mut self,
        steps: u32,
        input: &NetInput,
        collision: &mut Collision,
        mut apply_to_prop: impl FnMut(u32, f32) -> bool,
    ) {
        if self.prediction.is_none() || !self.joined || self.local_dead {
            return;
        }
        for _ in 0..steps {
            let sampled =
                input_sampler::sample(self.next_seq, &input.frame, input.aim_yaw, input.aim_pitch);
            self.next_seq = self.next_seq.wrapping_add(1);
            if let Some(pred) = self.prediction.as_mut() {
                pred.step(sampled, collision, &mut apply_to_prop);
            }
            self.ring.push(sampled);
            self.send_input_packet();
        }
        if steps > 0 {
            // Flush immediately so the input is in flight before the next frame's receive pass.
            let _ = self.client.send_packets();
        }
    }

    fn send_input_packet(&mut self) {
        let Some(start_seq) = self.ring.send_start_seq() else {
            return;
        };
        let frames = self.ring.send_frames();
        if frames.is_empty() {
            return;
        }
        let packet = InputPacket { start_seq, frames };
        self.client.send(HOT, packet.to_bytes());
        self.stats.inputs_sent += 1;
    }

    /// Applies the newest authoritative local state and replays unacked inputs.
    pub fn reconcile_pending(
        &mut self,
        collision: &mut Collision,
        mut apply_to_prop: impl FnMut(u32, f32) -> bool,
    ) -> Option<Reconcile> {
        let state = self.pending_local_state.take()?;
        let id = self.local_id?;
        let acked = state.last_acked_seq;
        let frames: Vec<SampledInput> = self
            .ring
            .iter()
            .filter(|frame| frame.seq.wrapping_sub(acked) < 0x8000 && frame.seq != acked)
            .copied()
            .collect();
        self.ring.ack(acked);
        self.acked_seq = acked;
        let result = match self.prediction.as_mut() {
            Some(pred) => pred.reconcile(&state, frames.into_iter(), collision, &mut apply_to_prop),
            None => {
                let mut pred =
                    Prediction::from_state(acvd_sim::ids::NetId(id.get()), self.loadout, &state);
                for frame in frames {
                    pred.step(frame, collision, &mut apply_to_prop);
                }
                self.prediction = Some(pred);
                Reconcile::Initial
            }
        };
        self.stats.reconciles += 1;
        match result {
            Reconcile::Smooth { .. } => self.stats.smoothed += 1,
            Reconcile::Snapped { .. } => self.stats.snaps += 1,
            _ => {}
        }
        Some(result)
    }

    pub fn take_breaks(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.pending_breaks)
    }

    pub fn take_world_state(&mut self) -> Option<WorldState> {
        self.pending_world.take()
    }

    pub fn take_fires(&mut self) -> Vec<FireEvent> {
        std::mem::take(&mut self.pending_fires)
    }

    pub fn take_damage(&mut self) -> Vec<DamageEvent> {
        std::mem::take(&mut self.pending_damage)
    }

    pub fn take_hits(&mut self) -> Vec<ProjectileHit> {
        std::mem::take(&mut self.pending_hits)
    }

    pub fn take_expires(&mut self) -> Vec<ProjectileExpire> {
        std::mem::take(&mut self.pending_expires)
    }

    pub fn take_kills(&mut self) -> Vec<KillEvent> {
        std::mem::take(&mut self.pending_kills)
    }

    pub fn take_respawns(&mut self) -> Vec<RespawnDrop> {
        std::mem::take(&mut self.pending_respawns)
    }

    pub fn take_corrections(&mut self) -> Vec<MissileCorrection> {
        std::mem::take(&mut self.pending_corrections)
    }

    pub fn handle_join_accepted(&mut self, join: &JoinAccepted) {
        if join.protocol != PROTOCOL_VERSION {
            log::warn!("server protocol {} != {}", join.protocol, PROTOCOL_VERSION);
            return;
        }
        self.local_id = Some(join.net_id);
        // The server echoed the authoritative loadout; keep the client's matching design parts.
        if let Ok(loadout) = Loadout::from_blob(&join.loadout) {
            self.loadout = loadout;
        }
        self.quantizer = PosQuantizer::new(join.pos_min.into(), join.pos_max.into());
        self.last_server_tick = join.server_tick;
        self.last_snapshot_at = Instant::now();
        self.joined = true;
        self.stats.joins += 1;
        log::info!(
            "joined server as net id {} at tick {} (map {})",
            join.net_id,
            join.server_tick,
            join.map_id
        );
    }
}

/// Bevy resource owning the session and the local prediction glue.
#[derive(Resource)]
pub struct NetRuntime {
    pub session: OnlineSession,
    pub input: NetInput,
    pub last_aim: (f32, f32),
    pub server_shutdown: Option<Arc<AtomicBool>>,
    pub server_thread: Option<JoinHandle<()>>,
    pub netem: Option<NetemHandle>,
}

impl NetRuntime {
    /// Starts a listen server and/or transport. Returns `None` for solo play.
    pub fn start(config: NetConfig) -> Result<Option<Self>> {
        if !config.enabled() {
            return Ok(None);
        }
        let mut server_shutdown = None;
        let mut server_thread = None;
        let mut target = config.connect;
        let mut netem = None;

        if config.listen {
            let port = if config.listen_port == 0 {
                acvd_proto::DEFAULT_PORT
            } else {
                config.listen_port
            };
            let server_config = ServerConfig {
                bind: SocketAddr::from(([127, 0, 0, 1], port)),
                max_clients: config.max_clients.max(config.bots + 1).max(1),
                protocol_id: NETCODE_PROTOCOL_ID,
                map: config.map.clone(),
                disc: config.disc.clone(),
                allow_missing_disc: false,
                snapshot_every: 2,
                ..ServerConfig::default()
            };
            let shutdown = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&shutdown);
            let bots = config.bots;
            let thread = std::thread::Builder::new()
                .name("acvd-listen-server".into())
                .spawn(move || {
                    let result = acvd_server::run_with_bots(
                        server_config,
                        bots,
                        acvd_server::bots::Distribution::Cluster,
                        acvd_sim::world::DEFAULT_MATCH_SEED,
                        flag,
                    );
                    if let Err(err) = result {
                        log::error!("listen server: {err:#}");
                    }
                })
                .context("spawn listen server")?;
            server_shutdown = Some(shutdown);
            server_thread = Some(thread);
            target = Some(SocketAddr::from(([127, 0, 0, 1], port)));
            // Give the server socket a moment to bind before the client starts sending.
            std::thread::sleep(Duration::from_millis(100));
        }

        let server_addr = target.context("no connect address (use --connect or --listen)")?;
        if let Some(conditioner) = config.netem {
            let relay = netem::start(server_addr, "127.0.0.1:0".parse().unwrap(), conditioner)
                .context("start netem relay")?;
            target = Some(relay.addr());
            netem = Some(relay);
        }
        let client_id = (std::process::id() as u64) << 8 | 1;
        let client = NetClient::connect(target.unwrap(), client_id, NETCODE_PROTOCOL_ID)
            .context("connect netcode client")?;
        let session = OnlineSession::new(client, config.loadout);
        Ok(Some(Self {
            session,
            input: NetInput::default(),
            last_aim: (0.0, 0.0),
            server_shutdown,
            server_thread,
            netem,
        }))
    }
}

impl Drop for NetRuntime {
    fn drop(&mut self) {
        self.session.client.disconnect();
        let _ = self.session.client.send_packets();
        if let Some(mut relay) = self.netem.take() {
            relay.shutdown();
        }
        if let Some(flag) = self.server_shutdown.take() {
            flag.store(true, Ordering::Relaxed);
        }
        if let Some(thread) = self.server_thread.take() {
            let _ = thread.join();
        }
    }
}

/// Updates the transport and processes received state. Runs before prediction each frame.
pub fn update_transport(time: Res<Time>, mut runtime: ResMut<NetRuntime>) {
    let dt = time.delta();
    runtime.session.update(dt);
}

/// Applies reliable destructible deltas to the local collision and prop presentation before any
/// prediction replay.
pub fn apply_world_deltas(
    mut runtime: ResMut<NetRuntime>,
    mut collision: ResMut<crate::collision::Collision>,
    mut props: Query<&mut crate::map::DestructibleProp>,
) {
    let breaks = runtime.session.take_breaks();
    for prop_id in breaks {
        collision.break_prop(prop_id);
        for mut prop in &mut props {
            if prop.prop_id.0 == prop_id {
                // A local prediction may already have broken this prop; only the first break
                // transition may spawn debris/sfx.
                let was_broken = prop.sim.broken;
                prop.sim.hp = 0.0;
                prop.sim.broken = true;
                prop.sim.just_broken = !was_broken;
            }
        }
    }
    if let Some(world) = runtime.session.take_world_state() {
        // Full broken bitset first (late join), then the damaged HP array.
        for (index, byte) in world.broken_bits.iter().enumerate() {
            for bit in 0..8 {
                if byte & (1 << bit) == 0 {
                    continue;
                }
                let prop_id = (index * 8 + bit + 1) as u32;
                collision.break_prop(prop_id);
                for mut prop in &mut props {
                    if prop.prop_id.0 == prop_id {
                        prop.sim.hp = 0.0;
                        prop.sim.broken = true;
                        prop.sim.just_broken = false;
                    }
                }
            }
        }
        for state in &world.props {
            if state.broken {
                collision.break_prop(state.prop_id);
            }
            for mut prop in &mut props {
                if prop.prop_id.0 == state.prop_id {
                    prop.sim.hp = state.hp;
                    prop.sim.broken = state.broken;
                    prop.sim.just_broken = false;
                }
            }
        }
    }
}

/// Runs the 60 Hz local prediction and copies the result onto the local `Pilot`/transform.
pub fn step_prediction(
    time: Res<Time>,
    mut runtime: ResMut<NetRuntime>,
    mut collision: ResMut<crate::collision::Collision>,
    mut pilots: Query<(&mut crate::control::Pilot, &mut Transform)>,
) {
    // The server owns prop HP online. Local prediction therefore never applies collision damage
    // to props (a local break followed by `DestructibleBreak` would double-spawn debris/sfx).
    if runtime.session.joined {
        let _ = runtime
            .session
            .reconcile_pending(&mut collision.0, |_prop_id, _raw| false);
    }
    let steps = runtime.session.steps_due(time.delta_secs());
    if steps > 0 {
        let input = runtime.input;
        runtime
            .session
            .step_ticks(steps, &input, &mut collision.0, |_prop_id, _raw| false);
    }
    runtime.last_aim = (runtime.input.aim_yaw, runtime.input.aim_pitch);
    let Some(pred) = runtime.session.prediction.as_mut() else {
        return;
    };
    pred.decay_visual(time.delta_secs());
    let position = pred.visual_position();
    let yaw = pred.visual_yaw();
    let motion = pred.ac.motion.clone();
    pred.clear_presentation();
    if let Ok((mut pilot, mut transform)) = pilots.single_mut() {
        pilot.motion = motion;
        transform.translation = position;
        transform.rotation = Quat::from_rotation_y(yaw);
        // Keep the camera on the sampled absolute aim; the body turns toward it under the
        // server's turn-rate clamp.
        let body_yaw = pilot.motion.yaw;
        pilot.set_net_aim(
            acvd_proto::wrap_angle(runtime.last_aim.0 - body_yaw),
            runtime.last_aim.1,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    fn test_server() -> acvd_server::Server {
        acvd_server::Server::bind(acvd_server::ServerConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            max_clients: 4,
            allow_missing_disc: true,
            snapshot_every: 2,
            ..acvd_server::ServerConfig::default()
        })
        .expect("server")
    }

    fn client_id() -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(0x4400);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn online_session_joins_predicts_and_reconciles() {
        let mut server = test_server();
        let net = NetClient::connect(server.local_addr(), client_id(), NETCODE_PROTOCOL_ID)
            .expect("client");
        let mut session = OnlineSession::new(net, acvd_sim::stats::default_loadout());
        let mut collision = acvd_server::test_arena().0;
        let dt = Duration::from_millis(2);
        let input = NetInput {
            frame: acvd_sim::input::InputFrame {
                stick: glam::Vec2::Y,
                aim_yaw: 0.35,
                ..acvd_sim::input::InputFrame::default()
            },
            aim_yaw: 0.35,
            aim_pitch: 0.0,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut predicted_ticks = 0u32;
        while Instant::now() < deadline && predicted_ticks < 120 {
            server.step(dt).expect("server step");
            session.update(dt);
            if session.joined {
                let _ = session.reconcile_pending(&mut collision, |_, _| false);
                if session.prediction.is_some() {
                    session.step_ticks(1, &input, &mut collision, |_, _| false);
                    predicted_ticks += 1;
                }
            }
            std::thread::sleep(dt);
        }
        assert!(session.joined, "client joins");
        assert!(session.local_id.is_some());
        let pred = session.prediction.as_ref().expect("prediction");
        assert!(predicted_ticks >= 100, "predicted {predicted_ticks} ticks");
        assert!(
            pred.stats.reconciles > 0,
            "snapshots reconciled against prediction"
        );
        assert!(pred.ac.motion.yaw > 0.1, "turned toward the aim");
        assert!(
            pred.position().distance(Vec3::new(-2.0, 0.0, 8.0)) > 0.5,
            "moved from spawn: {:?}",
            pred.position()
        );
        assert!(session.stats.snapshots > 0);
        let local = session.local_id.unwrap();
        let ac = server
            .world()
            .acs
            .get(&acvd_sim::ids::NetId(local.get()))
            .expect("server AC");
        assert!(ac.motion.yaw > 0.1, "server turned too");
        session.client.disconnect();
        let _ = session.client.send_packets();
        server.shutdown();
    }
}
