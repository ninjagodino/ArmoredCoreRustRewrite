//! Phase 3 gate tests: rewind validation, fast/slow projectile authority, channel discipline,
//! replicated bay swaps, destructible late-join state, and the kill -> respawn loop.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use acvd_net::{NetClient, BULK, COMBAT, COMMAND, HOT, RELIABLE};
use acvd_proto::*;
use acvd_server::lagcomp::{self, HitboxHistory, HitscanOutcome};
use acvd_server::{Server, ServerConfig};
use acvd_sim::collision::Collision;
use acvd_sim::damage::{self, DamageType};
use acvd_sim::ids::{NetId as SimNetId, PropId};
use acvd_sim::map::{Destructible, DestructibleSet};
use acvd_sim::stats;
use acvd_sim::world::{apply_world_state, SimAc, SimWorld};
use glam::Vec3;

const DT: Duration = Duration::from_millis(2);

fn config() -> ServerConfig {
    ServerConfig {
        bind: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        max_clients: 8,
        allow_missing_disc: true,
        snapshot_every: 2,
        ..ServerConfig::default()
    }
}

fn join_command(seq: u16) -> ClientCommandPacket {
    ClientCommandPacket {
        seq,
        kind: command_kind::JOIN,
        protocol: PROTOCOL_VERSION,
        loadout: stats::default_loadout().to_blob(),
    }
}

