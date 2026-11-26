//! Transparent seeded UDP conditioner (netem).
//!
//! The relay sits below renet_netcode: clients connect to the relay address, the relay forwards
//! raw datagrams to the server (each client gets its own upstream socket so replies demultiplex
//! naturally), and every packet is delayed, dropped, duplicated, or reordered by a seeded PCG.
//! No netcode parsing or regeneration happens here.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

/// Conditioner parameters. All probabilities are percentages (`2.0` = 2%).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NetemConfig {
    /// Added round-trip time (split evenly across the two directions).
    pub rtt_ms: u32,
    /// Uniform one-way jitter window in milliseconds.
    pub jitter_ms: u32,
    pub loss_pct: f32,
    pub duplicate_pct: f32,
    /// Extra delay for a percentage of packets, producing reordering.
    pub reorder_pct: f32,
    pub seed: u64,
}

impl Default for NetemConfig {
    fn default() -> Self {
        Self {
            rtt_ms: 0,
            jitter_ms: 0,
            loss_pct: 0.0,
            duplicate_pct: 0.0,
            reorder_pct: 0.0,
            seed: 0,
        }
    }
}

impl NetemConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        rtt_ms: u32,
        jitter_ms: u32,
        loss_pct: f32,
        duplicate_pct: f32,
        reorder_pct: f32,
        seed: u64,
    ) -> Self {
        Self {
            rtt_ms,
            jitter_ms,
            loss_pct: loss_pct.clamp(0.0, 100.0),
            duplicate_pct: duplicate_pct.clamp(0.0, 100.0),
            reorder_pct: reorder_pct.clamp(0.0, 100.0),
            seed,
        }
    }
}

/// Counters for one relay lifetime.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetemStats {
    /// Datagrams actually forwarded (duplicates included).
    pub forwarded: u64,
    pub dropped: u64,
    pub duplicated: u64,
    pub reordered: u64,
}

