//! Standalone renet 2.0.1 + renet_netcode 2.0.1 transport for ACVD multiplayer.
//!
//! No Bevy, sim, or render types: the server and client both speak `acvd-proto` bytes through
//! this crate. The netem relay is a transparent raw-UDP conditioner below netcode.

pub mod channels;
pub mod client;
pub mod local;
pub mod metrics;
pub mod netem;
pub mod server;

pub use channels::{
    client_channels, connection_config, server_channels, AVAILABLE_BYTES_PER_TICK, BULK, COMBAT,
    COMMAND, HOT, RELIABLE,
};
pub use client::NetClient;
pub use local::LocalPair;
pub use metrics::NetMetrics;
pub use netem::{NetemConfig, NetemHandle, NetemStats};
pub use renet::{ClientId, DisconnectReason, SendType, ServerEvent};
pub use server::NetServer;
