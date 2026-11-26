//! Golden wire sizes and round trips. These pin the protocol contract used by the bandwidth
//! budget in the netcode plan.

use acvd_proto::*;

fn record(net_id: u8) -> AcRecord {
    AcRecord {
        net_id: NetId::new(net_id).unwrap(),
        pos: [1000, 2000, 3000],
        vel: [-10, 20, -30],
        yaw: 42_000,
        pitch: -12,
        flags: ac_flags::BOOST | ac_flags::TEAM_B,
        pose_state: 2,
        combat_flags: combat_flags::LEFT_BAY_ACTIVE | combat_flags::RIGHT_SWAPPING,
        ap: 500,
        en: 200,
        reload: [1, 2],
        ammo: [30, 7],
    }
}

fn local_state() -> LocalPlayerState {
    LocalPlayerState {
        pos: [10.0, 20.0, 30.0],
        vel: [-1.0, 0.0, 2.5],
        yaw: 1.25,
        pitch: -0.1,
        aim_yaw: 1.35,
        flags: ac_flags::AIRBORNE,
        en: 1000.5,
        en_max: 1200.0,
        ap: 0.75,
        last_acked_seq: 65530,
        server_tick: 1234,
        combat_flags: combat_flags::LEFT_BAY_ACTIVE
            | combat_flags::RIGHT_SWAPPING
            | combat_flags::GLIDE_QB,
        swap_ticks: [150, 40],
        hand_ammo: [24, 30],
        bay_ammo: [8, 0],
        shoulder_ammo: 6,
        hand_cooldown: [0.0, 3.5],
        bay_cooldown: [7.0, 0.0],
        shoulder_cooldown: 2.0,
        hand_charge: [0.0, 12.0],
        bay_charge: [0.0, 0.0],
        shoulder_charge: 4.0,
        burst_remaining: [0, 2],
        burst_timer: [0.0, 5.0],
        qb_timer: 0.1,
        wj_timer: 0.2,
        kick_timer: 0.3,
        blade_timer: 0.4,
        stagger_timer: 0.5,
        en_regen_delay: 0.6,
        glide_time: 0.7,
        glide_lift_left: 0.8,
        glide_activation_y: 0.9,
        takeoff: 1.0,
        qb_cooldown: 1.1,
        wj_lockout: 1.2,
        ow_dash_ticks: 1.3,
        qb_weight: 0.9,
        qb_p1_ticks: 2.0,
        qb_p2_ticks: 3.0,
        qb_charge_held: 4.0,
        qb_dir: [10, -20, 30],
        wj_dir: [40, 50, 60],
        wj_normal: [70, 80, 90],
        kick_dir: [1, 2, 3],
        blade_target: [100, 200, 300],
        blade_max_dash_ticks: 5.0,
        blade_entry_speed: 6.0,
        qb_phase: 2,
        wj_phase: 0x12,
        blade_phase: 3,
        kick_hit: 1,
        glide_phase: 1,
        glide_flags: 3,
        blade_flags: 1,
        pose_state: 9,
    }
}

#[test]
fn input_frame_is_nine_bytes() {
    let frame = InputFrame {
        buttons: 0xabcd,
        stick: [-100, 100],
        aim_yaw: 30_000,
        aim_pitch: -1234,
        lock_target: 7,
    };
    let bytes = frame.to_bytes();
    assert_eq!(bytes.len(), 9);
    assert_eq!(bytes.len(), InputFrame::WIRE_BYTES);
    assert_eq!(InputFrame::from_bytes(&bytes).unwrap(), frame);

    let packet = InputPacket {
        start_seq: 60_000,
        frames: vec![frame; 3],
    };
    let bytes = packet.to_bytes();
    assert_eq!(bytes.len(), 2 + 1 + 3 * 9);
    assert_eq!(InputPacket::from_bytes(&bytes).unwrap(), packet);
}

#[test]
fn ac_record_is_at_most_32_bytes() {
    let rec = record(42);
    let bytes = rec.to_bytes();
    assert_eq!(bytes.len(), AC_RECORD_BYTES);
    assert!(bytes.len() <= AcRecord::MAX_WIRE_BYTES);
    assert!(bytes.len() < AcRecord::HARD_CEILING);
    assert_eq!(AcRecord::from_bytes(&bytes).unwrap(), rec);
}

