//! Dedicated ACVD server binary.
//!
//! Usage:
//!   acvd-server [--bind 0.0.0.0:5000] [--max-clients 24] [--protocol-id N]
//!               [--map m4000] [--disc <ACV2.pe>] [--bots N]
//!               [--distribution cluster|all-near] [--seed N]
//!   acvd-server --bots N --soak [--duration S] [--metrics <json>]
//!               [--distribution cluster|all-near] [--churn] [--seed N]
//!
//! `--bots N` in normal mode adds N real-time bot clients to the match and reserves one extra
//! slot, so a human `--connect` joins as player N+1. `--soak` is the headless load gate; the
//! phase-4 invocation (`--bots N --duration S --metrics <json>`) still selects it.
//!
//! The owned disc (and the generated tables compiled into `acvd-data`) is never bundled; pass
//! `--disc` or set `ACVD_DISC`. Missing data fails fast with a clear message.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use acvd_server::bots::{Distribution, SoakOptions};
use acvd_server::{run_with_bots, Server, ServerConfig};

fn main() -> ExitCode {
    if let Err(err) = real_main() {
        eprintln!("acvd-server: {err:#}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn real_main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let mut config = ServerConfig::default();
    let mut disc: Option<PathBuf> = None;
    let mut bots = 0usize;
    let mut duration = 60u64;
    let mut duration_given = false;
    let mut metrics = PathBuf::from("private/soak/soak.json");
    let mut metrics_given = false;
    let mut soak = false;
    let mut distribution = Distribution::Cluster;
    let mut churn = false;
    let mut seed = acvd_sim::world::DEFAULT_MATCH_SEED;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bind" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--bind needs an address"))?;
                config.bind = value.parse::<SocketAddr>()?;
            }
            "--max-clients" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--max-clients needs a number"))?;
                config.max_clients = value.parse()?;
            }
            "--protocol-id" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--protocol-id needs a number"))?;
                config.protocol_id = value.parse()?;
            }
            "--map" => {
                config.map = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--map needs a map id"))?;
            }
            "--disc" => {
                disc = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--disc needs a path"))?,
                ));
            }
            "--bots" => {
                bots = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--bots needs a number"))?
                    .parse()?;
            }
            "--duration" => {
                duration = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--duration needs seconds"))?
                    .parse()?;
                duration_given = true;
            }
            "--metrics" => {
                metrics = PathBuf::from(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--metrics needs a path"))?,
                );
                metrics_given = true;
            }
            "--soak" | "--headless" => soak = true,
            "--distribution" => {
                distribution = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--distribution needs cluster|all-near"))?
                    .parse()?;
            }
            "--churn" => churn = true,
            "--seed" => {
                seed = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--seed needs a number"))?
                    .parse()?;
            }
            "-h" | "--help" => {
                println!(
                    "acvd-server [--bind 0.0.0.0:5000] [--max-clients 24] [--protocol-id N]\n\
                     \x20            [--map m4000] [--disc <ACV2.pe>] [--bots N]\n\
                     \x20            [--distribution cluster|all-near] [--seed N]\n\
                     acvd-server --bots N --soak [--duration S] [--metrics <json>]\n\
                     \x20            [--distribution cluster|all-near] [--churn] [--seed N]\n\n\
                     --bots N adds live bots to a real-time match and reserves one extra client\n\
                     slot, so a human --connect joins as player N+1. --soak runs the headless\n\
                     load gate. Missing disc/generated data fails fast; the owned disc is never\n\
                     bundled. Bot soak without --disc uses the synthetic arena."
                );
                return Ok(());
            }
            other => anyhow::bail!("unknown argument {other} (try --help)"),
        }
    }
    config.disc = disc;
    config.allow_missing_disc = false;

    let shutdown = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&shutdown);
    ctrlc::set_handler(move || {
        flag.store(true, Ordering::Relaxed);
    })
    .map_err(|e| anyhow::anyhow!("install Ctrl-C handler: {e}"))?;

    // Live mode is the default: `--bots N` joins N real-time bots to a normal server that
    // accepts `--connect` clients. The headless gate is explicit via `--soak`; the phase-4
    // invocation (`--bots N --duration S --metrics <json>`) still selects it for compatibility.
    let soak = soak || (bots > 0 && (duration_given || metrics_given));

    if soak {
        if bots == 0 {
            anyhow::bail!("--soak needs --bots N");
        }
        if bots > 64 {
            anyhow::bail!("at most 64 bots are supported");
        }
        // Bot mode is a load generator: without an explicit disc it uses the synthetic arena so
        // the plan's `--bots 64 --duration 600 --metrics` command works headlessly. Pass --disc
        // (or ACVD_DISC) to run the same gate on the real map.
        if config.disc.is_none() && std::env::var_os("ACVD_DISC").is_none() {
            config.allow_missing_disc = true;
            log::warn!("no disc supplied; bot soak uses the synthetic test arena");
        }
        config.max_clients = config.max_clients.max(bots.min(64));
        let server = Server::bind(config)?;
        let options = SoakOptions {
            bots,
            duration: Duration::from_secs(duration.max(1)),
            distribution,
            churn,
            seed,
            metrics_path: Some(metrics),
        };
        let result = acvd_server::bots::run_soak(server, options, Some(&shutdown))?;
        println!(
            "soak: {} bots {} ticks ({:.0} s), total p95 {} us p99 {} us, \
             client avg max {:.1} kbps peak {:.1} kbps, server {:.2} Mbps, \
             max packet {} B, {} fire events, {} kills",
            result.bots,
            result.ticks,
            result.duration_s,
            result.timings.total.p95_us,
            result.timings.total.p99_us,
            result.bandwidth.per_client_avg_kbps_max,
            result.bandwidth.per_client_peak_kbps_max,
            result.bandwidth.server_avg_mbps,
            result.snapshots.max_packet_bytes,
            result.fire_events,
            result.kills,
        );
        let failures = result.failures();
        if !failures.is_empty() {
            for failure in &failures {
                eprintln!("FAIL {}: {}", failure.name, failure.detail);
            }
            anyhow::bail!("{} soak assertion(s) failed", failures.len());
        }
        return Ok(());
    }

    run_with_bots(config, bots, distribution, seed, shutdown)
}
