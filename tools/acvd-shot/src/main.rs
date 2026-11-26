//! `acvd-shot diff <reference.png> <candidate.png>`: objective parity check for screenshots.
//!
//! Use it instead of eyeballing a `--shot` against a 360 guest shot. It resizes the candidate onto
//! the reference canvas, optionally finds a small translation, reports tone/gradient/tile errors,
//! and writes an amplified diff plus a `reference | candidate | diff` strip. Exits 1 when the
//! defaults are exceeded, so a shot loop can be scripted.
//!
//! ```text
//! acvd-shot diff private/xenia/shots/ac_test_start.png private/shots/ac_test_start.png
//! acvd-shot diff ref.png cand.png --crop 640,0,640,360 --ignore 500,600,280,120 --align 6
//! ```

mod compare;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use compare::{Fit, Options, Rect};

const USAGE: &str = "\
usage: acvd-shot diff <reference.png> <candidate.png> [options]

  --fit stretch|letterbox|crop  fit candidate to the reference (default stretch)
  --crop X,Y,W,H                compare one rectangle after fitting
  --ignore X,Y,W,H              exclude a rectangle from metrics (repeatable)
  --align N                     search +/-N px translation, use the best (default 0)
  --max-diff N                  per-channel diff counted as over (0-255, default 8)
  --max-mae F                   pass threshold for mean abs error 0-1 (default 0.02)
  --max-pct F                   pass threshold for over-fraction 0-1 (default 0.10)
  --diff-gain F                 diff image amplification (default 4)
  --tiles N                     tile grid side (default 8)
  --out DIR                     output directory (default: candidate's folder)
  --json PATH                   report path (default <out>/<stem>_compare.json)
  --no-fail                     always exit 0

Exit codes: 0 pass, 1 thresholds exceeded, 2 usage/IO error.";

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("acvd-shot: {e:#}");
            std::process::exit(2);
        }
    }
}

fn run() -> Result<i32> {
    let mut it = std::env::args().skip(1);
    let Some(cmd) = it.next() else {
        println!("{USAGE}");
        return Ok(0);
    };
    match cmd.as_str() {
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            Ok(0)
        }
        "diff" | "compare" => diff(it.collect()),
        other => bail!("unknown command {other:?}\n{USAGE}"),
    }
}

fn value(args: &[String], i: &mut usize, flag: &str) -> Result<String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .with_context(|| format!("{flag} needs a value"))
}

