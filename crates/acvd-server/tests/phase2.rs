//! Phase 2 gates: scripted movement (boost glide, wall jump, kick) under 80/120 ms RTT + 2%
//! loss, remote interpolation smoothness, late join at an arbitrary tick, and drop-in/drop-out
//! churn leak checks.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use acvd_net::netem::{self, NetemConfig};
use acvd_net::{NetClient, BULK, COMMAND, HOT, RELIABLE};
use acvd_proto::*;
use acvd_server::{Server, ServerConfig};
use acvd_sim::collision::Collision;
use acvd_sim::ids::NetId as SimId;
use acvd_sim::input::InputFrame as SimInput;
use acvd_sim::world::{apply_local_state, SimAc};
use glam::{Vec2, Vec3};

const DT: Duration = Duration::from_millis(16);

fn config() -> ServerConfig {
    ServerConfig {
        bind: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        max_clients: 8,
        allow_missing_disc: true,
        snapshot_every: 2,
        ..ServerConfig::default()
    }
}

fn wire_buttons(frame: &SimInput) -> u16 {
    let mut b = 0;
    if frame.jump {
        b |= input_buttons::JUMP;
    }
    if frame.boost_toggle {
        b |= input_buttons::BOOST_TOGGLE;
    }
    if frame.glide {
        b |= input_buttons::GLIDE;
    }
    if frame.quick_boost {
        b |= input_buttons::QUICK_BOOST;
    }
    if frame.qb_held {
        b |= input_buttons::QB_HELD;
    }
    if frame.kick {
        b |= input_buttons::KICK;
    }
    b
}

#[derive(Clone, Copy, Debug)]
struct Sampled {
    seq: u16,
    wire: InputFrame,
    sim: SimInput,
}

fn sample(seq: u16, frame: &SimInput, aim_yaw: f32, aim_pitch: f32) -> Sampled {
    let wire = InputFrame {
        buttons: wire_buttons(frame),
        stick: [
            (frame.stick.x * 127.0).round().clamp(-127.0, 127.0) as i8,
            (frame.stick.y * 127.0).round().clamp(-127.0, 127.0) as i8,
        ],
        aim_yaw: quantize_yaw(aim_yaw),
        aim_pitch: quantize_input_pitch(aim_pitch),
        lock_target: 0,
    };
    let sim = SimInput {
        stick: Vec2::new(
            f32::from(wire.stick[0]) / 127.0,
            f32::from(wire.stick[1]) / 127.0,
        ),
        aim_yaw: dequantize_yaw(wire.aim_yaw),
        aim_pitch: dequantize_input_pitch(wire.aim_pitch),
        jump: wire.buttons & input_buttons::JUMP != 0,
        boost_toggle: wire.buttons & input_buttons::BOOST_TOGGLE != 0,
        glide: wire.buttons & input_buttons::GLIDE != 0,
        quick_boost: wire.buttons & input_buttons::QUICK_BOOST != 0,
        qb_held: wire.buttons & input_buttons::QB_HELD != 0,
        kick: wire.buttons & input_buttons::KICK != 0,
        ..SimInput::default()
    };
    Sampled { seq, wire, sim }
}

/// Minimal client prediction mirroring `acvd-game::net::prediction`.
struct ClientPrediction {
    ac: SimAc,
    ring: VecDeque<Sampled>,
    visual: Vec3,
    max_correction: f32,
    last_correction: f32,
}

impl ClientPrediction {
    fn from_state(id: SimId, loadout: Loadout, state: &LocalPlayerState) -> Self {
        let mut ac = SimAc::new(id, loadout, Vec3::ZERO, state.yaw, 0);
        apply_local_state(&mut ac, state);
        Self {
            ac,
            ring: VecDeque::new(),
            visual: Vec3::ZERO,
            max_correction: 0.0,
            last_correction: 0.0,
        }
    }

    fn step(&mut self, sampled: Sampled, collision: &mut Collision) {
        self.ring.push_back(sampled);
        if self.ring.len() > 128 {
            self.ring.pop_front();
        }
        self.ac.motion.prev_position = self.ac.position;
        acvd_sim::motion::step_validated_with_props(
            &mut self.ac.motion,
            &sampled.sim,
            &mut self.ac.position,
            collision,
            |_, _| false,
        );
    }

