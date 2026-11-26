//! Renet channel layout and send priority.

use std::time::Duration;

use renet::{ChannelConfig, ConnectionConfig, SendType};

/// Inputs, snapshots, FireEvent, missile corrections.
pub const HOT: u8 = 0;
/// Server -> client lifecycle/world transitions (`ReliableOrdered`).
pub const RELIABLE: u8 = 1;
/// Client -> server join/loadout/respawn commands. Shares id 1 with `RELIABLE` because renet
/// channel ids are unilateral.
pub const COMMAND: u8 = 1;
/// Late-join full world/loadout/destructible state (`ReliableOrdered`).
pub const BULK: u8 = 2;
/// Damage/hit/expire feedback (`ReliableUnordered`).
pub const COMBAT: u8 = 3;

/// Per-tick send budget for every connection (renet default is 60_000).
pub const AVAILABLE_BYTES_PER_TICK: u64 = 4096;

const HOT_MEMORY: usize = 1024 * 1024;
const RELIABLE_MEMORY: usize = 1024 * 1024;
const BULK_MEMORY: usize = 4 * 1024 * 1024;
const COMBAT_MEMORY: usize = 1024 * 1024;
const COMMAND_MEMORY: usize = 256 * 1024;

fn hot() -> ChannelConfig {
    ChannelConfig {
        channel_id: HOT,
        max_memory_usage_bytes: HOT_MEMORY,
        send_type: SendType::Unreliable,
    }
}

fn reliable() -> ChannelConfig {
    ChannelConfig {
        channel_id: RELIABLE,
        max_memory_usage_bytes: RELIABLE_MEMORY,
        send_type: SendType::ReliableOrdered {
            resend_time: Duration::from_millis(50),
        },
    }
}

fn combat() -> ChannelConfig {
    ChannelConfig {
        channel_id: COMBAT,
        max_memory_usage_bytes: COMBAT_MEMORY,
        send_type: SendType::ReliableUnordered {
            resend_time: Duration::from_millis(50),
        },
    }
}

fn bulk() -> ChannelConfig {
    ChannelConfig {
        channel_id: BULK,
        max_memory_usage_bytes: BULK_MEMORY,
        send_type: SendType::ReliableOrdered {
            resend_time: Duration::from_millis(100),
        },
    }
}

fn command() -> ChannelConfig {
    ChannelConfig {
        channel_id: COMMAND,
        max_memory_usage_bytes: COMMAND_MEMORY,
        send_type: SendType::ReliableOrdered {
            resend_time: Duration::from_millis(50),
        },
    }
}

/// Server send order: `HOT -> RELIABLE -> COMBAT -> BULK`.
///
/// renet spends `available_bytes_per_tick` on the first channel and passes the remainder to the
/// next, so this ordering keeps rare kills/destructibles (`RELIABLE`) ahead of combat hit
/// feedback and far ahead of late-join bulk state.
pub fn server_channels() -> Vec<ChannelConfig> {
    vec![hot(), reliable(), combat(), bulk()]
}

/// Client send order: inputs first, then reliable commands.
pub fn client_channels() -> Vec<ChannelConfig> {
    vec![hot(), command()]
}

/// One config usable by both [`renet::RenetServer`] and [`renet::RenetClient`].
pub fn connection_config() -> ConnectionConfig {
    ConnectionConfig {
        available_bytes_per_tick: AVAILABLE_BYTES_PER_TICK,
        server_channels_config: server_channels(),
        client_channels_config: client_channels(),
    }
}

/// Symmetric layout for renet's socket-free `new_local_client` path.
///
/// renet builds the local client with the server's send channels and the client's receive
/// channels, so the production asymmetric config cannot receive `BULK`/`COMBAT` there. The
/// socket-free test uses this mirror layout; real clients use [`connection_config`].
pub fn local_connection_config() -> ConnectionConfig {
    ConnectionConfig {
        available_bytes_per_tick: AVAILABLE_BYTES_PER_TICK,
        server_channels_config: server_channels(),
        client_channels_config: server_channels(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_priority_is_hot_reliable_combat_bulk() {
        let config = server_channels();
        let ids: Vec<u8> = config.iter().map(|c| c.channel_id).collect();
        assert_eq!(ids, vec![HOT, RELIABLE, COMBAT, BULK]);
        assert!(matches!(config[0].send_type, SendType::Unreliable));
        assert!(matches!(
            config[1].send_type,
            SendType::ReliableOrdered { .. }
        ));
        assert!(matches!(
            config[2].send_type,
            SendType::ReliableUnordered { .. }
        ));
        assert!(matches!(
            config[3].send_type,
            SendType::ReliableOrdered { .. }
        ));
    }

    #[test]
    fn client_send_order_is_hot_then_command() {
        let config = client_channels();
        assert_eq!(config[0].channel_id, HOT);
        assert_eq!(config[1].channel_id, COMMAND);
        assert!(matches!(config[0].send_type, SendType::Unreliable));
        assert!(matches!(
            config[1].send_type,
            SendType::ReliableOrdered { .. }
        ));
    }

    #[test]
    fn budget_is_4096_bytes_per_tick() {
        assert_eq!(connection_config().available_bytes_per_tick, 4096);
    }
}
