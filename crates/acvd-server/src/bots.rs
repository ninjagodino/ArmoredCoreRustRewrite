//! Phase 4 deterministic bot load generator and headless soak harness.
//!
//! Bots are local clients: their `InputFrame`s are accepted by the same per-player [`InputQueue`]
//! and stepped by the same `SimWorld`, and their snapshots are built/split/encoded by the same
//! `snapshot` path as UDP clients. Their encoded datagrams are measured and discarded instead of
//! being sent, so the measured bytes are exactly what a real connection would receive.
//!
//! `run_soak` runs the fixed 60 Hz loop as fast as possible for deterministic tick counts. That
//! makes 64-AC CPU budgets and 10-minute churn leak checks finish in seconds while still
//! exercising the real authoritative path.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use acvd_proto::{input_buttons, quantize_input_pitch, quantize_yaw, InputFrame};
use acvd_sim::ids::NetId as SimNetId;
use acvd_sim::stats;
use acvd_sim::world::SimWorld;
use anyhow::{Context, Result};
use glam::Vec3;
use serde::Serialize;

use crate::server::Server;
use crate::TICK;

/// Snapshot/packet budget from the netcode plan.
pub const MAX_SNAPSHOT_BYTES: usize = acvd_proto::MAX_SNAPSHOT_BYTES;
pub const MAX_PACKETS_PER_SNAPSHOT: u8 = 2;
pub const BUDGET_P95_US: u64 = 4_000;
pub const BUDGET_P99_US: u64 = 8_000;
pub const BUDGET_CLIENT_AVG_KBPS: f64 = 500.0;
pub const BUDGET_CLIENT_PEAK_KBPS: f64 = 1_500.0;
pub const BUDGET_SERVER_MBPS: f64 = 35.0;
/// Ten-minute churn memory ceiling (RSS growth).
pub const BUDGET_MEMORY_GROWTH_KB: u64 = 128 * 1024;

const BOT_COUNT_MAX: usize = 64;
const CHURN_REMOVE_EVERY: u64 = 120;
const CHURN_WAVE_EVERY: u64 = 1_800;
const CHURN_GAP_TICKS: u64 = 20;
const CLEANUP_TICKS: u64 = 900;

/// Spawn distribution for the 64-AC gates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Distribution {
    /// Eight clusters of eight, four per team, spread 160-480 m apart.
    Cluster,
    /// All 64 inside one 82 m ring: every AC is tier-0 for every viewer.
    AllNear,
}

impl FromStr for Distribution {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "cluster" => Ok(Self::Cluster),
            "all-near" | "all_near" | "allnear" => Ok(Self::AllNear),
            other => anyhow::bail!("unknown distribution {other} (cluster|all-near)"),
        }
    }
}

/// One headless soak request.
#[derive(Clone, Debug)]
pub struct SoakOptions {
    pub bots: usize,
    pub duration: Duration,
    pub distribution: Distribution,
    pub churn: bool,
    pub seed: u64,
    pub metrics_path: Option<PathBuf>,
}

impl Default for SoakOptions {
    fn default() -> Self {
        Self {
            bots: 64,
            duration: Duration::from_secs(60),
            distribution: Distribution::Cluster,
            churn: false,
            seed: acvd_sim::world::DEFAULT_MATCH_SEED,
            metrics_path: Some(PathBuf::from("private/soak/soak.json")),
        }
    }
}

/// A deterministic load bot. Decisions are pure functions of `(id, tick, world)`.
struct Bot {
    net_id: acvd_proto::NetId,
    seq: u16,
    home: Vec3,
    phase: u32,
}

/// Bots fed through the authoritative client queue.
pub struct BotDriver {
    bots: Vec<Bot>,
    distribution: Distribution,
    target: usize,
    next_index: usize,
}

impl BotDriver {
    /// Spawns `target` bots and places them with the requested distribution.
    pub fn spawn(
        server: &mut Server,
        target: usize,
        distribution: Distribution,
        seed: u64,
    ) -> Self {
        let mut driver = Self {
            bots: Vec::with_capacity(target),
            distribution,
            target,
            next_index: 0,
        };
        for _ in 0..target {
            driver.spawn_one(server, seed);
        }
        driver
    }

    pub fn len(&self) -> usize {
        self.bots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bots.is_empty()
    }

    pub fn ids(&self) -> Vec<acvd_proto::NetId> {
        self.bots.iter().map(|bot| bot.net_id).collect()
    }

