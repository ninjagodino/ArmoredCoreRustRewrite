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

use anyhow::{bail, Result};

use bots::{BotDriver, Distribution, BOT_COUNT_MAX};

/// Fixed simulation tick.
pub const TICK: Duration = Duration::from_nanos(16_666_667);

/// Runs until `shutdown` is set, then disconnects every client and flushes the socket.
pub fn run(config: ServerConfig, shutdown: Arc<AtomicBool>) -> Result<()> {
    run_with_bots(
        config,
        0,
        Distribution::Cluster,
        acvd_sim::world::DEFAULT_MATCH_SEED,
        shutdown,
    )
}

/// Runs a real-time server with `bots` local bot clients in the match.
///
/// Bots join and send inputs through the same authoritative queue and snapshot path as UDP
/// clients, so a human can `--connect` at any time. With bots, one extra client slot is reserved
/// for that human (63 bots + 1 player = the 64-AC match).
pub fn run_with_bots(
    config: ServerConfig,
    bots: usize,
    distribution: Distribution,
    seed: u64,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    if bots > BOT_COUNT_MAX {
        bail!("at most {BOT_COUNT_MAX} bots are supported");
    }
    let mut config = config;
    if bots > 0 {
        // Bots take NetIds 1..=bots; keep room for at least one human client on top.
        config.max_clients = config.max_clients.max(bots + 1);
    }
    let mut server = Server::bind(config)?;
    let mut driver = (bots > 0).then(|| BotDriver::spawn(&mut server, bots, distribution, seed));
    if bots > 0 {
        log::info!(
            "acvd-server listening on {} with {bots} live bots; connect a client to join as player #{}",
            server.local_addr(),
            bots + 1
        );
    } else {
        log::info!("acvd-server listening on {}", server.local_addr());
    }

    let mut next = Instant::now();
    let mut last_metrics = Instant::now();
    while !shutdown.load(Ordering::Relaxed) {
        if let Some(driver) = driver.as_mut() {
            let tick = server.tick();
            driver.enqueue_inputs(&mut server, tick);
        }
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
