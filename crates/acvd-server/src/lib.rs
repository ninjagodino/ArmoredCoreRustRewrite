//! Authoritative ACVD multiplayer server (Phase 1 skeleton).
//!
//! `run` owns the fixed 60 Hz loop and graceful shutdown; [`Server`] is the stepped core used by
//! tests and later by the listen-server thread.

pub mod bots;
pub mod config;
pub mod input_queue;
pub mod interest;
pub mod lagcomp;
pub mod server;
pub mod snapshot;

pub use config::ServerConfig;
pub use input_queue::{InputQueue, QueueStats, MAX_NEW_TICKS_PER_STEP, REPEAT_STARVED_TICKS};
pub use server::{test_arena, Server};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;

/// Fixed simulation tick.
pub const TICK: Duration = Duration::from_nanos(16_666_667);

/// Runs until `shutdown` is set, then disconnects every client and flushes the socket.
pub fn run(config: ServerConfig, shutdown: Arc<AtomicBool>) -> Result<()> {
    let mut server = Server::bind(config)?;
    log::info!("acvd-server listening on {}", server.local_addr());

    let mut next = Instant::now();
    let mut last_metrics = Instant::now();
    while !shutdown.load(Ordering::Relaxed) {
        server.step(TICK)?;
        next += TICK;

        let now = Instant::now();
        if now < next {
            thread::sleep(next - now);
        } else if now.duration_since(next) > TICK * 4 {
            // More than four ticks behind: drop the backlog instead of spiralling.
            next = now;
        }

        if last_metrics.elapsed() >= Duration::from_secs(5) {
            for (client, metrics) in server.metrics() {
                log::info!(
                    "client {client}: rtt {:.1} ms loss {:.2}% up {:.0} B/s down {:.0} B/s",
                    metrics.rtt_ms,
                    metrics.packet_loss * 100.0,
                    metrics.bytes_sent_per_second,
                    metrics.bytes_received_per_second,
                );
            }
            last_metrics = Instant::now();
        }
    }

    log::info!("shutting down");
    server.shutdown();
    Ok(())
}
