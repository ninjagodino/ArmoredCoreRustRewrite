//! Socket-free renet `new_local_client` path with the project's real channel layout.

use acvd_net::{LocalPair, ServerEvent, BULK, COMMAND, HOT};
use acvd_proto::*;

fn pump(pair: &mut LocalPair) {
    for _ in 0..5 {
        pair.update();
    }
}

#[test]
fn new_local_client_join_loadout_snapshot_and_disconnect() {
    let client_id = 7;
    let mut pair = LocalPair::new(client_id);
    assert!(matches!(
        pair.server_event(),
        Some(ServerEvent::ClientConnected { client_id: 7 })
    ));
    assert_eq!(pair.server_connected_clients(), 1);

    let command = ClientCommandPacket {
        seq: 1,
        kind: command_kind::JOIN,
        protocol: PROTOCOL_VERSION,
        loadout: vec![1, 2, 3, 4],
    };
    pair.client_send(COMMAND, command.to_bytes());
    pump(&mut pair);
    let bytes = pair
        .server_receive(COMMAND)
        .expect("server receives the join command");
    assert_eq!(ClientCommandPacket::from_bytes(&bytes).unwrap(), command);

    let join = JoinAccepted {
        protocol: PROTOCOL_VERSION,
        net_id: NetId::new(1).unwrap(),
        server_tick: 42,
        map_id: 4000,
        pos_min: [-800.0, -50.0, -800.0],
        pos_max: [800.0, 100.0, 800.0],
        loadout: vec![1, 2, 3, 4],
    };
    pair.server_send(BULK, join.to_bytes());
    pump(&mut pair);
    let bytes = pair
        .client_receive(BULK)
        .expect("client receives join accepted");
    assert_eq!(JoinAccepted::from_bytes(&bytes).unwrap(), join);

    let snapshot = SnapshotPacket {
        server_tick: 42,
        snapshot_seq: 1,
        acked_input_seq: 0,
        flags: 0,
        local: Some(LocalPlayerState::default()),
        records: vec![],
        corrections: vec![],
    };
    pair.server_send(HOT, snapshot.try_encode().unwrap());
    pump(&mut pair);
    let bytes = pair
        .client_receive(HOT)
        .expect("client receives a hot snapshot");
    assert_eq!(SnapshotPacket::from_bytes(&bytes).unwrap(), snapshot);

    pair.disconnect();
    assert!(matches!(
        pair.server_event(),
        Some(ServerEvent::ClientDisconnected { client_id: 7, .. })
    ));
    assert_eq!(pair.server_connected_clients(), 0);
}