fn client_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0x3300);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn join(server: &mut Server, clients: &mut [NetClient]) -> Vec<JoinAccepted> {
    let mut accepted: Vec<Option<JoinAccepted>> = vec![None; clients.len()];
    let deadline = Instant::now() + Duration::from_secs(15);
    while accepted.iter().any(Option::is_none) && Instant::now() < deadline {
        server.step(DT).expect("server step");
        for (i, client) in clients.iter_mut().enumerate() {
            if client.is_disconnected() {
                continue;
            }
            client.update(DT).expect("client update");
            if client.is_connected() {
                client.send(COMMAND, join_command(1).to_bytes());
            }
            let _ = client.send_packets();
            while let Some(bytes) = client.receive(BULK) {
                if let Ok(join) = JoinAccepted::from_bytes(&bytes) {
                    accepted[i] = Some(join);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    accepted
        .into_iter()
        .map(|a| a.expect("client joined"))
        .collect()
}

#[test]
fn sniper_rewind_at_120ms_applies_exact_ke_ce_te_damage() {
    let loadout = stats::default_loadout();
    let power = [1200.0_f32, 900.0, 1500.0];
    let attrs = [DamageType::Ke, DamageType::Ce, DamageType::Te];
    for (attr, power) in attrs.into_iter().zip(power) {
        let mut world = SimWorld::new();
        let mut shooter = SimAc::new(SimNetId(1), loadout, Vec3::ZERO, 0.0, 0);
        shooter.motion.yaw = 0.0;
        world.insert(shooter);
        // Current target is 5 m to the right; at the 120 ms shooter view tick it was dead ahead.
        let target = SimAc::new(SimNetId(2), loadout, Vec3::new(5.0, 0.0, -20.0), 0.0, 1);
        world.insert(target);
        world.tick = acvd_sim::tick::Tick(100);
        let mut history = HitboxHistory::new();
        history.push(SimNetId(1), 92, Vec3::ZERO, 0.0);
        history.push(SimNetId(2), 92, Vec3::ZERO, 0.0);
        history.push(SimNetId(2), 100, Vec3::new(5.0, 0.0, -20.0), 0.0);
        let collision = Collision::default();
        let rewind = lagcomp::rewind_ticks(120.0, 0.0);
        assert_eq!(rewind, lagcomp::INTERP_DELAY_TICKS + 4);
        let outcome = lagcomp::resolve_hitscan(
            &history,
            &world,
            &collision,
            SimNetId(1),
            Vec3::new(0.0, 3.0, 0.0),
            Vec3::new(0.0, 0.0, -1.0),
            200.0,
            100,
            rewind,
        );
        let HitscanOutcome::Achit { target, position } = outcome else {
            panic!("expected a rewound hit, got {outcome:?}");
        };
        assert_eq!(target, SimNetId(2));
        let expected = damage::calculate_damage(
            attr,
            power,
            50.0,
            &world.acs[&SimNetId(2)].stats.defense,
            false,
        );
        let mut events = Vec::new();
        let applied = world
            .apply_hit(
                target,
                SimNetId(1),
                attr,
                power,
                50.0,
                position,
                world.tick,
                &mut events,
            )
            .expect("alive target");
        assert_eq!(applied.amount, expected.damage, "{attr:?} exact damage");
        assert!(
            (world.acs[&SimNetId(2)].ap - (world.acs[&SimNetId(2)].max_ap - expected.damage)).abs()
                < 1e-3
        );
    }
}

#[test]
fn hot_channel_carries_only_fire_and_snapshots() {
    let mut server = Server::bind(config()).expect("server");
    let addr = server.local_addr();
    let mut clients = vec![
        NetClient::connect(addr, client_id(), NETCODE_PROTOCOL_ID).expect("client A"),
        NetClient::connect(addr, client_id(), NETCODE_PROTOCOL_ID).expect("client B"),
    ];
    let joined = join(&mut server, &mut clients);
    let a = joined[0].net_id;
    let b = joined[1].net_id;
    let sim_a = SimNetId(a.get());
    if let Some(ac) = server.world_mut().acs.get_mut(&sim_a) {
        ac.position = Vec3::ZERO;
        ac.motion.yaw = 0.0;
    }
    let sim_b = SimNetId(b.get());
    if let Some(ac) = server.world_mut().acs.get_mut(&sim_b) {
        ac.position = Vec3::new(0.0, 0.0, -40.0);
        ac.motion.yaw = 0.0;
    }

    let mut fires: HashMap<u32, u32> = HashMap::new();
    let mut snapshots = 0u32;
    for seq in 1..=90u16 {
        server.step(DT).expect("server step");
        for (i, client) in clients.iter_mut().enumerate() {
            client.update(DT).expect("client update");
            if i == 0 && client.is_connected() {
                let packet = InputPacket {
                    start_seq: seq,
                    frames: vec![InputFrame {
                        buttons: input_buttons::FIRE_R,
                        stick: [0, 0],
                        // Aim away from B so this test only exercises HOT content.
                        aim_yaw: quantize_yaw(1.0),
                        aim_pitch: 0,
                        lock_target: 0,
                    }],
                };
                client.send(HOT, packet.to_bytes());
            }
            let _ = client.send_packets();
        }
        while let Some(bytes) = clients[1].receive(HOT) {
            let message = HotMessage::from_bytes(&bytes).expect("every HOT payload is tagged");
            match message {
                HotMessage::Snapshot(_) => snapshots += 1,
                HotMessage::Fire(fire) => {
                    *fires.entry(fire.projectile_id).or_default() += 1;
                }
            }
        }
        while clients[1].receive(COMBAT).is_some() {}
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(snapshots > 0, "snapshots still flow on HOT");
    assert!(!fires.is_empty(), "the server broadcasts FireEvents");
    assert!(
        fires.values().all(|count| *count <= 3),
        "each FireEvent is sent once plus two repeat ticks: {fires:?}"
    );
    assert_eq!(
        fires.len() as u64,
        server.hot_fire_events_sent(),
        "HOT has one shot event per bullet id plus deduped repeats only"
    );
}

#[test]
fn bay_rack_switch_replicates_between_clients() {
    let mut server = Server::bind(config()).expect("server");
    let addr = server.local_addr();
    let mut clients = vec![
        NetClient::connect(addr, client_id(), NETCODE_PROTOCOL_ID).expect("client A"),
        NetClient::connect(addr, client_id(), NETCODE_PROTOCOL_ID).expect("client B"),
    ];
    let joined = join(&mut server, &mut clients);
    let a = joined[0].net_id;
    let b = joined[1].net_id;
    let sim_a = SimNetId(a.get());
    if let Some(ac) = server.world_mut().acs.get_mut(&sim_a) {
        ac.position = Vec3::ZERO;
        ac.motion.yaw = 0.0;
    }
    let sim_b = SimNetId(b.get());
    if let Some(ac) = server.world_mut().acs.get_mut(&sim_b) {
        ac.position = Vec3::new(0.0, 0.0, -4.0);
        ac.motion.yaw = 0.0;
    }

    let mut saw_swapping = false;
    let mut saw_active = false;
    for seq in 1..=230u16 {
        server.step(DT).expect("server step");
        for (i, client) in clients.iter_mut().enumerate() {
            client.update(DT).expect("client update");
            if client.is_connected() {
                let mut buttons = 0;
                if i == 0 && seq == 1 {
                    buttons |= input_buttons::BAY_L;
                }
                let lock = if i == 1 { a.get() } else { b.get() };
                let packet = InputPacket {
                    start_seq: seq,
                    frames: vec![InputFrame {
                        buttons,
                        stick: [0, 0],
                        aim_yaw: quantize_yaw(0.0),
                        aim_pitch: 0,
                        lock_target: lock,
                    }],
                };
                client.send(HOT, packet.to_bytes());
            }
            let _ = client.send_packets();
        }
        while let Some(bytes) = clients[1].receive(HOT) {
            let Ok(HotMessage::Snapshot(packet)) = HotMessage::from_bytes(&bytes) else {
                continue;
            };
            for record in &packet.records {
                if record.net_id == a {
                    saw_swapping |= record.combat_flags & combat_flags::LEFT_SWAPPING != 0;
                    saw_active |= record.combat_flags & combat_flags::LEFT_BAY_ACTIVE != 0;
                }
            }
        }
        while clients[1].receive(COMBAT).is_some() {}
        if saw_active {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(saw_swapping, "the 150-tick swap state is replicated");
    assert!(saw_active, "the flipped rack weapon state is replicated");
}

#[test]
fn early_and_late_joiners_share_the_destructible_bitset() {
    let mut server = Server::bind(config()).expect("server");
    let addr = server.local_addr();
    let mut a = NetClient::connect(addr, client_id(), NETCODE_PROTOCOL_ID).expect("client A");
    join(&mut server, std::slice::from_mut(&mut a));

    let prop = Destructible {
        prop_id: PropId(1),
        model: "phase3-test".into(),
        center: Vec3::ZERO,
        radius: 1.0,
        hp: 10.0,
        max_hp: 10.0,
        cutoff_damage: 0.0,
        collision_damage_mult: 0.0,
        damage_id: 0,
        broken: false,
        just_broken: false,
    };
    server.world_mut().destructibles = DestructibleSet::new(vec![prop.clone()]);
    assert!(server
        .world_mut()
        .destructibles
        .apply_damage(PropId(1), 100.0));

    let mut b = NetClient::connect(addr, client_id(), NETCODE_PROTOCOL_ID).expect("client B");
    let mut bulk_world: Option<WorldState> = None;
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut accepted = false;
    while (!accepted || bulk_world.is_none()) && Instant::now() < deadline {
        server.step(DT).expect("server step");
        a.update(DT).expect("A update");
        b.update(DT).expect("B update");
        if b.is_connected() {
            b.send(COMMAND, join_command(1).to_bytes());
        }
        let _ = a.send_packets();
        let _ = b.send_packets();
        while a.receive(BULK).is_some() {}
        while let Some(bytes) = b.receive(BULK) {
            if JoinAccepted::from_bytes(&bytes).is_ok() {
                accepted = true;
            } else if let Ok(ReliableMessage::WorldState(world)) =
                ReliableMessage::from_bytes(&bytes)
            {
                bulk_world = Some(world);
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let bulk = bulk_world.expect("late join receives the BULK world state");
    assert!(bulk.is_broken(0));
    let mut late = DestructibleSet::new(vec![prop]);
    apply_world_state(&mut late, &bulk);
    assert_eq!(
        late.broken_bits(),
        server.world().destructibles.broken_bits(),
        "early and late joiners share the exact broken bitset"
    );
    while a.receive(RELIABLE).is_some() {}
    while b.receive(RELIABLE).is_some() {}
}

#[test]
fn kill_then_deterministic_respawn_restores_active_combat() {
    let mut config = config();
    config.respawn_ticks = 20;
    let mut server = Server::bind(config).expect("server");
    let addr = server.local_addr();
    let mut clients = vec![
        NetClient::connect(addr, client_id(), NETCODE_PROTOCOL_ID).expect("client A"),
        NetClient::connect(addr, client_id(), NETCODE_PROTOCOL_ID).expect("client B"),
    ];
    let joined = join(&mut server, &mut clients);
    let a = joined[0].net_id;
    let b = joined[1].net_id;
    let sim_a = SimNetId(a.get());
    let sim_b = SimNetId(b.get());
    if let Some(ac) = server.world_mut().acs.get_mut(&sim_a) {
        ac.position = Vec3::ZERO;
        ac.motion.yaw = 0.0;
    }
    if let Some(ac) = server.world_mut().acs.get_mut(&sim_b) {
        ac.position = Vec3::new(0.0, 0.0, -25.0);
        ac.motion.yaw = 0.0;
    }
    // Fill hitbox history at the repositioned spot before firing.
    for _ in 0..10 {
        server.step(DT).expect("server step");
        for client in &mut clients {
            client.update(DT).expect("client update");
            let _ = client.send_packets();
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    if let Some(ac) = server.world_mut().acs.get_mut(&sim_b) {
        ac.ap = 1.0;
    }
    let mut saw_kill = false;
    let mut saw_drop = false;
    for seq in 1..=320u16 {
        server.step(DT).expect("server step");
        for (i, client) in clients.iter_mut().enumerate() {
            client.update(DT).expect("client update");
            if i == 0 && client.is_connected() {
                let packet = InputPacket {
                    start_seq: seq,
                    frames: vec![InputFrame {
                        buttons: input_buttons::FIRE_R,
                        stick: [0, 0],
                        aim_yaw: quantize_yaw(0.0),
                        aim_pitch: 0,
                        lock_target: b.get(),
                    }],
                };
                client.send(HOT, packet.to_bytes());
            }
            let _ = client.send_packets();
        }
        for client in &mut clients {
            while let Some(bytes) = client.receive(RELIABLE) {
                if let Ok(ReliableMessage::KillEvent(kill)) = ReliableMessage::from_bytes(&bytes) {
                    if kill.victim == b && kill.killer == a {
                        saw_kill = true;
                    }
                }
                if let Ok(ReliableMessage::RespawnDrop(drop)) = ReliableMessage::from_bytes(&bytes)
                {
                    if drop.net_id == b {
                        saw_drop = true;
                        assert!(drop.start_tick > 0);
                    }
                }
            }
            while client.receive(COMBAT).is_some() {}
            while client.receive(HOT).is_some() {}
        }
        if saw_drop {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        saw_kill,
        "the server authoritatively kills the low-AP target"
    );
    assert!(saw_drop, "the killed AC gets a deterministic RespawnDrop");
    assert!(server.kill_count() >= 1);
    assert!(server.respawn_count() >= 1);
    let b_ac = server.world().acs.get(&sim_b).expect("respawned target");
    assert!(b_ac.alive, "respawn reactivates the AC");
    assert!((b_ac.ap - b_ac.max_ap).abs() < 1e-3);
}