#[test]
fn net_id_is_1_to_254() {
    for id in 1..=254u8 {
        assert_eq!(NetId::new(id).unwrap().get(), id);
    }
    assert!(NetId::new(0).is_err());
    assert!(NetId::new(255).is_err());
    // `from_wire` allows 0 for optional hints but still rejects the reserved value.
    assert!(NetId::from_wire(0).unwrap().is_none());
    assert!(NetId::from_wire(255).is_err());
}

#[test]
fn snapshot_packet_fits_one_datagram() {
    let packet = SnapshotPacket {
        server_tick: 123,
        snapshot_seq: 9,
        acked_input_seq: 8,
        flags: 0,
        local: Some(local_state()),
        records: (1..=20).map(record).collect(),
        corrections: vec![MissileCorrection {
            projectile_id: 1,
            pos: [1, 2, 3],
            vel: [4, 5, 6],
            target: NetId::new(2).unwrap(),
        }],
    };
    let bytes = packet.try_encode().unwrap();
    assert!(bytes.len() <= MAX_SNAPSHOT_BYTES);
    assert_eq!(SnapshotPacket::from_bytes(&bytes).unwrap(), packet);
}

#[test]
fn snapshot_split_respects_the_two_packet_cap() {
    let packet = SnapshotPacket {
        server_tick: 500,
        snapshot_seq: 3,
        acked_input_seq: 2,
        flags: snapshot_flags::HAS_LOCAL,
        local: Some(local_state()),
        records: (1..=64).map(record).collect(),
        corrections: Vec::new(),
    };
    let split = packet.split_for_udp();
    assert!(split.packets.len() <= 2);
    assert_eq!(split.dropped_records, 0);
    assert_eq!(split.dropped_corrections, 0);
    assert_eq!(
        split.packets.iter().map(|p| p.records.len()).sum::<usize>(),
        64
    );
    for (i, pkt) in split.packets.iter().enumerate() {
        assert!(pkt.wire_len() <= MAX_SNAPSHOT_BYTES, "packet {i} too large");
        assert_eq!(
            pkt.local.is_some(),
            i == 0,
            "local state must stay in packet 1"
        );
        // Every packet must actually encode and decode on its own.
        let bytes = pkt.try_encode().unwrap();
        assert_eq!(SnapshotPacket::from_bytes(&bytes).unwrap(), *pkt);
    }

    let packet = SnapshotPacket {
        records: (1..=100).map(record).collect(),
        local: Some(local_state()),
        ..packet
    };
    let split = packet.split_for_udp();
    assert_eq!(split.packets.len(), 2);
    assert!(split.dropped_records > 0);
    assert_eq!(
        split.packets.iter().map(|p| p.records.len()).sum::<usize>() + split.dropped_records,
        100
    );
    assert!(split
        .packets
        .iter()
        .all(|p| p.wire_len() <= MAX_SNAPSHOT_BYTES));

    let oversized = SnapshotPacket {
        records: (1..=100).map(record).collect(),
        local: Some(local_state()),
        ..Default::default()
    };
    assert!(matches!(
        oversized.try_encode(),
        Err(ProtoError::TooLarge { .. })
    ));
}

#[test]
fn loadout_is_compact_and_roundtrips() {
    let mut parts = [0i16; Loadout::PARTS];
    parts[Loadout::HEAD] = 111;
    parts[Loadout::LEGS] = 8020;
    parts[Loadout::BOOSTER] = 6110;
    parts[Loadout::ARM_R] = 6940;
    let loadout = Loadout {
        parts,
        coloring_id: 6265,
        emblem_id: 276,
    };
    let bytes = loadout.to_bytes();
    assert_eq!(bytes.len(), Loadout::WIRE_BYTES);
    assert_eq!(Loadout::from_bytes(&bytes).unwrap(), loadout);
    assert_eq!(Loadout::from_blob(&loadout.to_blob()).unwrap(), loadout);
}

#[test]
fn world_state_lists_damaged_props() {
    let state = WorldState {
        server_tick: 42,
        props: vec![
            PropState {
                prop_id: 7,
                hp: 12.5,
                broken: false,
            },
            PropState {
                prop_id: 9,
                hp: 0.0,
                broken: true,
            },
        ],
        broken_bits: vec![0b0000_0010],
    };
    let bytes = state.to_bytes();
    assert!(bytes.len() <= 4 + 2 + 2 * PropState::WIRE_BYTES + 2 + 1);
    assert_eq!(WorldState::from_bytes(&bytes).unwrap(), state);
    assert!(state.is_broken(1));
    assert!(!state.is_broken(0));

    let wrapped = ReliableMessage::WorldState(state);
    assert_eq!(
        ReliableMessage::from_bytes(&wrapped.to_bytes()).unwrap(),
        wrapped
    );
}