    /// Adds one deterministic bot and snaps it to the distribution layout.
    pub fn spawn_one(&mut self, server: &mut Server, seed: u64) {
        let index = self.next_index;
        self.next_index = self.next_index.wrapping_add(1);
        let team = (index % 2) as u8;
        let Some(net_id) = server.add_bot(stats::default_loadout(), team) else {
            return;
        };
        let start = server.start();
        let (x, z) = layout(index, self.target, self.distribution, start.position);
        server.place_bot(net_id, x, z, start.yaw);
        let home = server
            .world()
            .acs
            .get(&SimNetId(net_id.get()))
            .map(|ac| ac.position)
            .unwrap_or(start.position);
        self.bots.push(Bot {
            net_id,
            seq: 1,
            home,
            phase: (u32::from(net_id.get()) * 2_654_435_761 + (seed as u32)) % 360,
        });
    }

    /// Removes one bot (oldest first) and returns its wire id.
    pub fn remove_one(&mut self, server: &mut Server) -> Option<acvd_proto::NetId> {
        if self.bots.is_empty() {
            return None;
        }
        let bot = self.bots.remove(0);
        server.remove_bot(bot.net_id);
        Some(bot.net_id)
    }

    /// Removes every bot, leaving no world/history/input remnants.
    pub fn remove_all(&mut self, server: &mut Server) {
        for bot in self.bots.drain(..) {
            server.remove_bot(bot.net_id);
        }
    }

    /// Generates and queues one frame per bot for this tick. The generated frames go through the
    /// same `InputQueue::accept` path as `InputPacket`s from a UDP client.
    pub fn enqueue_inputs(&mut self, server: &mut Server, tick: u32) -> usize {
        let world = server.world();
        let frames: Vec<(acvd_proto::NetId, u16, InputFrame)> = self
            .bots
            .iter_mut()
            .map(|bot| {
                let frame = bot.frame(world, tick);
                bot.seq = bot.seq.wrapping_add(1);
                (bot.net_id, bot.seq, frame)
            })
            .collect();
        let mut accepted = 0;
        for (net_id, seq, frame) in frames {
            if server.enqueue_bot_input(net_id, seq, frame) {
                accepted += 1;
            }
        }
        accepted
    }
}

impl Bot {
    fn frame(&self, world: &SimWorld, tick: u32) -> InputFrame {
        let mut frame = InputFrame::default();
        let Some(ac) = world.acs.get(&SimNetId(self.net_id.get())) else {
            return frame;
        };
        if !ac.alive {
            return frame;
        }

        // Patrol around home; if knocked far away, return before resuming the orbit.
        let patrol_angle = (tick as f32 / 240.0) * 2.399_963 + self.phase as f32 * 0.017_453;
        let home_distance = Vec3::new(
            ac.position.x - self.home.x,
            0.0,
            ac.position.z - self.home.z,
        )
        .length();
        let waypoint = if home_distance > 70.0 {
            self.home
        } else {
            self.home + Vec3::new(patrol_angle.cos() * 18.0, 0.0, patrol_angle.sin() * 18.0)
        };

        let move_delta = waypoint - ac.position;
        let move_yaw = yaw_towards(move_delta);
        let target = nearest_enemy(world, ac.id, ac.team);
        let target_distance = target
            .map(|target| target.position.distance(ac.position))
            .unwrap_or(f32::INFINITY);

        let cycle = tick.wrapping_add(self.phase);
        let firing = target.is_some() && target_distance < 800.0 && cycle % 180 < 2;

        let (aim_yaw, aim_pitch) = if firing {
            let target = target.expect("firing requires a target");
            (
                yaw_towards(target.position - ac.position),
                pitch_towards(target.position - ac.position),
            )
        } else {
            (move_yaw, 0.0)
        };
        frame.aim_yaw = quantize_yaw(aim_yaw);
        frame.aim_pitch = quantize_input_pitch(aim_pitch);
        frame.lock_target = target.map(|target| target.id.0).unwrap_or(0);

        // Camera-relative stick: normal ticks move toward the patrol point, firing ticks keep the
        // forward stick and simply aim at the enemy for the two-tick trigger pulse.
        let strafe = (tick as f32 * 0.045 + self.phase as f32 * 0.017_453).sin() * 0.35;
        frame.stick = [
            (strafe * 127.0).round().clamp(-127.0, 127.0) as i8,
            if home_distance > 70.0 { 127 } else { 96 },
        ];

        if firing {
            frame.buttons |= match (cycle / 180) % 3 {
                0 => input_buttons::FIRE_R,
                1 => input_buttons::FIRE_L,
                _ => input_buttons::FIRE_SHOULDER,
            };
        }

        // Boost/glide/jump/quick-boost edges. They are deliberately sparse so the 64-bot
        // all-near bandwidth gate is dominated by snapshots, not FireEvent repeats.
        let en_max = ac.motion.en_max.max(1.0);
        if !ac.motion.boost && ac.motion.en > en_max * 0.30 && (cycle % 30) < 2 {
            frame.buttons |= input_buttons::BOOST_TOGGLE;
        } else if ac.motion.boost && ac.motion.en <= en_max * 0.05 && (cycle % 120) < 2 {
            frame.buttons |= input_buttons::BOOST_TOGGLE;
        }
        if ac.motion.boost
            && !ac.motion.glide
            && home_distance < 60.0
            && target_distance > 120.0
            && (cycle % 900) < 2
        {
            frame.buttons |= input_buttons::GLIDE;
        }
        if (cycle % 240) < 2 {
            frame.buttons |= input_buttons::JUMP;
        }
        if ac.motion.en > en_max * 0.50 && (cycle % 300) < 2 {
            frame.buttons |= input_buttons::QUICK_BOOST;
        }
        if target_distance < 10.0 && (cycle % 360) < 2 {
            frame.buttons |= input_buttons::KICK;
        }

        // Occasional rack swaps exercise the replicated bay/hanger state.
        if (cycle % 420) < 2 && !ac.armament.swapping(0) {
            frame.buttons |= input_buttons::BAY_R;
        } else if (cycle % 570) < 2 && !ac.armament.swapping(1) {
            frame.buttons |= input_buttons::BAY_L;
        }

        frame
    }
}

