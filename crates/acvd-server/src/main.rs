//! Dedicated ACVD server binary.
//!
//! Usage:
//!   acvd-server [--bind 0.0.0.0:5000] [--max-clients 24] [--protocol-id N]
//!               [--map m4000] [--disc <ACV2.pe>]
//!
//! The owned disc (and the generated tables compiled into `acvd-data`) is never bundled; pass
//! `--disc` or set `ACVD_DISC`. Missing data fails fast with a clear message.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use acvd_server::{run, ServerConfig};

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
            "-h" | "--help" => {
                println!(
                    "acvd-server [--bind 0.0.0.0:5000] [--max-clients 24] [--protocol-id N]\n\
                     \x20            [--map m4000] [--disc <ACV2.pe>]\n\n\
                     Missing disc/generated data fails fast; the owned disc is never bundled."
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

    run(config, shutdown)
}