fn diff(args: Vec<String>) -> Result<i32> {
    let mut opts = Options::default();
    let mut out: Option<PathBuf> = None;
    let mut json: Option<PathBuf> = None;
    let mut no_fail = false;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--fit" => {
                let s = value(&args, &mut i, "--fit")?;
                opts.fit = Fit::parse(&s)
                    .with_context(|| format!("--fit expects stretch|letterbox|crop, got {s:?}"))?;
            }
            "--crop" => {
                let s = value(&args, &mut i, "--crop")?;
                opts.crop = Some(
                    Rect::parse(&s)
                        .with_context(|| format!("--crop expects X,Y,W,H, got {s:?}"))?,
                );
            }
            "--ignore" => {
                let s = value(&args, &mut i, "--ignore")?;
                opts.ignore.push(
                    Rect::parse(&s)
                        .with_context(|| format!("--ignore expects X,Y,W,H, got {s:?}"))?,
                );
            }
            "--align" => {
                opts.align = value(&args, &mut i, "--align")?
                    .parse()
                    .context("--align")?
            }
            "--max-diff" => {
                opts.max_diff = value(&args, &mut i, "--max-diff")?
                    .parse()
                    .context("--max-diff")?
            }
            "--max-mae" => {
                opts.max_mae = value(&args, &mut i, "--max-mae")?
                    .parse()
                    .context("--max-mae")?
            }
            "--max-pct" => {
                opts.max_over = value(&args, &mut i, "--max-pct")?
                    .parse()
                    .context("--max-pct")?
            }
            "--diff-gain" => {
                opts.diff_gain = value(&args, &mut i, "--diff-gain")?
                    .parse()
                    .context("--diff-gain")?
            }
            "--tiles" => {
                opts.tiles = value(&args, &mut i, "--tiles")?
                    .parse()
                    .context("--tiles")?;
                if opts.tiles == 0 {
                    bail!("--tiles must be at least 1");
                }
            }
            "--out" => out = Some(PathBuf::from(value(&args, &mut i, "--out")?)),
            "--json" => json = Some(PathBuf::from(value(&args, &mut i, "--json")?)),
            "--no-fail" => no_fail = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(0);
            }
            a if a.starts_with('-') => bail!("unknown option {a:?}\n{USAGE}"),
            a => positional.push(a.to_string()),
        }
        i += 1;
    }
    if positional.len() != 2 {
        bail!("diff needs a reference and a candidate PNG\n{USAGE}");
    }
    if opts.align > 64 {
        bail!("--align is limited to 64 pixels");
    }
    if opts.tiles > 64 {
        bail!("--tiles is limited to 64");
    }
    if !(0.0..=1.0).contains(&opts.max_mae) {
        bail!("--max-mae must be within 0..1");
    }
    if !(0.0..=1.0).contains(&opts.max_over) {
        bail!("--max-pct must be within 0..1");
    }
    if !opts.diff_gain.is_finite() || opts.diff_gain <= 0.0 {
        bail!("--diff-gain must be positive");
    }
    let (ref_path, cand_path) = (PathBuf::from(&positional[0]), PathBuf::from(&positional[1]));
    let reference =
        compare::load(&ref_path).with_context(|| format!("reading {}", ref_path.display()))?;
    let candidate =
        compare::load(&cand_path).with_context(|| format!("reading {}", cand_path.display()))?;
    let report = compare::compare(&reference, &candidate, &opts);

    let out_dir = out
        .or_else(|| {
            cand_path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(Path::to_path_buf)
        })
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let stem = cand_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("shot");
    let diff_path = out_dir.join(format!("{stem}_diff.png"));
    let side_path = out_dir.join(format!("{stem}_side.png"));
    let json_path = json.unwrap_or_else(|| out_dir.join(format!("{stem}_compare.json")));
    report
        .diff
        .save(&diff_path)
        .with_context(|| format!("writing {}", diff_path.display()))?;
    report
        .side
        .save(&side_path)
        .with_context(|| format!("writing {}", side_path.display()))?;

    let m = &report.metrics;
    println!(
        "reference {}x{}  candidate {}x{}  fit {}",
        report.reference_size.0,
        report.reference_size.1,
        report.candidate_size.0,
        report.candidate_size.1,
        opts.fit.name()
    );
    println!(
        "canvas {}x{}  shift ({},{})  pixels {}",
        report.width, report.height, report.shift.0, report.shift.1, m.pixels
    );
    println!(
        "mae {:.4} ({:.1}/255)  rmse {:.4}  over {:.1}%  luma bias {:+.1}/255  gradient {:.4}",
        m.mae,
        m.mae * 255.0,
        m.rmse,
        m.over * 100.0,
        m.luma_bias * 255.0,
        m.gradient
    );
    if !report.tiles.is_empty() {
        let worst = report
            .tiles
            .iter()
            .map(|t| {
                format!(
                    "[{},{}] {:.4} bias {:+.1}/255 @{}x{}+{},{}",
                    t.tx,
                    t.ty,
                    t.mae,
                    t.bias * 255.0,
                    t.w,
                    t.h,
                    t.x,
                    t.y
                )
            })
            .collect::<Vec<_>>()
            .join("  ");
        println!("hotspots {worst}");
    }
    println!(
        "wrote {} / {} / {}",
        diff_path.display(),
        side_path.display(),
        json_path.display()
    );

    let mut reasons = Vec::new();
    if m.mae > opts.max_mae {
        reasons.push(format!("mae {:.4} > {:.4}", m.mae, opts.max_mae));
    }
    if m.over > opts.max_over {
        reasons.push(format!(
            "over {:.1}% > {:.1}%",
            m.over * 100.0,
            opts.max_over * 100.0
        ));
    }
    if report.pass {
        println!("PASS");
    } else {
        println!("FAIL {}", reasons.join("; "));
    }

    let tiles: Vec<_> = report
        .tiles
        .iter()
        .map(|t| {
            serde_json::json!({
                "tx": t.tx,
                "ty": t.ty,
                "rect": [t.x, t.y, t.w, t.h],
                "mae": t.mae,
                "bias": t.bias,
                "bias_255": t.bias * 255.0,
            })
        })
        .collect();
    let doc = serde_json::json!({
        "reference": ref_path.display().to_string(),
        "candidate": cand_path.display().to_string(),
        "reference_size": [report.reference_size.0, report.reference_size.1],
        "candidate_size": [report.candidate_size.0, report.candidate_size.1],
        "canvas": [report.width, report.height],
        "fit": opts.fit.name(),
        "shift": [report.shift.0, report.shift.1],
        "metrics": {
            "pixels": m.pixels,
            "mae": m.mae,
            "mae_255": m.mae * 255.0,
            "rmse": m.rmse,
            "over_fraction": m.over,
            "over_percent": m.over * 100.0,
            "ref_luma": m.ref_luma,
            "cand_luma": m.cand_luma,
            "luma_bias": m.luma_bias,
            "luma_bias_255": m.luma_bias * 255.0,
            "gradient": m.gradient,
        },
        "thresholds": {
            "max_diff": opts.max_diff,
            "max_mae": opts.max_mae,
            "max_over": opts.max_over,
        },
        "pass": report.pass,
        "hotspots": tiles,
        "diff": diff_path.display().to_string(),
        "side": side_path.display().to_string(),
    });
    std::fs::write(&json_path, serde_json::to_vec_pretty(&doc)?)
        .with_context(|| format!("writing {}", json_path.display()))?;

    Ok(if report.pass || no_fail { 0 } else { 1 })
}