fn nearest_enemy<'a>(
    world: &'a SimWorld,
    self_id: SimNetId,
    team: u8,
) -> Option<&'a acvd_sim::world::SimAc> {
    let me = world.acs.get(&self_id)?;
    world
        .acs
        .values()
        .filter(|ac| ac.alive && ac.id != self_id && ac.team != team)
        .min_by(|a, b| {
            a.position
                .distance_squared(me.position)
                .total_cmp(&b.position.distance_squared(me.position))
                .then(a.id.cmp(&b.id))
        })
}

fn yaw_towards(delta: Vec3) -> f32 {
    if delta.x.abs() < 1e-4 && delta.z.abs() < 1e-4 {
        return 0.0;
    }
    (-delta.x).atan2(-delta.z)
}

fn pitch_towards(delta: Vec3) -> f32 {
    let length = delta.length().max(1e-4);
    (delta.y / length)
        .clamp(-1.0, 1.0)
        .asin()
        .clamp(-0.55, 0.55)
}

/// XZ layout for one bot. `AllNear` keeps every AC inside an 82 m ring (all tier 0); `Cluster`
/// puts eight bots around each of eight centers, four per team.
fn layout(index: usize, total: usize, distribution: Distribution, start: Vec3) -> (f32, f32) {
    match distribution {
        Distribution::AllNear => {
            let count = total.max(1) as f32;
            let angle = (index % total.max(1)) as f32 / count * std::f32::consts::TAU;
            let radius = 6.0 + (index % 8) as f32 * 5.0;
            (
                start.x + angle.cos() * radius,
                start.z + angle.sin() * radius,
            )
        }
        Distribution::Cluster => {
            let cluster = index % 8;
            let member = (index / 8) % 8;
            let centers = [
                (-240.0, -200.0),
                (-240.0, 200.0),
                (-80.0, -300.0),
                (-80.0, 300.0),
                (240.0, -200.0),
                (240.0, 200.0),
                (80.0, -300.0),
                (80.0, 300.0),
            ];
            let (cx, cz) = centers[cluster];
            let angle = member as f32 / 8.0 * std::f32::consts::TAU + cluster as f32 * 0.4;
            let radius = 8.0 + (member % 3) as f32 * 4.0;
            (
                start.x + cx + angle.cos() * radius,
                start.z + cz + angle.sin() * radius,
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize)]
pub struct TimingSummary {
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
    pub mean_us: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct TimingMetrics {
    pub sim: TimingSummary,
    pub snapshot: TimingSummary,
    pub encode: TimingSummary,
    pub net: TimingSummary,
    pub total: TimingSummary,
    pub driver: TimingSummary,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct BandwidthMetrics {
    pub per_client_avg_kbps: Vec<f64>,
    pub per_client_avg_kbps_mean: f64,
    pub per_client_avg_kbps_max: f64,
    pub per_client_peak_kbps_max: f64,
    pub server_avg_mbps: f64,
    pub server_peak_mbps: f64,
    pub total_egress_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct SnapshotMetrics {
    pub max_packet_bytes: u16,
    pub max_packets_per_tick: u8,
    pub snapshot_ticks: u64,
    pub packets_total: u64,
    pub missing_local: u64,
    pub dropped_records: u64,
    pub dropped_corrections: u64,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct LivenessMetrics {
    pub non_finite: u64,
    pub orphan_acs_max: usize,
    /// A snapshot without `LocalPlayerState` in packet 1 is a protocol desync.
    pub snapshot_desync: u64,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct EntityMetrics {
    pub acs_min: usize,
    pub acs_max: usize,
    pub acs_final: usize,
    pub clients_min: usize,
    pub clients_max: usize,
    pub clients_final: usize,
    pub projectiles_max: usize,
    pub missiles_max: usize,
    pub pending_fires_max: usize,
    pub history_max: usize,
    pub queued_inputs_max: usize,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct MemoryMetrics {
    /// `None` when the platform does not expose a process RSS value.
    pub rss_start_kb: Option<u64>,
    pub rss_end_kb: Option<u64>,
    pub rss_peak_kb: Option<u64>,
    pub rss_growth_kb: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct QueueMetrics {
    pub accepted: u64,
    pub duplicates: u64,
    pub stale: u64,
    pub too_far: u64,
    pub starved_ticks: u64,
    pub idle_ticks: u64,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct ChurnMetrics {
    pub enabled: bool,
    pub joins: u64,
    pub leaves: u64,
    pub active_min: usize,
    pub active_max: usize,
    pub active_final: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct Assertion {
    pub name: String,
    pub pass: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SoakMetrics {
    pub mode: String,
    pub distribution: Distribution,
    pub bots: usize,
    pub churn: bool,
    pub seed: u64,
    pub ticks: u64,
    pub duration_s: f64,
    pub completed: bool,
    pub timings: TimingMetrics,
    pub bandwidth: BandwidthMetrics,
    pub snapshots: SnapshotMetrics,
    pub liveness: LivenessMetrics,
    pub entities: EntityMetrics,
    pub memory: MemoryMetrics,
    pub queues: QueueMetrics,
    pub churn_metrics: ChurnMetrics,
    pub cleanup: crate::server::EntityCounts,
    pub kills: u64,
    pub respawns: u64,
    pub fire_events: u64,
    pub combat_packets: u64,
    pub assertions: Vec<Assertion>,
}

impl SoakMetrics {
    pub fn failures(&self) -> Vec<&Assertion> {
        self.assertions.iter().filter(|a| !a.pass).collect()
    }

    fn evaluate(&mut self) {
        let mut assertions = Vec::new();
        let mut push = |name: &str, pass: bool, detail: String| {
            assertions.push(Assertion {
                name: name.into(),
                pass,
                detail,
            });
        };

        push(
            "snapshot_mtu",
            self.snapshots.max_packet_bytes as usize <= MAX_SNAPSHOT_BYTES,
            format!(
                "max packet {} B <= {MAX_SNAPSHOT_BYTES} B",
                self.snapshots.max_packet_bytes
            ),
        );
        push(
            "snapshot_packets",
            self.snapshots.max_packets_per_tick <= MAX_PACKETS_PER_SNAPSHOT,
            format!(
                "max {} packets/tick <= {MAX_PACKETS_PER_SNAPSHOT}",
                self.snapshots.max_packets_per_tick
            ),
        );
        push(
            "local_in_packet_one",
            self.snapshots.missing_local == 0,
            format!(
                "{} snapshots without LocalPlayerState in packet 1",
                self.snapshots.missing_local
            ),
        );
        push(
            "finite_world",
            self.liveness.non_finite == 0,
            format!(
                "{} non-finite positions/velocities",
                self.liveness.non_finite
            ),
        );
        push(
            "no_desync",
            self.liveness.snapshot_desync == 0,
            format!("{} snapshot desyncs", self.liveness.snapshot_desync),
        );
        push(
            "no_orphan_acs",
            self.liveness.orphan_acs_max == 0,
            format!("max {} orphan ACs", self.liveness.orphan_acs_max),
        );
        push(
            "no_dropped_records",
            self.snapshots.dropped_records == 0 && self.snapshots.dropped_corrections == 0,
            format!(
                "dropped {} records / {} corrections",
                self.snapshots.dropped_records, self.snapshots.dropped_corrections
            ),
        );
        push(
            "cleanup_no_leaks",
            self.cleanup.acs == 0
                && self.cleanup.clients == 0
                && self.cleanup.projectiles == 0
                && self.cleanup.missiles == 0
                && self.cleanup.pending_fires == 0
                && self.cleanup.combat_pending == 0
                && self.cleanup.history_entries == 0,
            format!("cleanup counts {:?}", self.cleanup),
        );

        if self.bots >= 64 && !self.churn {
            push(
                "tick_p95",
                self.timings.total.p95_us < BUDGET_P95_US,
                format!("p95 {} us < {BUDGET_P95_US} us", self.timings.total.p95_us),
            );
            push(
                "tick_p99",
                self.timings.total.p99_us < BUDGET_P99_US,
                format!("p99 {} us < {BUDGET_P99_US} us", self.timings.total.p99_us),
            );
            push(
                "client_avg_bandwidth",
                self.bandwidth.per_client_avg_kbps_max < BUDGET_CLIENT_AVG_KBPS,
                format!(
                    "worst client average {:.1} kbps < {BUDGET_CLIENT_AVG_KBPS} kbps",
                    self.bandwidth.per_client_avg_kbps_max
                ),
            );
            push(
                "client_peak_bandwidth",
                self.bandwidth.per_client_peak_kbps_max < BUDGET_CLIENT_PEAK_KBPS,
                format!(
                    "worst 1 s client peak {:.1} kbps < {BUDGET_CLIENT_PEAK_KBPS} kbps",
                    self.bandwidth.per_client_peak_kbps_max
                ),
            );
            push(
                "server_egress",
                self.bandwidth.server_avg_mbps < BUDGET_SERVER_MBPS,
                format!(
                    "server egress {:.2} Mbps < {BUDGET_SERVER_MBPS} Mbps",
                    self.bandwidth.server_avg_mbps
                ),
            );
        }

        if self.churn {
            push(
                "churn_entity_stability",
                self.liveness.orphan_acs_max == 0
                    && self.entities.queued_inputs_max <= 2
                    && self.entities.active_ac_count_stable(),
                format!(
                    "acs {}..{}, clients {}..{}, queued max {}, orphan max {}",
                    self.entities.acs_min,
                    self.entities.acs_max,
                    self.entities.clients_min,
                    self.entities.clients_max,
                    self.entities.queued_inputs_max,
                    self.liveness.orphan_acs_max
                ),
            );
            let memory_pass = self
                .memory
                .rss_growth_kb
                .is_none_or(|growth| growth < BUDGET_MEMORY_GROWTH_KB as i64);
            push(
                "memory_stable",
                memory_pass,
                format!(
                    "RSS growth {:?} KB < {BUDGET_MEMORY_GROWTH_KB} KB",
                    self.memory.rss_growth_kb
                ),
            );
            push(
                "churn_final_count",
                self.churn_metrics.active_final == self.bots,
                format!(
                    "final active {} == target {}",
                    self.churn_metrics.active_final, self.bots
                ),
            );
        }
        self.assertions = assertions;
    }
}

impl EntityMetrics {
    fn active_ac_count_stable(&self) -> bool {
        // One bot is briefly absent during a churn gap; waves remove at most three.
        self.acs_min + 3 >= self.clients_max && self.acs_max <= self.clients_max + 3
    }
}

// ---------------------------------------------------------------------------
// Soak loop
// ---------------------------------------------------------------------------

#[derive(Default)]
struct TimingSamples {
    sim: Vec<u64>,
    snapshot: Vec<u64>,
    encode: Vec<u64>,
    net: Vec<u64>,
    total: Vec<u64>,
    driver: Vec<u64>,
}

impl TimingSamples {
    fn record(&mut self, timings: crate::server::StepTimings, driver_us: u64) {
        self.sim.push(timings.sim_us);
        self.snapshot.push(timings.snapshot_us);
        self.encode.push(timings.encode_us);
        self.net.push(timings.net_us);
        self.total.push(timings.total_us);
        self.driver.push(driver_us);
    }

    fn finish(self) -> TimingMetrics {
        TimingMetrics {
            sim: summarize(self.sim),
            snapshot: summarize(self.snapshot),
            encode: summarize(self.encode),
            net: summarize(self.net),
            total: summarize(self.total),
            driver: summarize(self.driver),
        }
    }
}

fn summarize(mut samples: Vec<u64>) -> TimingSummary {
    if samples.is_empty() {
        return TimingSummary::default();
    }
    samples.sort_unstable();
    let percentile = |p: f64| {
        let index = ((samples.len() - 1) as f64 * p).round() as usize;
        samples[index.min(samples.len() - 1)]
    };
    TimingSummary {
        p50_us: percentile(0.50),
        p95_us: percentile(0.95),
        p99_us: percentile(0.99),
        max_us: *samples.last().unwrap(),
        mean_us: samples.iter().map(|v| *v as f64).sum::<f64>() / samples.len() as f64,
    }
}

/// Runs one headless soak against a bound [`Server`].
pub fn run_soak(
    mut server: Server,
    options: SoakOptions,
    shutdown: Option<&AtomicBool>,
) -> Result<SoakMetrics> {
    if options.bots == 0 {
        anyhow::bail!("soak needs at least one bot");
    }
    if options.bots > BOT_COUNT_MAX {
        anyhow::bail!("at most {BOT_COUNT_MAX} bots are supported");
    }
    log::info!(
        "soak: {} bots, {} distribution, churn {}, {} s, listening on {}",
        options.bots,
        if options.distribution == Distribution::AllNear {
            "all-near"
        } else {
            "cluster"
        },
        options.churn,
        options.duration.as_secs_f64(),
        server.local_addr()
    );

    let mut driver = BotDriver::spawn(
        &mut server,
        options.bots,
        options.distribution,
        options.seed,
    );
    let total_ticks = (options.duration.as_secs_f64() * 60.0).round().max(1.0) as u64;

    let rss_start = process_rss_kb();
    let mut rss_peak = rss_start;
    let mut timings = TimingSamples::default();
    let mut per_second: HashMap<u8, u64> = HashMap::new();
    let mut per_client_peak_kbps: HashMap<u8, f64> = HashMap::new();
    let mut total_sent_bytes = 0u64;
    let mut peak_server_mbps = 0.0f64;
    let mut nan = 0u64;
    let mut desync = 0u64;
    let mut orphan_max = 0usize;
    let mut queued_max = 0usize;
    let mut entities = EntityMetrics::default();
    entities.acs_min = usize::MAX;
    entities.clients_min = usize::MAX;
    let mut pending_spawns: VecDeque<u64> = VecDeque::new();
    let mut churn = ChurnMetrics {
        enabled: options.churn,
        active_max: driver.len(),
        active_min: driver.len(),
        ..ChurnMetrics::default()
    };
    let mut ticks_done = 0u64;
    let mut completed = true;

    for tick in 0..total_ticks {
        if shutdown.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            completed = false;
            break;
        }

        if options.churn {
            if tick > 0 && tick % CHURN_REMOVE_EVERY == 0 && driver.len() > 1 {
                if let Some(id) = driver.remove_one(&mut server) {
                    per_second.remove(&id.get());
                    per_client_peak_kbps.remove(&id.get());
                    churn.leaves = churn.leaves.saturating_add(1);
                    pending_spawns.push_back(tick + CHURN_GAP_TICKS);
                }
            }
            if tick > 0 && tick % CHURN_WAVE_EVERY == 0 {
                for i in 0..2 {
                    if driver.len() <= 1 {
                        break;
                    }
                    if let Some(id) = driver.remove_one(&mut server) {
                        per_second.remove(&id.get());
                        per_client_peak_kbps.remove(&id.get());
                        churn.leaves = churn.leaves.saturating_add(1);
                        pending_spawns.push_back(tick + CHURN_GAP_TICKS + i * 10);
                    }
                }
            }
            while pending_spawns.front().is_some_and(|due| *due <= tick) {
                pending_spawns.pop_front();
                let before = driver.len();
                driver.spawn_one(&mut server, options.seed);
                if driver.len() > before {
                    churn.joins = churn.joins.saturating_add(1);
                }
            }
            churn.active_min = churn.active_min.min(driver.len());
            churn.active_max = churn.active_max.max(driver.len());
        }

        let driver_started = Instant::now();
        driver.enqueue_inputs(&mut server, tick as u32);
        let driver_us = driver_started.elapsed().as_micros() as u64;
        // Before the step every live bot has exactly one queued frame; after it the queue is
        // drained. A growing value here is a leaked/duplicated input queue.
        queued_max = queued_max.max(server.max_queued_inputs());

        server.step(TICK)?;
        ticks_done = tick + 1;
        timings.record(server.last_timings(), driver_us);

        nan = nan.saturating_add(non_finite(&server));
        orphan_max = orphan_max.max(server.orphan_acs());
        queued_max = queued_max.max(server.max_queued_inputs());
        let counts = server.entity_counts();
        entities.acs_final = counts.acs;
        entities.clients_final = counts.clients;
        entities.acs_min = entities.acs_min.min(counts.acs);
        entities.acs_max = entities.acs_max.max(counts.acs);
        entities.clients_min = entities.clients_min.min(counts.clients);
        entities.clients_max = entities.clients_max.max(counts.clients);
        entities.projectiles_max = entities.projectiles_max.max(counts.projectiles);
        entities.missiles_max = entities.missiles_max.max(counts.missiles);
        entities.pending_fires_max = entities.pending_fires_max.max(counts.pending_fires);
        entities.history_max = entities.history_max.max(counts.history_entries);

        if (tick + 1) % 60 == 0 {
            let rss = process_rss_kb();
            if let (Some(current), Some(peak)) = (rss, rss_peak) {
                rss_peak = Some(peak.max(current));
            } else if rss_peak.is_none() {
                rss_peak = rss;
            }

            let mut second_total = 0u64;
            let ids = driver.ids();
            for id in &ids {
                let now = server
                    .client_egress(*id)
                    .map(|egress| egress.bytes)
                    .unwrap_or(0);
                let previous = per_second.insert(id.get(), now).unwrap_or(0);
                let delta = now.saturating_sub(previous);
                second_total = second_total.saturating_add(delta);
                let kbps = delta as f64 * 8.0 / 1000.0;
                let peak = per_client_peak_kbps.entry(id.get()).or_insert(0.0);
                *peak = peak.max(kbps);
            }
            total_sent_bytes = total_sent_bytes.saturating_add(second_total);
            peak_server_mbps = peak_server_mbps.max(second_total as f64 * 8.0 / 1_000_000.0);

            if options.churn {
                log::info!(
                    "soak t={:.0}s acs={} clients={} rss={:?} KB",
                    (tick + 1) as f64 / 60.0,
                    counts.acs,
                    counts.clients,
                    rss
                );
            }
        }
    }

    while let Some(_) = pending_spawns.pop_front() {
        let before = driver.len();
        driver.spawn_one(&mut server, options.seed);
        if driver.len() > before {
            churn.joins = churn.joins.saturating_add(1);
        }
    }
    if ticks_done == 0 {
        entities.acs_min = 0;
        entities.clients_min = 0;
    }
    churn.active_final = driver.len();
    churn.active_max = churn.active_max.max(driver.len());
    let final_entities = server.entity_counts();

    let elapsed_s = (ticks_done as f64 / 60.0).max(1.0 / 60.0);
    let mut per_client_avg = Vec::new();
    let mut max_packet = 0u16;
    let mut max_packets = 0u8;
    let mut snapshot_ticks = 0u64;
    let mut packets_total = 0u64;
    let mut missing_local = 0u64;
    for id in driver.ids() {
        if let Some(egress) = server.client_egress(id) {
            per_client_avg.push(egress.bytes as f64 * 8.0 / 1000.0 / elapsed_s);
            max_packet = max_packet.max(egress.max_snapshot_packet);
            max_packets = max_packets.max(egress.max_packets_per_tick);
            snapshot_ticks = snapshot_ticks.max(egress.snapshot_ticks);
            packets_total = packets_total.saturating_add(egress.snapshot_packets);
            missing_local = missing_local.saturating_add(egress.missing_local);
        }
    }
    if total_sent_bytes == 0 {
        total_sent_bytes = server.egress_bytes();
    }
    let avg_mean = if per_client_avg.is_empty() {
        0.0
    } else {
        per_client_avg.iter().sum::<f64>() / per_client_avg.len() as f64
    };
    let avg_max = per_client_avg.iter().copied().fold(0.0, f64::max);
    let peak_max = per_client_peak_kbps.values().copied().fold(0.0, f64::max);

    let queue_totals = server.queue_totals();
    desync = desync.saturating_add(missing_local);

    // Remove every bot, then advance 15 s so in-flight rounds/missiles expire. Anything left is
    // a leaked AC, input queue, history entry, or channel message.
    let cleanup = {
        driver.remove_all(&mut server);
        for _ in 0..CLEANUP_TICKS {
            server.step(TICK)?;
        }
        server.entity_counts()
    };

    let mut metrics = SoakMetrics {
        mode: if options.churn {
            "churn".into()
        } else {
            "soak".into()
        },
        distribution: options.distribution,
        bots: options.bots,
        churn: options.churn,
        seed: options.seed,
        ticks: ticks_done,
        duration_s: elapsed_s,
        completed,
        timings: timings.finish(),
        bandwidth: BandwidthMetrics {
            per_client_avg_kbps: per_client_avg,
            per_client_avg_kbps_mean: avg_mean,
            per_client_avg_kbps_max: avg_max,
            per_client_peak_kbps_max: peak_max,
            server_avg_mbps: total_sent_bytes as f64 * 8.0 / 1_000_000.0 / elapsed_s,
            server_peak_mbps: peak_server_mbps,
            total_egress_bytes: total_sent_bytes,
        },
        snapshots: SnapshotMetrics {
            max_packet_bytes: max_packet,
            max_packets_per_tick: max_packets,
            snapshot_ticks,
            packets_total,
            missing_local,
            dropped_records: server.dropped_snapshot_records(),
            dropped_corrections: server.dropped_missile_corrections(),
        },
        liveness: LivenessMetrics {
            non_finite: nan,
            orphan_acs_max: orphan_max,
            snapshot_desync: desync,
        },
        entities: EntityMetrics {
            acs_final: final_entities.acs,
            clients_final: final_entities.clients,
            history_max: entities.history_max,
            queued_inputs_max: queued_max,
            ..entities
        },
        memory: MemoryMetrics {
            rss_start_kb: rss_start,
            rss_end_kb: process_rss_kb(),
            rss_peak_kb: rss_peak,
            rss_growth_kb: match (rss_start, process_rss_kb()) {
                (Some(start), Some(end)) => Some(end as i64 - start as i64),
                _ => None,
            },
        },
        queues: QueueMetrics {
            accepted: queue_totals.accepted,
            duplicates: queue_totals.duplicates,
            stale: queue_totals.stale,
            too_far: queue_totals.too_far,
            starved_ticks: queue_totals.starved_ticks,
            idle_ticks: queue_totals.idle_ticks,
        },
        churn_metrics: churn,
        cleanup,
        kills: server.kill_count(),
        respawns: server.respawn_count(),
        fire_events: server.hot_fire_events_sent(),
        combat_packets: server.combat_packets_sent(),
        assertions: Vec::new(),
    };
    metrics.evaluate();

    if let Some(path) = &options.metrics_path {
        write_metrics(path, &metrics)?;
        log::info!("soak metrics written to {}", path.display());
    }
    server.shutdown();
    Ok(metrics)
}

fn non_finite(server: &Server) -> u64 {
    let world = server.world();
    let mut count = 0u64;
    for ac in world.acs.values() {
        if !ac.position.is_finite()
            || !ac.motion.velocity.is_finite()
            || !ac.motion.yaw.is_finite()
            || !ac.ap.is_finite()
        {
            count += 1;
        }
    }
    for projectile in world.projectiles.values() {
        if !projectile.position.is_finite() || !projectile.velocity.is_finite() {
            count += 1;
        }
    }
    for missile in world.missiles.values() {
        if !missile.position.is_finite() || !missile.velocity.is_finite() {
            count += 1;
        }
    }
    count
}

fn write_metrics(path: &Path, metrics: &SoakMetrics) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create soak metrics directory {}", parent.display()))?;
    }
    let json = serde_json::to_vec_pretty(metrics).context("encode soak metrics JSON")?;
    fs::write(path, json).with_context(|| format!("write soak metrics {}", path.display()))?;
    Ok(())
}

/// Current process resident set size, if the platform exposes it.
#[cfg(windows)]
fn process_rss_kb() -> Option<u64> {
    #[repr(C)]
    #[allow(non_snake_case)]
    struct ProcessMemoryCounters {
        cb: u32,
        PageFaultCount: u32,
        PeakWorkingSetSize: usize,
        WorkingSetSize: usize,
        QuotaPeakPagedPoolUsage: usize,
        QuotaPagedPoolUsage: usize,
        QuotaPeakNonPagedPoolUsage: usize,
        QuotaNonPagedPoolUsage: usize,
        PagefileUsage: usize,
        PeakPagefileUsage: usize,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(
            process: isize,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }

    let mut counters = ProcessMemoryCounters {
        cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
        PageFaultCount: 0,
        PeakWorkingSetSize: 0,
        WorkingSetSize: 0,
        QuotaPeakPagedPoolUsage: 0,
        QuotaPagedPoolUsage: 0,
        QuotaPeakNonPagedPoolUsage: 0,
        QuotaNonPagedPoolUsage: 0,
        PagefileUsage: 0,
        PeakPagefileUsage: 0,
    };
    // SAFETY: the counter struct is plain-old-data and `cb` is its exact size.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    (ok != 0).then_some(counters.WorkingSetSize as u64 / 1024)
}

#[cfg(target_os = "linux")]
fn process_rss_kb() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(value) = line.strip_prefix("VmRSS:") {
            return value
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<u64>().ok());
        }
    }
    None
}

#[cfg(not(any(windows, target_os = "linux")))]
fn process_rss_kb() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_distributions_are_bounded() {
        let start = Vec3::ZERO;
        let near: Vec<(f32, f32)> = (0..64)
            .map(|i| layout(i, 64, Distribution::AllNear, start))
            .collect();
        for a in &near {
            for b in &near {
                let distance = ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
                assert!(distance <= 150.0, "all-near distance {distance}");
            }
        }
        let clusters: Vec<(f32, f32)> = (0..64)
            .map(|i| layout(i, 64, Distribution::Cluster, start))
            .collect();
        let max = clusters
            .iter()
            .map(|(x, z)| (x * x + z * z).sqrt())
            .fold(0.0, f32::max);
        assert!(max > 200.0, "clusters are spread across the arena");
    }

    #[test]
    fn nearest_enemy_uses_team_and_alive() {
        let loadout = stats::default_loadout();
        let mut world = SimWorld::new();
        let mut a = acvd_sim::world::SimAc::new(SimNetId(1), loadout, Vec3::ZERO, 0.0, 0);
        a.alive = true;
        world.insert(a);
        let mut ally = acvd_sim::world::SimAc::new(SimNetId(2), loadout, Vec3::X, 0.0, 0);
        ally.alive = true;
        world.insert(ally);
        let mut enemy = acvd_sim::world::SimAc::new(SimNetId(3), loadout, Vec3::Z * 5.0, 0.0, 1);
        enemy.alive = true;
        world.insert(enemy);
        assert_eq!(
            nearest_enemy(&world, SimNetId(1), 0).map(|ac| ac.id),
            Some(SimNetId(3))
        );
    }

    #[test]
    fn yaw_points_at_delta() {
        let yaw = yaw_towards(Vec3::new(0.0, 0.0, -10.0));
        assert!(yaw.abs() < 1e-5);
        let yaw = yaw_towards(Vec3::new(10.0, 0.0, 0.0));
        assert!((yaw - (-std::f32::consts::FRAC_PI_2)).abs() < 1e-5);
    }
}
