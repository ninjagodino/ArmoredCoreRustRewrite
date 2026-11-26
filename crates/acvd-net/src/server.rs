//! Server-side renet + netcode wrapper.

use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use renet::{ClientId, RenetServer, ServerEvent};
use renet_netcode::{NetcodeServerTransport, ServerAuthentication, ServerConfig};

use crate::channels::connection_config;
use crate::metrics::NetMetrics;

/// A bound netcode server with the project's channel layout.
pub struct NetServer {
    pub(crate) server: RenetServer,
    pub(crate) transport: NetcodeServerTransport,
    local: SocketAddr,
}

impl NetServer {
    /// Binds `bind` and prepares a netcode server. `bind` may use port 0; read the real port with
    /// [`Self::local_addr`].
    pub fn bind(bind: SocketAddr, max_clients: usize, protocol_id: u64) -> Result<Self> {
        let socket = UdpSocket::bind(bind).with_context(|| format!("bind UDP {bind}"))?;
        Self::from_socket(socket, max_clients, protocol_id)
    }

    pub fn from_socket(socket: UdpSocket, max_clients: usize, protocol_id: u64) -> Result<Self> {
        let local = socket.local_addr().context("read server socket address")?;
        let config = ServerConfig {
            current_time: unix_time(),
            max_clients,
            protocol_id,
            public_addresses: vec![local],
            authentication: ServerAuthentication::Unsecure,
        };
        let transport = NetcodeServerTransport::new(config, socket)
            .context("create netcode server transport")?;
        Ok(Self {
            server: RenetServer::new(connection_config()),
            transport,
            local,
        })
    }

    /// The bound UDP address.
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// Advances transport + renet and returns connection events.
    pub fn poll(&mut self, dt: Duration) -> Result<Vec<ServerEvent>> {
        self.transport
            .update(dt, &mut self.server)
            .context("netcode transport update")?;
        self.server.update(dt);
        let mut events = Vec::new();
        while let Some(event) = self.server.get_event() {
            events.push(event);
        }
        Ok(events)
    }

    pub fn send(&mut self, client: ClientId, channel: u8, message: impl Into<Bytes>) {
        self.server.send_message(client, channel, message);
    }

    pub fn broadcast(&mut self, channel: u8, message: impl Into<Bytes>) {
        self.server.broadcast_message(channel, message);
    }

    pub fn broadcast_except(&mut self, except: ClientId, channel: u8, message: impl Into<Bytes>) {
        self.server
            .broadcast_message_except(except, channel, message);
    }

    pub fn receive(&mut self, client: ClientId, channel: u8) -> Option<Bytes> {
        self.server.receive_message(client, channel)
    }

    pub fn clients(&self) -> Vec<ClientId> {
        self.server.clients_id()
    }

    pub fn connected_clients(&self) -> usize {
        self.server.connected_clients()
    }

    pub fn is_connected(&self, client: ClientId) -> bool {
        self.server.is_connected(client)
    }

    pub fn disconnect(&mut self, client: ClientId) {
        self.server.disconnect(client);
    }

    pub fn metrics(&self, client: ClientId) -> NetMetrics {
        self.server
            .network_info(client)
            .map(NetMetrics::from)
            .unwrap_or_default()
    }

    pub fn rtt_ms(&self, client: ClientId) -> f64 {
        self.server.rtt(client) * 1000.0
    }

    pub fn packet_loss(&self, client: ClientId) -> f64 {
        self.server.packet_loss(client)
    }

    /// Flushes queued packets to the socket.
    pub fn send_packets(&mut self) {
        self.transport.send_packets(&mut self.server);
    }

    /// Disconnects everyone, flushes, and drains the renet event queue.
    pub fn shutdown(&mut self) {
        self.transport.disconnect_all(&mut self.server);
        self.transport.send_packets(&mut self.server);
        while self.server.get_event().is_some() {}
    }
}

pub(crate) fn unix_time() -> Duration {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
}