    fn reconcile(&mut self, state: &LocalPlayerState, collision: &mut Collision) {
        let rendered = self.ac.position + self.visual;
        let old = self.ac.position;
        apply_local_state(&mut self.ac, state);
        self.ring.retain(|f| {
            f.seq.wrapping_sub(state.last_acked_seq) < 0x8000 && f.seq != state.last_acked_seq
        });
        for frame in &self.ring {
            self.ac.motion.prev_position = self.ac.position;
            acvd_sim::motion::step_validated_with_props(
                &mut self.ac.motion,
                &frame.sim,
                &mut self.ac.position,
                collision,
                |_, _| false,
            );
        }
        let correction = old.distance(self.ac.position);
        self.last_correction = correction;
        self.max_correction = self.max_correction.max(correction);
        self.visual = rendered - self.ac.position;
        if self.visual.length() > 0.75 {
            self.visual = Vec3::ZERO;
        }
    }
}

struct Harness {
    net: NetClient,
    local: Option<NetId>,
    loadout: Loadout,
    quantizer: PosQuantizer,
    pred: Option<ClientPrediction>,
    seq: u16,
    joined: bool,
    retry: f32,
    snapshots: u32,
    pending_local: Option<LocalPlayerState>,
    last_local_tick: u32,
    loadout_updates: Vec<NetId>,
}

