//! Socket-free in-process server/client pair for deterministic tests.
//!
//! This wraps renet's `new_local_client`/`process_local_client` path: no UDP socket, no netcode
//! handshake, same channel configuration as the real transport.

use std::time::Duration;

use bytes::Bytes;
use renet::{ClientId, RenetClient, RenetServer, ServerEvent};

use crate::channels::local_connection_config;

const DT: Duration = Duration::from_nanos(16_666_667);

/// A `RenetServer` plus one local client connection.
pub struct LocalPair {
    server: RenetServer,
    client: RenetClient,
    client_id: ClientId,
}

impl LocalPair {
    pub fn new(client_id: ClientId) -> Self {
        let mut server = RenetServer::new(local_connection_config());
        let client = server.new_local_client(client_id);
        Self {
            server,
            client,
            client_id,
        }
    }

    pub fn client_id(&self) -> ClientId {
        self.client_id
    }

    /// Exchanges queued messages and advances both sides by one 60 Hz frame.
    pub fn update(&mut self) {
        self.server.update(DT);
        self.client.update(DT);
        self.server
            .process_local_client(self.client_id, &mut self.client)
            .expect("local client connection");
    }

    pub fn server_event(&mut self) -> Option<ServerEvent> {
        self.server.get_event()
    }

    pub fn client_send(&mut self, channel: u8, message: impl Into<Bytes>) {
        self.client.send_message(channel, message);
    }

    pub fn server_send(&mut self, channel: u8, message: impl Into<Bytes>) {
        self.server.send_message(self.client_id, channel, message);
    }

    pub fn server_receive(&mut self, channel: u8) -> Option<Bytes> {
        self.server.receive_message(self.client_id, channel)
    }

    pub fn client_receive(&mut self, channel: u8) -> Option<Bytes> {
        self.client.receive_message(channel)
    }

    pub fn disconnect(&mut self) {
        self.server
            .disconnect_local_client(self.client_id, &mut self.client);
    }

    pub fn server_connected_clients(&self) -> usize {
        self.server.connected_clients()
    }
}
