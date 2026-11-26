//! Client-side renet + netcode wrapper.

use std::net::{SocketAddr, UdpSocket};

use anyhow::{Context, Result};
use bytes::Bytes;
use renet::{ClientId, RenetClient};
use renet_netcode::{ClientAuthentication, NetcodeClientTransport};

use crate::channels::connection_config;
use crate::metrics::NetMetrics;
use crate::server::unix_time;

/// A netcode client connected (or connecting) to one server address.
pub struct NetClient {
    pub(crate) client: RenetClient,
    pub(crate) transport: NetcodeClientTransport,
}

impl NetClient {
    /// Connects with unsecure netcode auth. `server` may be the dedicated server or a netem
    /// relay; the token is generated for that address.
    pub fn connect(server: SocketAddr, client_id: ClientId, protocol_id: u64) -> Result<Self> {
        let bind = if server.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        };
        let socket = UdpSocket::bind(bind).context("bind client UDP socket")?;
        let authentication = ClientAuthentication::Unsecure {
            protocol_id,
            client_id,
            server_addr: server,
            user_data: None,
        };
        let transport = NetcodeClientTransport::new(unix_time(), authentication, socket)
            .context("create netcode client transport")?;
        Ok(Self {
            client: RenetClient::new(connection_config()),
            transport,
        })
    }

    /// Advances transport + renet. Transport disconnects are reported as errors like any other
    /// renet failure; callers that expect shutdown should check [`Self::is_disconnected`].
    pub fn update(&mut self, dt: std::time::Duration) -> Result<()> {
        self.transport
            .update(dt, &mut self.client)
            .context("netcode client update")?;
        self.client.update(dt);
        Ok(())
    }

    pub fn client_id(&self) -> ClientId {
        self.transport.client_id()
    }

    pub fn is_connecting(&self) -> bool {
        self.client.is_connecting()
    }

    pub fn is_connected(&self) -> bool {
        self.client.is_connected()
    }

    pub fn is_disconnected(&self) -> bool {
        self.client.is_disconnected()
    }

    pub fn send(&mut self, channel: u8, message: impl Into<Bytes>) {
        self.client.send_message(channel, message);
    }

    pub fn receive(&mut self, channel: u8) -> Option<Bytes> {
        self.client.receive_message(channel)
    }

    pub fn metrics(&self) -> NetMetrics {
        NetMetrics::from(self.client.network_info())
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.transport.addr().context("client socket address")
    }

    pub fn disconnect(&mut self) {
        self.transport.disconnect();
    }

    /// Flushes queued packets to the socket.
    pub fn send_packets(&mut self) -> Result<()> {
        self.transport
            .send_packets(&mut self.client)
            .context("flush client packets")?;
        Ok(())
    }
}