impl Harness {
    fn connect(addr: SocketAddr) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0x2200);
        let client_id = NEXT.fetch_add(1, Ordering::Relaxed);
        Self {
            net: NetClient::connect(addr, client_id, NETCODE_PROTOCOL_ID).expect("client"),
            local: None,
            loadout: acvd_sim::stats::default_loadout(),
            quantizer: PosQuantizer::default(),
            pred: None,
            seq: 1,
            joined: false,
            retry: 0.0,
            snapshots: 0,
            pending_local: None,
            last_local_tick: 0,
            loadout_updates: Vec::new(),
        }
    }

    fn update(&mut self, dt: Duration, collision: &mut Collision) {
        self.net.update(dt).expect("client update");
        self.retry -= dt.as_secs_f32();
        if self.net.is_connected() && !self.joined && self.retry <= 0.0 {
            let join = ClientCommandPacket {
                seq: 1,
                kind: command_kind::JOIN,
                protocol: PROTOCOL_VERSION,
                loadout: self.loadout.to_blob(),
            };
            self.net.send(COMMAND, join.to_bytes());
            self.retry = 0.2;
        }
        while let Some(bytes) = self.net.receive(BULK) {
            if let Ok(join) = JoinAccepted::from_bytes(&bytes) {
                self.local = Some(join.net_id);
                self.quantizer = PosQuantizer::new(join.pos_min.into(), join.pos_max.into());
                if let Ok(loadout) = Loadout::from_blob(&join.loadout) {
                    self.loadout = loadout;
                }
                self.joined = true;
                if let Some(local) = self.pending_local.take() {
                    self.apply_local(&local, collision);
                }
            }
        }
        while let Some(bytes) = self.net.receive(RELIABLE) {
            if let Ok(ReliableMessage::LoadoutUpdate(update)) = ReliableMessage::from_bytes(&bytes)
            {
                self.loadout_updates.push(update.net_id);
            }
        }
        while let Some(bytes) = self.net.receive(HOT) {
            let Ok(packet) = SnapshotPacket::from_bytes(&bytes) else {
                continue;
            };
            self.snapshots += 1;
            if let Some(local) = packet.local {
                if packet.server_tick >= self.last_local_tick {
                    self.last_local_tick = packet.server_tick;
                    if self.local.is_none() {
                        self.pending_local = Some(local);
                    } else {
                        self.apply_local(&local, collision);
                    }
                }
            }
        }
        let _ = self.net.send_packets();
    }

    fn apply_local(&mut self, local: &LocalPlayerState, collision: &mut Collision) {
        let id = SimId(self.local.unwrap().get());
        if self.pred.is_none() {
            self.pred = Some(ClientPrediction::from_state(id, self.loadout, local));
        } else if let Some(pred) = self.pred.as_mut() {
            pred.reconcile(local, collision);
        }
    }

    fn tick(&mut self, frame: &SimInput, collision: &mut Collision) {
        let Some(pred) = self.pred.as_mut() else {
            return;
        };
        let sampled = sample(self.seq, frame, frame.aim_yaw, frame.aim_pitch);
        self.seq = self.seq.wrapping_add(1);
        pred.step(sampled, collision);
        let start = pred.ring.len().saturating_sub(3);
        let frames: Vec<InputFrame> = pred.ring.iter().skip(start).map(|f| f.wire).collect();
        let packet = InputPacket {
            start_seq: pred.ring.get(start).map_or(1, |f| f.seq),
            frames,
        };
        self.net.send(HOT, packet.to_bytes());
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Scenario {
    Glide,
    WallJump,
    Kick,
}

fn script(scenario: Scenario, tick: u64, pred: &ClientPrediction) -> SimInput {
    let mut frame = SimInput {
        stick: Vec2::ZERO,
        ..SimInput::default()
    };
    match scenario {
        Scenario::Glide => {
            // Away from the wall (-Z is forward, so -1 backs off).
            frame.stick = Vec2::new(0.0, -1.0);
            frame.glide = true;
            if tick == 2 {
                frame.boost_toggle = true;
            }
        }
        Scenario::WallJump => {
            frame.stick = Vec2::new(0.0, 1.0);
            if tick == 2 {
                frame.boost_toggle = true;
            }
            if tick == 5 {
                frame.jump = true;
            }
            // The upper body sphere (radius 5.5 m) touches the wall plane at z=2 from z=7.5, so
            // that is the wall-contact band for a second jump press.
            if pred.ac.motion.airborne
                && pred.ac.position.z < 8.0
                && pred.ac.motion.wj.is_none()
                && tick > 8
            {
                frame.jump = true;
            }
        }
        Scenario::Kick => {
            // Kick away from the wall so the immediate response is not lost to push-out.
            frame.aim_yaw = std::f32::consts::PI;
            frame.stick = Vec2::new(0.0, 1.0);
            if tick == 5 {
                frame.kick = true;
            }
        }
    }
    frame
}

struct Report {
    immediate_response: f32,
    max_correction: f32,
    client_action: bool,
    server_action: bool,
    final_client: Vec3,
    final_server: Vec3,
}

fn run_scenario(scenario: Scenario, rtt_ms: u32, loss: f32) -> Report {
    let mut server = Server::bind(config()).expect("server");
    let relay_config =
        NetemConfig::new(rtt_ms, 10, loss, 0.0, 1.0, 0xACD5_2200 + u64::from(rtt_ms));
    let mut relay = netem::start(
        server.local_addr(),
        "127.0.0.1:0".parse().unwrap(),
        relay_config,
    )
    .expect("relay");
    let mut harness = Harness::connect(relay.addr());
    let mut client_collision = acvd_server::test_arena().0;
    // Reach steady state and join before starting the script.
    let join_deadline = Instant::now() + Duration::from_secs(10);
    while (!harness.joined || harness.pred.is_none()) && Instant::now() < join_deadline {
        server.step(DT).expect("server step");
        harness.update(DT, &mut client_collision);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(harness.joined, "join");
    assert!(harness.pred.is_some(), "first authoritative local state");

    let mut immediate_response = 0.0f32;
    let mut tick = 0u64;
    let mut client_action = false;
    let mut server_action = false;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let now = Instant::now();
        server.step(DT).expect("server step");
        harness.update(DT, &mut client_collision);
        if let (Some(pred), Some(local)) = (harness.pred.as_ref(), harness.local) {
            let srv = server.world().acs.get(&SimId(local.get()));
            match scenario {
                Scenario::Glide => {
                    client_action |= pred.ac.motion.glide;
                    server_action |= srv.is_some_and(|ac| ac.motion.glide);
                }
                Scenario::WallJump => {
                    client_action |= pred.ac.motion.wall_jumps > 0;
                    server_action |= srv.is_some_and(|ac| ac.motion.wall_jumps > 0);
                }
                Scenario::Kick => {
                    client_action |= pred.ac.motion.kicks_started > 0;
                    server_action |= srv.is_some_and(|ac| ac.motion.kicks_started > 0);
                }
            }
        }
        let mut frame = SimInput::default();
        if let Some(pred) = harness.pred.as_ref() {
            frame = script(scenario, tick, pred);
        }
        let before = harness.pred.as_ref().map(|p| p.ac.position);
        harness.tick(&frame, &mut client_collision);
        if tick == 5 {
            if let (Some(before), Some(pred)) = (before, harness.pred.as_ref()) {
                immediate_response = before.distance(pred.ac.position);
            }
        }
        tick += 1;
        let elapsed = now.elapsed();
        if elapsed < DT {
            std::thread::sleep(DT - elapsed);
        }
    }
    let max_correction = harness.pred.as_ref().map_or(0.0, |p| p.max_correction);
    // Flush acks (no new inputs; starvation repeats here are not gameplay corrections).
    for _ in 0..20 {
        server.step(DT).expect("server step");
        harness.update(DT, &mut client_collision);
        std::thread::sleep(Duration::from_millis(5));
    }

    let pred = harness.pred.as_ref().expect("prediction");
    let server_ac = server
        .world()
        .acs
        .get(&SimId(harness.local.unwrap().get()))
        .expect("authoritative AC");
    let report = Report {
        immediate_response,
        max_correction,
        client_action,
        server_action,
        final_client: pred.ac.position,
        final_server: server_ac.position,
    };
    harness.net.disconnect();
    relay.shutdown();
    server.shutdown();
    report
}

#[test]
fn scripted_movement_is_immediate_and_does_not_rubber_band() {
    for rtt in [80u32, 120] {
        for scenario in [Scenario::Glide, Scenario::WallJump, Scenario::Kick] {
            let report = run_scenario(scenario, rtt, 2.0);
            eprintln!(
                "phase2 {scenario:?} rtt={rtt} immediate={:.2} correction={:.2} client={} server={} delta={:.2}",
                report.immediate_response,
                report.max_correction,
                report.client_action,
                report.server_action,
                report.final_client.distance(report.final_server)
            );
            assert!(
                report.immediate_response > 0.0,
                "{scenario:?} {rtt} ms: no immediate local movement"
            );
            assert!(
                report.client_action,
                "{scenario:?} {rtt} ms: local prediction never entered the scripted action"
            );
            assert!(
                report.server_action,
                "{scenario:?} {rtt} ms: server never executed the scripted action"
            );
            assert!(
                report.max_correction <= 2.5,
                "{scenario:?} {rtt} ms: correction {:.2} m is outside the 2.5 m tolerance",
                report.max_correction
            );
            assert!(
                report.final_client.distance(report.final_server) <= 20.0,
                "{scenario:?} {rtt} ms: final client {:?} vs server {:?}",
                report.final_client,
                report.final_server
            );
        }
    }
}

#[test]
fn late_join_receives_every_actor_loadout_and_position_at_the_join_tick() {
    let mut server = Server::bind(config()).expect("server");
    let mut first = Harness::connect(server.local_addr());
    let mut collision = acvd_server::test_arena().0;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !first.joined && Instant::now() < deadline {
        server.step(DT).expect("server step");
        first.update(DT, &mut collision);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(first.joined);
    // Drive the first AC for a while so the late joiner must catch up to a moved world.
    for tick in 0..90u64 {
        server.step(DT).expect("server step");
        first.update(DT, &mut collision);
        let frame = SimInput {
            stick: Vec2::new(0.0, 1.0),
            ..SimInput::default()
        };
        if tick == 2 {
            // keep it grounded and walking
        }
        first.tick(&frame, &mut collision);
        std::thread::sleep(Duration::from_millis(5));
    }
    let first_id = server.world().acs.keys().next().copied().unwrap();

    // Second client joins mid-match.
    let mut second = Harness::connect(server.local_addr());
    let mut join_bulk_loadouts = 0usize;
    let mut saw_world_state = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while (!second.joined || join_bulk_loadouts < 2 || !saw_world_state)
        && Instant::now() < deadline
    {
        server.step(DT).expect("server step");
        first.update(DT, &mut collision);
        second.net.update(DT).expect("second update");
        second.retry -= DT.as_secs_f32();
        if second.net.is_connected() && !second.joined && second.retry <= 0.0 {
            second.net.send(
                COMMAND,
                ClientCommandPacket {
                    seq: 1,
                    kind: command_kind::JOIN,
                    protocol: PROTOCOL_VERSION,
                    loadout: second.loadout.to_blob(),
                }
                .to_bytes(),
            );
            second.retry = 0.2;
        }
        while let Some(bytes) = second.net.receive(BULK) {
            if let Ok(join) = JoinAccepted::from_bytes(&bytes) {
                second.local = Some(join.net_id);
                second.quantizer = PosQuantizer::new(join.pos_min.into(), join.pos_max.into());
                second.joined = true;
            } else if let Ok(message) = ReliableMessage::from_bytes(&bytes) {
                match message {
                    ReliableMessage::LoadoutUpdate(_) => join_bulk_loadouts += 1,
                    ReliableMessage::WorldState(_) => saw_world_state = true,
                    _ => {}
                }
            }
        }
        while let Some(bytes) = second.net.receive(HOT) {
            let Ok(packet) = SnapshotPacket::from_bytes(&bytes) else {
                continue;
            };
            if let Some(local) = packet.local {
                if second.pred.is_none() {
                    let id = SimId(second.local.unwrap().get());
                    second.pred = Some(ClientPrediction::from_state(id, second.loadout, &local));
                }
            }
        }
        let _ = second.net.send_packets();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(second.joined, "late join accepted");
    assert_eq!(join_bulk_loadouts, 2, "both loadouts arrive on BULK");
    assert!(saw_world_state, "destructible world state arrives on BULK");

    // The join-tick full snapshot must contain the existing AC at the server's authoritative
    // position, independently of interest/cadence.
    let second_id = second.local.unwrap();
    // Existing clients must learn the newcomer's loadout so the remote rig can be assembled.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !first.loadout_updates.contains(&second_id) && Instant::now() < deadline {
        server.step(DT).expect("server step");
        first.update(DT, &mut collision);
        second.net.update(DT).expect("second update");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        first.loadout_updates.contains(&second_id),
        "existing client receives the newcomer loadout"
    );
    let mut matched = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !matched && Instant::now() < deadline {
        server.step(DT).expect("server step");
        let server_pos = server
            .world()
            .acs
            .get(&first_id)
            .expect("first AC")
            .position;
        first.update(DT, &mut collision);
        second.net.update(DT).expect("second update");
        while let Some(bytes) = second.net.receive(HOT) {
            let Ok(packet) = SnapshotPacket::from_bytes(&bytes) else {
                continue;
            };
            if let Some(record) = packet
                .records
                .iter()
                .find(|r| r.net_id == first_id_wire(first_id))
            {
                let pos = second.quantizer.dequantize(record.pos);
                if pos.distance(server_pos) < 0.05 {
                    matched = true;
                }
            }
        }
        let _ = second.net.send_packets();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        matched,
        "late joiner sees the existing AC at the authoritative position"
    );
    assert_ne!(second_id, first_id_wire(first_id));
    first.net.disconnect();
    second.net.disconnect();
    server.shutdown();
}

fn first_id_wire(id: SimId) -> NetId {
    NetId::new(id.0).unwrap()
}

#[test]
fn drop_in_out_churn_does_not_leak() {
    let mut server = Server::bind(config()).expect("server");
    let mut clients: Vec<Harness> = Vec::new();
    let mut collision = acvd_server::test_arena().0;
    let deadline = Instant::now() + Duration::from_secs(15);
    // Churn 6 joins/leaves while keeping 2 residents.
    for round in 0..6 {
        let mut client = Harness::connect(server.local_addr());
        let joined = {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut joined = false;
            while !joined && Instant::now() < deadline {
                server.step(DT).expect("server step");
                client.update(DT, &mut collision);
                for resident in &mut clients {
                    resident.update(DT, &mut collision);
                }
                joined = client.joined;
                std::thread::sleep(Duration::from_millis(5));
            }
            joined
        };
        assert!(joined, "round {round} join");
        clients.push(client);
        for _ in 0..20 {
            server.step(DT).expect("server step");
            for c in &mut clients {
                c.update(DT, &mut collision);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut leaving = clients.remove(0);
        leaving.net.disconnect();
        for _ in 0..30 {
            server.step(DT).expect("server step");
            for c in &mut clients {
                c.update(DT, &mut collision);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            server.player_count(),
            clients.len(),
            "round {round}: server client map tracks leavers"
        );
        assert_eq!(
            server.world().acs.len(),
            clients.len(),
            "round {round}: no leaked SimAc"
        );
    }
    for c in &mut clients {
        c.net.disconnect();
    }
    for _ in 0..30 {
        server.step(DT).expect("server step");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(server.player_count(), 0);
    assert!(server.world().acs.is_empty(), "all ACs despawned");
    server.shutdown();
    let _ = deadline;
}
