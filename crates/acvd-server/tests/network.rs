//! Network gates: loopback two-client exchange, netem (120 ms + 2% loss), and late-join bulk
//! state. Phase 2 movement/loss gates live in `phase2.rs`.

use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use acvd_net::netem::{self, NetemConfig};
use acvd_net::{NetClient, BULK, COMMAND, HOT, RELIABLE};
use acvd_proto::*;
use acvd_server::{Server, ServerConfig};

const DT_FALLBACK: Duration = Duration::from_millis(1);

fn test_config() -> ServerConfig {
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
        loadout: acvd_sim::stats::default_loadout().to_blob(),
    }
}

fn pump(
    server: &mut Server,
    clients: &mut [NetClient],
    iterations: usize,
    mut action: impl FnMut(usize, &mut NetClient),
) {
    let mut last = Instant::now();
    for _ in 0..iterations {
        let now = Instant::now();
        let dt = now.duration_since(last).max(DT_FALLBACK);
        last = now;

        server.step(dt).expect("server step");
        for (i, client) in clients.iter_mut().enumerate() {
            if client.is_disconnected() {
                continue;
            }
            client.update(dt).expect("client update");
            action(i, client);
            client.send_packets().expect("client flush");
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn loopback_two_clients_join_exchange_and_disconnect() {
    let mut server = Server::bind(test_config()).expect("bind server");
    let addr = server.local_addr();
    let mut clients = vec![
        NetClient::connect(addr, 101, NETCODE_PROTOCOL_ID).expect("client A"),
        NetClient::connect(addr, 202, NETCODE_PROTOCOL_ID).expect("client B"),
    ];

    let mut accepted: [Option<JoinAccepted>; 2] = [None, None];
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = Instant::now();
    while accepted.iter().any(Option::is_none) && Instant::now() < deadline {
        let now = Instant::now();
        let dt = now.duration_since(last).max(DT_FALLBACK);
        last = now;
        server.step(dt).expect("server step");
        for (i, client) in clients.iter_mut().enumerate() {
            if client.is_disconnected() {
                continue;
            }
            client.update(dt).expect("client update");
            if client.is_connected() {
                client.send(COMMAND, join_command(1).to_bytes());
                let _ = client.send_packets();
            }
            while let Some(bytes) = client.receive(BULK) {
                if let Ok(join) = JoinAccepted::from_bytes(&bytes) {
                    accepted[i] = Some(join);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    let a = accepted[0].as_ref().expect("client A joined");
    let b = accepted[1].as_ref().expect("client B joined");
    assert_eq!(a.protocol, PROTOCOL_VERSION);
    assert_eq!(b.protocol, PROTOCOL_VERSION);
    assert_ne!(a.net_id, b.net_id);
    assert_eq!(
        Loadout::from_blob(&a.loadout).unwrap(),
        acvd_sim::stats::default_loadout()
    );
    assert_eq!(
        Loadout::from_blob(&b.loadout).unwrap(),
        acvd_sim::stats::default_loadout()
    );

    // Client A sends inputs until its local state and the other AC's record arrive.
    let mut saw_local = false;
    let mut saw_b = false;
    let input = InputPacket {
        start_seq: 1,
        frames: vec![InputFrame {
            buttons: 0,
            stick: [100, 0],
            aim_yaw: quantize_yaw(0.5),
            aim_pitch: 0,
            lock_target: b.net_id.get(),
        }],
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = Instant::now();
    let mut seq = 1u16;
    while (!saw_local || !saw_b) && Instant::now() < deadline {
        let now = Instant::now();
        let dt = now.duration_since(last).max(DT_FALLBACK);
        last = now;
        server.step(dt).expect("server step");
        for (i, client) in clients.iter_mut().enumerate() {
            if client.is_disconnected() {
                continue;
            }
            client.update(dt).expect("client update");
            if i == 0 {
                let mut packet = input.clone();
                packet.start_seq = seq;
                seq = seq.wrapping_add(1);
                client.send(HOT, packet.to_bytes());
            }
            let _ = client.send_packets();
            if i == 0 {
                while let Some(bytes) = client.receive(HOT) {
                    let snapshot = SnapshotPacket::from_bytes(&bytes).expect("snapshot");
                    if snapshot.local.is_some() {
                        saw_local = true;
                    }
                    for record in &snapshot.records {
                        if record.net_id == b.net_id {
                            saw_b = true;
                        }
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(saw_local, "local authoritative state in snapshots");
    assert!(saw_b, "the other AC record arrives");

    // Client B leaves; server despawns it and A gets a reliable PlayerLeft.
    clients[1].disconnect();
    let mut saw_left = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = Instant::now();
    while (!saw_left || server.player_count() != 1) && Instant::now() < deadline {
        let now = Instant::now();
        let dt = now.duration_since(last).max(DT_FALLBACK);
        last = now;
        server.step(dt).expect("server step");
        for client in clients.iter_mut().take(1) {
            client.update(dt).expect("client update");
            client.send_packets().expect("client flush");
            while let Some(bytes) = client.receive(RELIABLE) {
                if let ReliableMessage::PlayerLeft(left) =
                    ReliableMessage::from_bytes(&bytes).expect("reliable message")
                {
                    if left.net_id == b.net_id {
                        saw_left = true;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(saw_left, "client A receives PlayerLeft");
    assert_eq!(server.player_count(), 1, "server despawns the leaving AC");
    assert_eq!(server.world().acs.len(), 1);
    server.shutdown();
}

#[test]
fn netem_120ms_2pct_loss_reliable_join_and_metrics() {
    let mut server = Server::bind(test_config()).expect("bind server");
    let relay_config = NetemConfig::new(120, 30, 2.0, 0.0, 0.0, 0xACD5_EED);
    let mut relay = netem::start(
        server.local_addr(),
        "127.0.0.1:0".parse().unwrap(),
        relay_config,
    )
    .expect("start netem");
    let mut client =
        NetClient::connect(relay.addr(), 77, NETCODE_PROTOCOL_ID).expect("netem client");

    // Joining crosses reliable bulk messages under real loss; resends must complete it.
    let mut accepted: Option<JoinAccepted> = None;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut last = Instant::now();
    while accepted.is_none() && Instant::now() < deadline {
        let now = Instant::now();
        let dt = now.duration_since(last).max(DT_FALLBACK);
        last = now;
        server.step(dt).expect("server step");
        client.update(dt).expect("client update");
        if client.is_connected() {
            client.send(COMMAND, join_command(1).to_bytes());
        }
        let _ = client.send_packets();
        while let Some(bytes) = client.receive(BULK) {
            if let Ok(join) = JoinAccepted::from_bytes(&bytes) {
                accepted = Some(join);
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let accepted = accepted.expect("reliable join completes over 120 ms + 2% loss");
    assert_eq!(
        Loadout::from_blob(&accepted.loadout).unwrap(),
        acvd_sim::stats::default_loadout()
    );
    assert_eq!(accepted.protocol, PROTOCOL_VERSION);

    // Drive inputs/snapshots long enough for renet's metric window and the seeded losses.
    let mut last = Instant::now();
    for seq in 1..=1200u16 {
        let now = Instant::now();
        let dt = now.duration_since(last).max(DT_FALLBACK);
        last = now;
        server.step(dt).expect("server step");
        client.update(dt).expect("client update");
        let packet = InputPacket {
            start_seq: seq,
            frames: vec![InputFrame {
                buttons: 0,
                stick: [60, 0],
                aim_yaw: quantize_yaw(0.25),
                aim_pitch: 0,
                lock_target: 0,
            }],
        };
        client.send(HOT, packet.to_bytes());
        client.send_packets().expect("client flush");
        while client.receive(HOT).is_some() {}
        std::thread::sleep(Duration::from_millis(1));
    }

    // Let acks/metrics settle.
    pump(
        &mut server,
        std::slice::from_mut(&mut client),
        200,
        |_, _| {},
    );

    let metrics = {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = server.metrics();
            if let Some((_, m)) = snapshot.first() {
                if m.rtt_ms > 40.0 {
                    break *m;
                }
            }
            assert!(Instant::now() < deadline, "server RTT metric never rose");
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    assert!(metrics.rtt_ms > 40.0, "rtt {:.1} ms", metrics.rtt_ms);
    assert!(metrics.packet_loss.is_finite() && metrics.packet_loss >= 0.0);

    let client_metrics = client.metrics();
    assert!(
        client_metrics.rtt_ms > 40.0,
        "client rtt {:.1} ms",
        client_metrics.rtt_ms
    );
    assert!(client_metrics.packet_loss.is_finite());

    let stats = relay.stats();
    assert!(stats.forwarded > 1000, "forwarded {}", stats.forwarded);
    assert!(stats.dropped > 0, "seeded 2% loss must drop packets");
    assert_eq!(server.player_count(), 1);

    relay.shutdown();
    let mut client = client;
    client.disconnect();
    server.shutdown();
}

#[test]
fn run_exits_on_shutdown_flag() {
    let shutdown = Arc::new(AtomicBool::new(true));
    acvd_server::run(test_config(), shutdown).expect("run exits when shutdown is already set");
}

#[test]
fn missing_disc_fails_fast() {
    let mut config = test_config();
    config.allow_missing_disc = false;
    config.disc = Some("private/tmp/acvd-phase1-no-such-disc.pe".into());
    let err = match Server::bind(config) {
        Ok(_) => panic!("a missing disc must fail startup"),
        Err(err) => err,
    };
    let text = format!("{err:#}");
    assert!(text.contains("game disc not found"), "{text}");
}

#[test]
fn empty_map_fails_fast() {
    let mut config = test_config();
    config.allow_missing_disc = false;
    config.map.clear();
    config.disc = Some("anything".into());
    let err = match Server::bind(config) {
        Ok(_) => panic!("an empty map must fail startup"),
        Err(err) => err,
    };
    assert!(format!("{err:#}").contains("no map"), "{err:#}");
}