#[test]
fn local_state_and_hot_messages_roundtrip() {
    let local = local_state();
    assert_eq!(local.to_bytes().len(), LocalPlayerState::WIRE_BYTES);
    assert_eq!(
        LocalPlayerState::from_bytes(&local.to_bytes()).unwrap(),
        local
    );

    let correction = MissileCorrection {
        projectile_id: 7,
        pos: [-100, 200, -300],
        vel: [3, -4, 5],
        target: NetId::new(9).unwrap(),
    };
    assert_eq!(correction.to_bytes().len(), MissileCorrection::WIRE_BYTES);
    assert_eq!(
        MissileCorrection::from_bytes(&correction.to_bytes()).unwrap(),
        correction
    );

    let fire = FireEvent {
        shooter: NetId::new(3).unwrap(),
        projectile_id: 12,
        weapon_slot: 1,
        muzzle_pos: [10, -20, 30],
        aim_dir: [127, -127, 0],
        fire_tick: 99,
        seed: 5,
    };
    assert_eq!(fire.to_bytes().len(), FireEvent::WIRE_BYTES);
    assert_eq!(FireEvent::from_bytes(&fire.to_bytes()).unwrap(), fire);
}

#[test]
fn hot_messages_are_tagged_and_have_no_bullet_transforms() {
    let fire = FireEvent {
        shooter: NetId::new(4).unwrap(),
        projectile_id: 99,
        weapon_slot: 0,
        muzzle_pos: [1, 2, 3],
        aim_dir: [0, 0, -127],
        fire_tick: 77,
        seed: 9,
    };
    let hot = HotMessage::Fire(fire);
    let bytes = hot.to_bytes();
    assert_eq!(bytes[0], hot_msg::FIRE);
    assert_eq!(HotMessage::from_bytes(&bytes).unwrap(), hot);

    let snapshot = SnapshotPacket {
        server_tick: 77,
        local: Some(local_state()),
        records: vec![record(4)],
        ..Default::default()
    };
    let hot = HotMessage::Snapshot(snapshot);
    let bytes = hot.to_bytes();
    assert_eq!(bytes[0], hot_msg::SNAPSHOT);
    assert_eq!(HotMessage::from_bytes(&bytes).unwrap(), hot);

    // Every HOT payload is either a snapshot or a shot event; there is no per-bullet transform.
    let mut unknown = bytes.to_vec();
    unknown[0] = 2;
    assert!(HotMessage::from_bytes(&unknown).is_err());
}

#[test]
fn combat_packet_coalesces_and_roundtrips() {
    let packet = CombatPacket {
        damages: vec![DamageEvent {
            target: NetId::new(2).unwrap(),
            attacker: NetId::new(1).unwrap(),
            amount: 900,
            attribute: 0,
            effective: 1,
            lethal: false,
            hit_pos: [10, 20, 30],
            tick: 55,
        }],
        hits: vec![ProjectileHit {
            projectile_id: 12,
            target: NetId::new(2).unwrap(),
            hit_pos: [10, 20, 30],
            tick: 55,
        }],
        expires: vec![ProjectileExpire {
            projectile_id: 13,
            tick: 56,
        }],
    };
    assert_eq!(
        CombatPacket::from_bytes(&packet.to_bytes()).unwrap(),
        packet
    );
}

