//! Phase 4 soak gates.
//!
//! The non-ignored smoke tests run the real bot -> InputQueue -> SimWorld -> snapshot/encode path
//! with small counts so CI catches entity/input/channel leaks quickly. The `#[ignore]` release
//! gates are the 64-AC cluster/all-near timing/bandwidth assertions and the 24-player, 10-minute
//! churn memory check. Run them serially in release:
//!
//! ```text
//! cargo test -p acvd-server --release -- --ignored soak --test-threads=1
//! ```

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use acvd_server::bots::{run_soak, Distribution, SoakMetrics, SoakOptions, BUDGET_P95_US};
use acvd_server::{Server, ServerConfig};

fn metrics_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("private/soak")
        .join(name)
}

fn config(bots: usize) -> ServerConfig {
    ServerConfig {
        bind: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        max_clients: bots.max(24),
        allow_missing_disc: true,
        snapshot_every: 2,
        ..ServerConfig::default()
    }
}

fn options(bots: usize, ticks: u64, distribution: Distribution, churn: bool) -> SoakOptions {
    SoakOptions {
        bots,
        duration: Duration::from_secs_f64(ticks as f64 / 60.0),
        distribution,
        churn,
        seed: acvd_sim::world::DEFAULT_MATCH_SEED,
        metrics_path: None,
    }
}

fn run(bots: usize, ticks: u64, distribution: Distribution, churn: bool) -> SoakMetrics {
    let server = Server::bind(config(bots)).expect("bind soak server");
    run_soak(server, options(bots, ticks, distribution, churn), None).expect("soak")
}

fn assert_common(metrics: &SoakMetrics) {
    let failures = metrics.failures();
    assert!(failures.is_empty(), "soak assertions failed: {failures:#?}");
    assert!(metrics.ticks > 0);
    assert!(metrics.snapshots.snapshot_ticks > 0);
    assert!(metrics.snapshots.max_packet_bytes as usize <= acvd_proto::MAX_SNAPSHOT_BYTES);
    assert!(metrics.snapshots.max_packets_per_tick <= 2);
    assert_eq!(metrics.cleanup.acs, 0);
    assert_eq!(metrics.cleanup.clients, 0);
    assert_eq!(metrics.cleanup.history_entries, 0);
}

#[test]
fn smoke_8_all_near_real_input_snapshot_path() {
    let metrics = run(8, 180, Distribution::AllNear, false);
    assert_common(&metrics);
    assert!(metrics.fire_events > 0, "bots must trigger real weapons");
    assert_eq!(metrics.entities.acs_final, 8);
    assert_eq!(metrics.entities.clients_final, 8);
    assert!(metrics.entities.queued_inputs_max <= 2);
    assert!(metrics.liveness.non_finite == 0);
    assert_eq!(metrics.snapshots.missing_local, 0);
    assert!(metrics.bandwidth.total_egress_bytes > 0);
    let json = serde_json::to_string(&metrics).expect("metrics JSON");
    assert!(json.contains("\"snapshots\""));
}

#[test]
fn smoke_12_player_churn_reclaims_everything() {
    let metrics = run(12, 420, Distribution::AllNear, true);
    assert_common(&metrics);
    assert!(metrics.churn_metrics.joins > 0);
    assert!(metrics.churn_metrics.leaves > 0);
    assert_eq!(metrics.churn_metrics.active_final, 12);
    assert_eq!(metrics.cleanup.acs, 0);
    assert_eq!(metrics.cleanup.projectiles, 0);
    assert_eq!(metrics.cleanup.missiles, 0);
    assert_eq!(metrics.cleanup.pending_fires, 0);
    assert_eq!(metrics.cleanup.combat_pending, 0);
}

#[test]
fn smoke_16_cluster_stays_healthy() {
    let metrics = run(16, 180, Distribution::Cluster, false);
    assert_common(&metrics);
    assert!(metrics.fire_events > 0);
    assert_eq!(metrics.entities.acs_final, 16);
    assert!(metrics.timings.total.max_us > 0);
}

fn release_gate(metrics: &SoakMetrics, label: &str) {
    assert_common(metrics);
    assert!(
        metrics.timings.total.p95_us < BUDGET_P95_US,
        "{label}: p95 {} us",
        metrics.timings.total.p95_us
    );
    assert!(
        metrics.timings.total.p99_us < 8_000,
        "{label}: p99 {} us",
        metrics.timings.total.p99_us
    );
    assert!(metrics.liveness.non_finite == 0, "{label}: NaN");
    assert_eq!(metrics.snapshots.missing_local, 0, "{label}: local state");
    assert_eq!(metrics.snapshots.dropped_records, 0, "{label}: tier drops");
    assert!(
        metrics.bandwidth.per_client_avg_kbps_max < 500.0,
        "{label}: client avg {:.1} kbps",
        metrics.bandwidth.per_client_avg_kbps_max
    );
    assert!(
        metrics.bandwidth.per_client_peak_kbps_max < 1_500.0,
        "{label}: client peak {:.1} kbps",
        metrics.bandwidth.per_client_peak_kbps_max
    );
    assert!(
        metrics.bandwidth.server_avg_mbps < 35.0,
        "{label}: server {:.2} Mbps",
        metrics.bandwidth.server_avg_mbps
    );
}

#[test]
#[ignore = "release 64-bot cluster timing/bandwidth gate"]
fn soak_64_cluster_release_budget() {
    let server = Server::bind(config(64)).expect("bind");
    let mut options = options(64, 1_800, Distribution::Cluster, false);
    options.metrics_path = Some(metrics_path("test_64_cluster.json"));
    let metrics = run_soak(server, options, None).expect("soak");
    release_gate(&metrics, "cluster");
}

#[test]
#[ignore = "release 64-bot all-near worst-case timing/bandwidth gate"]
fn soak_64_all_near_release_budget() {
    let server = Server::bind(config(64)).expect("bind");
    let mut options = options(64, 1_800, Distribution::AllNear, false);
    options.metrics_path = Some(metrics_path("test_64_all_near.json"));
    let metrics = run_soak(server, options, None).expect("soak");
    release_gate(&metrics, "all-near");
}

#[test]
#[ignore = "release 24-player 10-minute churn memory/leak gate"]
fn soak_24_churn_release_memory() {
    let server = Server::bind(config(24)).expect("bind");
    let mut options = options(24, 36_000, Distribution::Cluster, true);
    options.metrics_path = Some(metrics_path("test_24_churn.json"));
    let metrics = run_soak(server, options, None).expect("soak");
    assert_common(&metrics);
    assert_eq!(metrics.churn_metrics.active_final, 24);
    assert_eq!(metrics.churn_metrics.joins, metrics.churn_metrics.leaves);
    assert!(metrics.churn_metrics.joins > 100);
    assert!(metrics.memory.rss_growth_kb.is_none_or(|g| g < 128 * 1024));
}