/// Handle to a running relay thread.
pub struct NetemHandle {
    addr: SocketAddr,
    stats: Arc<Mutex<NetemStats>>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl NetemHandle {
    /// The relay address clients connect to.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn stats(&self) -> NetemStats {
        *self.stats.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for NetemHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Starts a relay forwarding to `server`. `listen` may use port 0.
pub fn start(server: SocketAddr, listen: SocketAddr, config: NetemConfig) -> Result<NetemHandle> {
    let down = UdpSocket::bind(listen).with_context(|| format!("bind netem relay {listen}"))?;
    down.set_nonblocking(true)
        .context("netem relay nonblocking")?;
    let addr = down.local_addr().context("netem relay address")?;
    let stats = Arc::new(Mutex::new(NetemStats::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stats = Arc::clone(&stats);
    let thread_stop = Arc::clone(&stop);
    let join = thread::Builder::new()
        .name("acvd-netem".into())
        .spawn(move || run_relay(server, down, config, thread_stop, thread_stats))
        .context("spawn netem relay thread")?;
    Ok(NetemHandle {
        addr,
        stats,
        stop,
        join: Some(join),
    })
}

struct Upstream {
    socket: UdpSocket,
    client: SocketAddr,
}

type Queued = Reverse<(Instant, u64, Option<usize>, SocketAddr, Vec<u8>)>;

fn run_relay(
    server: SocketAddr,
    down: UdpSocket,
    config: NetemConfig,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<NetemStats>>,
) {
    let mut upstreams: Vec<Upstream> = Vec::new();
    let mut by_client: HashMap<SocketAddr, usize> = HashMap::new();
    let mut queue: BinaryHeap<Queued> = BinaryHeap::new();
    let mut rng = Pcg32::new(config.seed);
    let mut seq = 0u64;
    let mut buf = [0u8; 65_535];
    let mut local_stats = NetemStats::default();

    let bind_wild = if server.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };

    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        let mut activity = false;

        // Clients -> server.
        loop {
            match down.recv_from(&mut buf) {
                Ok((len, from)) => {
                    activity = true;
                    let idx = *by_client.entry(from).or_insert_with(|| {
                        let socket =
                            UdpSocket::bind(bind_wild).expect("bind netem upstream socket");
                        socket
                            .set_nonblocking(true)
                            .expect("netem upstream nonblocking");
                        upstreams.push(Upstream {
                            socket,
                            client: from,
                        });
                        upstreams.len() - 1
                    });
                    enqueue(
                        &mut queue,
                        &mut rng,
                        &config,
                        &mut local_stats,
                        &mut seq,
                        now,
                        Some(idx),
                        server,
                        buf[..len].to_vec(),
                    );
                }
                Err(e)
                    if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted =>
                {
                    break
                }
                Err(e) => {
                    log::error!("netem relay receive: {e}");
                    break;
                }
            }
        }

        // Server -> clients (one upstream socket per client keeps the mapping unambiguous).
        for idx in 0..upstreams.len() {
            loop {
                match upstreams[idx].socket.recv_from(&mut buf) {
                    Ok((len, from)) => {
                        if from != server {
                            continue;
                        }
                        activity = true;
                        let client = upstreams[idx].client;
                        enqueue(
                            &mut queue,
                            &mut rng,
                            &config,
                            &mut local_stats,
                            &mut seq,
                            now,
                            None,
                            client,
                            buf[..len].to_vec(),
                        );
                    }
                    Err(e)
                        if e.kind() == ErrorKind::WouldBlock
                            || e.kind() == ErrorKind::Interrupted =>
                    {
                        break
                    }
                    Err(e) => {
                        log::error!("netem upstream receive: {e}");
                        break;
                    }
                }
            }
        }

        // Due packets.
        loop {
            let Some(Reverse((due, seq_no, via, dest, data))) = queue.peek().cloned() else {
                break;
            };
            if due > now {
                break;
            }
            queue.pop();
            let sent = match via {
                Some(idx) => upstreams[idx].socket.send_to(&data, dest),
                None => down.send_to(&data, dest),
            };
            if sent.is_ok() {
                local_stats.forwarded += 1;
            } else if let Err(e) = sent {
                log::debug!("netem forward to {dest} (seq {seq_no}): {e}");
            }
            activity = true;
        }

        if let Ok(mut shared) = stats.lock() {
            *shared = local_stats;
        }

        if !activity {
            thread::sleep(Duration::from_millis(1));
        } else if let Some(Reverse((due, ..))) = queue.peek() {
            let wait = due.saturating_duration_since(Instant::now());
            if wait > Duration::from_millis(1) {
                thread::sleep(wait.min(Duration::from_millis(1)));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn enqueue(
    queue: &mut BinaryHeap<Queued>,
    rng: &mut Pcg32,
    config: &NetemConfig,
    stats: &mut NetemStats,
    seq: &mut u64,
    now: Instant,
    via: Option<usize>,
    dest: SocketAddr,
    data: Vec<u8>,
) {
    if config.loss_pct > 0.0 && rng.chance(config.loss_pct) {
        stats.dropped += 1;
        return;
    }

    let mut delay_ms = f64::from(config.rtt_ms) / 2.0;
    if config.jitter_ms > 0 {
        delay_ms += (rng.next_f64() * 2.0 - 1.0) * f64::from(config.jitter_ms) / 2.0;
    }
    if config.reorder_pct > 0.0 && rng.chance(config.reorder_pct) {
        delay_ms += 20.0 + rng.next_f64() * 30.0;
        stats.reordered += 1;
    }
    let due = now + Duration::from_secs_f64(delay_ms.max(0.0) / 1000.0);

    *seq += 1;
    queue.push(Reverse((due, *seq, via, dest, data.clone())));

    if config.duplicate_pct > 0.0 && rng.chance(config.duplicate_pct) {
        stats.duplicated += 1;
        *seq += 1;
        queue.push(Reverse((
            due + Duration::from_millis(5),
            *seq,
            via,
            dest,
            data,
        )));
    }
}

/// Small PCG32 so conditioner runs are reproducible from `seed`.
struct Pcg32 {
    state: u64,
    inc: u64,
}

impl Pcg32 {
    fn new(seed: u64) -> Self {
        let mut rng = Self {
            state: 0,
            inc: (seed << 1) | 1,
        };
        rng.next_u32();
        rng.state = rng.state.wrapping_add(seed);
        rng.next_u32();
        rng
    }

    fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(self.inc);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    fn next_f64(&mut self) -> f64 {
        f64::from(self.next_u32()) / (f64::from(u32::MAX) + 1.0)
    }

    fn chance(&mut self, pct: f32) -> bool {
        self.next_f64() * 100.0 < f64::from(pct)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_chance_is_reproducible() {
        let mut a = Pcg32::new(7);
        let mut b = Pcg32::new(7);
        for _ in 0..1000 {
            assert_eq!(a.next_u32(), b.next_u32());
        }
    }

    #[test]
    fn zero_config_relays_datagrams() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_addr = server.local_addr().unwrap();
        let mut relay = start(
            server_addr,
            "127.0.0.1:0".parse().unwrap(),
            NetemConfig::default(),
        )
        .unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client.send_to(b"ping", relay.addr()).unwrap();

        let mut buf = [0u8; 16];
        let (n, from) = server
            .recv_from(&mut buf)
            .expect("relay forwards to server");
        assert_eq!(&buf[..n], b"ping");
        server.send_to(b"pong", from).unwrap();
        let (n, _) = client
            .recv_from(&mut buf)
            .expect("relay forwards to client");
        assert_eq!(&buf[..n], b"pong");

        let deadline = Instant::now() + Duration::from_secs(2);
        while relay.stats().forwarded < 2 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let stats = relay.stats();
        assert_eq!(stats.forwarded, 2);
        assert_eq!(stats.dropped, 0);
        relay.shutdown();
    }

    #[test]
    fn duplicate_and_reorder_are_reported() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let config = NetemConfig::new(0, 0, 0.0, 100.0, 100.0, 11);
        let mut relay = start(
            server.local_addr().unwrap(),
            "127.0.0.1:0".parse().unwrap(),
            config,
        )
        .unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.send_to(b"dup", relay.addr()).unwrap();
        let mut buf = [0u8; 16];
        for _ in 0..2 {
            let (n, _) = server.recv_from(&mut buf).expect("both copies arrive");
            assert_eq!(&buf[..n], b"dup");
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while relay.stats().forwarded < 2 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let stats = relay.stats();
        assert_eq!(stats.forwarded, 2);
        assert_eq!(stats.duplicated, 1);
        assert_eq!(stats.reordered, 1);
        relay.shutdown();
    }

    #[test]
    fn loss_is_seeded_and_reported() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_addr = server.local_addr().unwrap();
        let config = NetemConfig::new(0, 0, 100.0, 0.0, 0.0, 3);
        let mut relay = start(server_addr, "127.0.0.1:0".parse().unwrap(), config).unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        for _ in 0..10 {
            client.send_to(b"lost", relay.addr()).unwrap();
        }
        thread::sleep(Duration::from_millis(100));
        let stats = relay.stats();
        assert_eq!(stats.dropped, 10);
        assert_eq!(stats.forwarded, 0);
        relay.shutdown();
    }
}