#[test]
fn lifecycle_and_combat_messages_roundtrip() {
    let join = JoinAccepted {
        protocol: PROTOCOL_VERSION,
        net_id: NetId::new(1).unwrap(),
        server_tick: 10,
        map_id: 4000,
        pos_min: [-800.0, -50.0, -800.0],
        pos_max: [800.0, 100.0, 800.0],
        loadout: vec![1, 2, 3, 4],
    };
    assert_eq!(JoinAccepted::from_bytes(&join.to_bytes()).unwrap(), join);

    let command = ClientCommandPacket {
        seq: 1,
        kind: command_kind::JOIN,
        protocol: PROTOCOL_VERSION,
        loadout: vec![9, 8, 7],
    };
    assert_eq!(
        ClientCommandPacket::from_bytes(&command.to_bytes()).unwrap(),
        command
    );

    let joined = PlayerJoined {
        net_id: NetId::new(2).unwrap(),
        team: 1,
    };
    assert_eq!(
        PlayerJoined::from_bytes(&joined.to_bytes()).unwrap(),
        joined
    );

    let left = PlayerLeft {
        net_id: NetId::new(2).unwrap(),
        reason: 1,
    };
    assert_eq!(PlayerLeft::from_bytes(&left.to_bytes()).unwrap(), left);

    let loadout = LoadoutUpdate {
        net_id: NetId::new(1).unwrap(),
        loadout: vec![4; 32],
    };
    assert_eq!(
        LoadoutUpdate::from_bytes(&loadout.to_bytes()).unwrap(),
        loadout
    );

    let state = MatchState {
        state: 2,
        score_a: 5,
        score_b: 3,
        time_left_ticks: 12_000,
    };
    assert_eq!(MatchState::from_bytes(&state.to_bytes()).unwrap(), state);

    let kill = KillEvent {
        killer: NetId::new(1).unwrap(),
        victim: NetId::new(2).unwrap(),
        weapon: 4,
        tick: 500,
    };
    assert_eq!(KillEvent::from_bytes(&kill.to_bytes()).unwrap(), kill);

    let respawn = RespawnDrop {
        net_id: NetId::new(2).unwrap(),
        drop_point: [1.0, 2.0, 3.0],
        approach_yaw: 0.5,
        start_tick: 600,
    };
    assert_eq!(
        RespawnDrop::from_bytes(&respawn.to_bytes()).unwrap(),
        respawn
    );

    let broke = DestructibleBreak {
        prop_id: 44,
        tick: 700,
    };
    assert_eq!(
        DestructibleBreak::from_bytes(&broke.to_bytes()).unwrap(),
        broke
    );

    let damage = DamageEvent {
        target: NetId::new(2).unwrap(),
        attacker: NetId::new(1).unwrap(),
        amount: 250,
        attribute: 1,
        effective: 1,
        lethal: false,
        hit_pos: [10, -20, 30],
        tick: 800,
    };
    assert_eq!(DamageEvent::from_bytes(&damage.to_bytes()).unwrap(), damage);

    let hit = ProjectileHit {
        projectile_id: 5,
        target: NetId::new(2).unwrap(),
        hit_pos: [1, 2, 3],
        tick: 900,
    };
    assert_eq!(ProjectileHit::from_bytes(&hit.to_bytes()).unwrap(), hit);

    let expire = ProjectileExpire {
        projectile_id: 5,
        tick: 1000,
    };
    assert_eq!(
        ProjectileExpire::from_bytes(&expire.to_bytes()).unwrap(),
        expire
    );
}

#[test]
fn reliable_channel_messages_are_tagged() {
    let left = ReliableMessage::PlayerLeft(PlayerLeft {
        net_id: NetId::new(2).unwrap(),
        reason: 1,
    });
    let bytes = left.to_bytes();
    assert_eq!(bytes[0], reliable_msg::PLAYER_LEFT);
    assert_eq!(ReliableMessage::from_bytes(&bytes).unwrap(), left);

    let state = ReliableMessage::MatchState(MatchState {
        state: 1,
        score_a: 2,
        score_b: 3,
        time_left_ticks: 500,
    });
    assert_eq!(
        ReliableMessage::from_bytes(&state.to_bytes()).unwrap(),
        state
    );

    let joined = ReliableMessage::PlayerJoined(PlayerJoined {
        net_id: NetId::new(1).unwrap(),
        team: 1,
    });
    assert_eq!(joined.to_bytes()[0], reliable_msg::PLAYER_JOINED);
    assert_eq!(
        ReliableMessage::from_bytes(&joined.to_bytes()).unwrap(),
        joined
    );
}

#[test]
fn rejects_malformed_messages() {
    let mut bytes = InputFrame::default().to_bytes().to_vec();
    bytes.push(0);
    assert!(InputFrame::from_bytes(&bytes).is_err());

    let bad_count = [0x00, 0x00, 0x00, 0x00];
    assert!(InputPacket::from_bytes(&bad_count).is_err());

    let bad_id = record(1);
    let mut bytes = bad_id.to_bytes().to_vec();
    bytes[0] = 0;
    assert!(AcRecord::from_bytes(&bytes).is_err());
}
