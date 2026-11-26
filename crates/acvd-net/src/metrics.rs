//! Transport metrics exposed by the wrappers.

use renet::NetworkInfo;

/// Round-trip time, packet loss, and bandwidth for one connection.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NetMetrics {
    /// Round-trip time in milliseconds.
    pub rtt_ms: f64,
    /// Packet loss as a fraction (`0.02` = 2%).
    pub packet_loss: f64,
    pub bytes_sent_per_second: f64,
    pub bytes_received_per_second: f64,
}

impl From<NetworkInfo> for NetMetrics {
    fn from(info: NetworkInfo) -> Self {
        Self {
            rtt_ms: info.rtt * 1000.0,
            packet_loss: info.packet_loss,
            bytes_sent_per_second: info.bytes_sent_per_second,
            bytes_received_per_second: info.bytes_received_per_second,
        }
    }
}
