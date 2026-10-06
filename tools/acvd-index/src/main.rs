//! Static fact index over the Xbox 360 executable `private/x360/vd/ACV2.pe`.
//!
//! `build` writes `private/index/*.csv` (functions, calls, slots, vtables, consts, imms, fields,
//! switches, switch_cases, vcalls, names). `q` prints only the rows a question needs; the index
//! is too large to read whole. `audit` checks the 360 addresses, slots and offsets cited in
//! `sheets/*.csv` against the index and writes `private/index/audit.md`.

mod audit;
mod build;
mod image;
mod names;
mod query;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Result};

const USAGE: &str = "usage: acvd-index <build|q ...|audit> [--pe <ACV2.pe>] [--root <repo root>]";

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
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut take = |flag: &str| -> Option<PathBuf> {
        let i = args.iter().position(|a| a == flag)?;
        let v = args.get(i + 1).map(PathBuf::from);
        args.drain(i..(i + 2).min(args.len()));
        v
    };
    let root = take("--root").unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join(".."));
    let root = root.canonicalize()?;
    let pe = take("--pe").unwrap_or_else(|| root.join("private").join("x360").join("vd").join("ACV2.pe"));
    let dir = root.join("private").join("index");
    let Some(cmd) = args.first().cloned() else { bail!(USAGE) };
    match cmd.as_str() {
        "build" => {
            let img = image::Image::open(&pe)?;
            build::run(&img, &dir)?;
            let n = names::run(&root, &dir)?;
            println!("names: {n} rows");
        }
        "q" => {
            let img = image::Image::open(&pe)?;
            if let Err(e) = query::run(&img, &dir, &args[1..]) {
                bail!("{e:#}\nusage: acvd-index {}", query::USAGE);
            }
        }
        "audit" => {
            let img = image::Image::open(&pe)?;
            if !audit::run(&img, &root, &dir)? {
                return Ok(ExitCode::FAILURE);
            }
        }
        _ => bail!(USAGE),
    }
    Ok(ExitCode::SUCCESS)
}
