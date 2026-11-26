//! Per-client snapshot assembly and MTU splitting.
//!
//! The global `AcRecord` set is built once per snapshot tick, then each client filters it through
//! interest management. `LocalPlayerState` is always placed in snapshot packet 1, and the shared
//! splitter caps every packet at 1100 B with at most two packets per snapshot tick.

use acvd_proto::{
    AcRecord, LocalPlayerState, PosQuantizer, SnapshotPacket, SnapshotSplit, AC_RECORD_BYTES,
    MAX_SNAPSHOT_BYTES,
};
use acvd_sim::ids::NetId as SimNetId;
use acvd_sim::world::{record_of, wire_id, SimWorld};

use crate::interest::{InterestManager, Tier};

/// Pre-built full records for one snapshot tick.
#[derive(Clone, Debug, Default)]
pub struct RecordSet {
    pub records: Vec<(SimNetId, AcRecord)>,
}

impl RecordSet {
    pub fn build(world: &SimWorld, quantizer: &PosQuantizer) -> Self {
        let mut records: Vec<(SimNetId, AcRecord)> = world
            .acs
            .values()
            .map(|ac| (ac.id, record_of(ac, quantizer)))
            .collect();
        records.sort_by_key(|(id, _)| *id);
        Self { records }
    }

    pub fn get(&self, id: SimNetId) -> Option<&AcRecord> {
        self.records
            .iter()
            .find(|(net_id, _)| *net_id == id)
            .map(|(_, record)| record)
    }
}

/// Reduced tier-2 record: position, facing, flags, and pose only.
pub fn reduced(record: &AcRecord) -> AcRecord {
    AcRecord {
        vel: [0, 0, 0],
        pitch: 0,
        en: 0,
        ap: 0,
        reload: [0, 0],
        ammo: [0, 0],
        ..*record
    }
}

/// Builds one client's snapshot. `viewer` and `lock_target` are simulation ids; the wire record
/// ids are converted with `wire_id`.
#[allow(clippy::too_many_arguments)]
pub fn build(
    world: &SimWorld,
    interest: &InterestManager,
    records: &RecordSet,
    viewer: SimNetId,
    lock_target: SimNetId,
    acked_input_seq: u16,
    snapshot_seq: u16,
    force_full: bool,
) -> SnapshotPacket {
    let mut out: Vec<AcRecord> = Vec::with_capacity(records.records.len());
    if force_full {
        // Late join: every AC at the join tick, independent of distance/cadence. `LocalPlayerState`
        // still carries the joining client's exact own state.
        for (id, record) in &records.records {
            if *id == viewer {
                continue;
            }
            out.push(*record);
        }
    } else {
        let visible = interest.visible(world, viewer, lock_target, false);
        for (id, tier) in visible {
            if !tier.should_send(snapshot_seq) {
                continue;
            }
            let Some(record) = records.get(id) else {
                continue;
            };
            out.push(if tier == Tier::Far {
                reduced(record)
            } else {
                *record
            });
        }
        // Keep the viewer's own AC out of the record list; the local high-precision state is
        // cheaper and more useful.
    }
    let _ = wire_id(viewer);
    let corrections = world
        .acs
        .get(&viewer)
        .map(|ac| world.missile_corrections(ac.position, 400.0, 8))
        .unwrap_or_default();
    SnapshotPacket {
        server_tick: world.tick.0,
        snapshot_seq,
        acked_input_seq,
        flags: 0,
        local: world.local_state(viewer, acked_input_seq),
        records: out,
        corrections,
    }
}

/// Splits a snapshot into at most two datagrams.
pub fn split(packet: &SnapshotPacket) -> SnapshotSplit {
    packet.split_for_udp()
}

/// Largest record count that still fits one packet with the local state.
pub fn max_records_per_packet() -> usize {
    (MAX_SNAPSHOT_BYTES - 12 - LocalPlayerState::WIRE_BYTES) / AC_RECORD_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;
    use acvd_proto::{ac_flags, quantize_yaw, Loadout, NetId};
    use acvd_sim::tick::Tick;
    use acvd_sim::world::SimAc;
    use glam::Vec3;

    fn world(n: u8) -> SimWorld {
        let mut world = SimWorld::new();
        for id in 1..=n {
            let mut ac = SimAc::new(
                SimNetId(id),
                Loadout::default(),
                Vec3::new(f32::from(id) * 2.0, 0.0, 0.0),
                0.0,
                0,
            );
            ac.pose = id % 12;
            world.insert(ac);
        }
        world.tick = Tick(100);
        world
    }

    #[test]
    fn local_state_is_always_in_packet_one() {
        let world = world(64);
        let records = RecordSet::build(&world, &PosQuantizer::default());
        let interest = InterestManager::new();
        let packet = build(
            &world,
            &interest,
            &records,
            SimNetId(1),
            SimNetId(0),
            7,
            0,
            true,
        );
        let split = split(&packet);
        assert!(!split.packets.is_empty());
        assert!(split.packets[0].local.is_some());
        for (i, pkt) in split.packets.iter().enumerate() {
            assert!(pkt.wire_len() <= MAX_SNAPSHOT_BYTES, "packet {i}");
            assert_eq!(pkt.local.is_some(), i == 0);
        }
        assert!(split.packets.len() <= 2);
        assert_eq!(
            split.packets.iter().map(|p| p.records.len()).sum::<usize>() + split.dropped_records,
            packet.records.len()
        );
    }

    #[test]
    fn full_late_join_includes_every_other_ac() {
        let world = world(24);
        let records = RecordSet::build(&world, &PosQuantizer::default());
        let interest = InterestManager::new();
        let packet = build(
            &world,
            &interest,
            &records,
            SimNetId(1),
            SimNetId(0),
            0,
            0,
            true,
        );
        assert_eq!(packet.records.len(), 23);
        assert!(packet.local.is_some());
        assert!(packet
            .records
            .iter()
            .all(|r| r.net_id != NetId::new(1).unwrap()));
    }

    #[test]
    fn interest_filter_and_far_reduction() {
        let mut world = world(3);
        world.acs.get_mut(&SimNetId(2)).unwrap().position = Vec3::new(500.0, 0.0, 0.0);
        world.acs.get_mut(&SimNetId(3)).unwrap().position = Vec3::new(1000.0, 0.0, 0.0);
        world.tick = Tick(90);
        let records = RecordSet::build(&world, &PosQuantizer::default());
        let mut interest = InterestManager::new();
        interest.rebuild(&world);
        let packet = build(
            &world,
            &interest,
            &records,
            SimNetId(1),
            SimNetId(0),
            0,
            30,
            false,
        );
        assert_eq!(
            packet.records.len(),
            1,
            "far goes every 15th snapshot, beyond 800 m culled"
        );
        assert_eq!(packet.records[0].net_id, NetId::new(2).unwrap());
        assert_eq!(packet.records[0].flags & ac_flags::TEAM_B, 0);
        assert_eq!(packet.records[0].yaw, quantize_yaw(0.0));
    }
}
