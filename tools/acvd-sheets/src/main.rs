//! Spreadsheet-driven build pipeline:
//! `extract` (disc -> sheets) -> `preflight` (overlap every sheet, report failures) -> `gen` (one strut per row).

mod archives;
mod calc;
mod extract;
mod model;
mod models;
mod motions;
mod names;
mod preflight;
mod strut;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Result};

use model::Paths;

const USAGE: &str = "usage: acvd-sheets <extract|preflight|gen|all|dump <asset>> [--disc <dump root or 360 ISO>] [--root <repo root>]";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode> {
    let mut args = std::env::args().skip(1);
    let Some(cmd) = args.next() else { bail!(USAGE) };
    let asset = if cmd == "dump" { args.next() } else { None };
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let mut disc = std::env::var_os("ACVD_DISC").map(PathBuf::from);
    while let Some(flag) = args.next() {
        let value = args.next().map(PathBuf::from);
        match (flag.as_str(), value) {
            ("--disc", Some(v)) => disc = Some(v),
            ("--root", Some(v)) => root = v,
            _ => bail!(USAGE),
        }
    }
    let root = root.canonicalize()?;
    let disc = disc.unwrap_or_else(|| acvd_formats::vfs::default_disc(&root));
    let open = || acvd_formats::vfs::Disc::open(&disc);
    let paths = Paths { root };

    let clean = |paths: &Paths| -> Result<bool> { Ok(preflight::run(paths)?.errors == 0) };
    match cmd.as_str() {
        "extract" => extract::run(&paths, &open()?)?,
        "preflight" => {
            if !clean(&paths)? {
                return Ok(ExitCode::FAILURE);
            }
        }
        "gen" | "all" => {
            if cmd == "all" {
                extract::run(&paths, &open()?)?;
            }
            if !clean(&paths)? {
                eprintln!("gen: preflight has errors; fix the sheets and re-run");
                return Ok(ExitCode::FAILURE);
            }
            strut::run(&paths)?;
        }
        "dump" => {
            let Some(asset) = asset else { bail!(USAGE) };
            let data = open()?.asset(&asset)?;
            let dir = paths.root.join("private").join("dump");
            std::fs::create_dir_all(&dir)?;
            let out = dir.join(asset.replace(['/', '|'], "_"));
            std::fs::write(&out, &data)?;
            println!("dump: {} bytes -> {}", data.len(), out.display());
            if let Ok(flver) = acvd_formats::flver::read(&data) {
                let json = out.with_extension("flver.json");
                std::fs::write(&json, serde_json::to_string_pretty(&flver)?)?;
                println!("dump: FLVER header -> {}", json.display());
            }
        }
        _ => bail!(USAGE),
    }
    Ok(ExitCode::SUCCESS)
}
