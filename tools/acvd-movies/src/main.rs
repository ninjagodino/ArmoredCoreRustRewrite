//! `acvd-movies`: transcode a 360 disc `.wmv` into an FFmpeg-free playback cache.
//!
//! The rewrite has no ASF demux or WMV3 decoder, so `cache` shells out to `ffmpeg` once and
//! stores what the game reads at runtime: one JPEG per frame plus a stereo 48 kHz PCM WAV and a
//! `manifest.json`. Every derived file stays under `private/movies/` (gitignored); `cache`
//! refuses to write outside `private/`.
//!
//! ```text
//! acvd-movies list                                  # disc movies + cache state
//! acvd-movies cache prologue                        # private/movies/prologue/
//! acvd-movies cache jp/prologue --quality 3         # same, explicit disc path
//! acvd-movies clean prologue
//! ```
//!
//! The game looks the cache up itself (`acvd-game --movie prologue`) and prints this command
//! when it is missing.

use std::path::{Path, PathBuf};
use std::process::Command;

use acvd_formats::vfs::{self, Disc};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    name: String,
    source: String,
    width: u32,
    height: u32,
    fps: f64,
    frames: u32,
    duration: f64,
    audio: Option<String>,
    #[serde(default)]
    audio_seconds: Option<f64>,
    generated_by: String,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("acvd-movies: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let root = vfs::repo_root();
    let mut args = std::env::args().skip(1);
    let mut out = root.join("private/movies");
    let mut disc_path = None;
    let mut quality = 4u32;
    let mut audio = true;
    let mut force = false;
    let mut positional = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = PathBuf::from(args.next().context("--out needs a directory")?),
            "--disc" => disc_path = Some(PathBuf::from(args.next().context("--disc needs a path")?)),
            "--quality" => quality = args.next().context("--quality needs 1..31")?.parse().context("--quality")?,
            "--no-audio" => audio = false,
            "--force" => force = true,
            "-h" | "--help" => {
                println!(
                    "usage: acvd-movies <list|cache|clean> [name] [--disc ISO] [--out DIR] \
                     [--quality 1..31] [--no-audio] [--force]"
                );
                return Ok(());
            }
            other => positional.push(other.to_string()),
        }
    }
    let command = positional.first().map(String::as_str).unwrap_or("list");
    if !out.starts_with(root.join("private")) {
        bail!("--out must stay under private/ (got {})", out.display());
    }
    let disc = Disc::open(&disc_path.clone().unwrap_or_else(|| vfs::default_disc(&root)))?;
    match command {
        "list" => list(&disc, &out),
        "cache" => {
            let name = positional.get(1).context("cache needs a movie name (see `acvd-movies list`)")?;
            cache(&disc, &root, &out, name, quality, audio, force)
        }
        "clean" => {
            let name = positional.get(1).context("clean needs a movie name")?;
            let dir = out.join(asset_stem(name)?.0);
            if dir.exists() {
                std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
                println!("removed {}", dir.display());
            }
            Ok(())
        }
        other => bail!("unknown command `{other}` (list, cache, clean)"),
    }
}

/// `(cache name, disc path)` for a CLI name: `prologue` -> `movie/jp/prologue.wmv`, and a path
/// containing `/` or ending in `.wmv` is taken as the disc asset itself.
fn asset_stem(name: &str) -> Result<(String, String)> {
    let name = name.trim();
    if name.is_empty() {
        bail!("empty movie name");
    }
    if name.contains('/') || name.contains('\\') {
        let path = name.replace('\\', "/");
        let file = path.rsplit('/').next().unwrap_or(&path);
        let stem = file.strip_suffix(".wmv").unwrap_or(file);
        return Ok((stem.to_string(), if path.ends_with(".wmv") { path } else { format!("{path}.wmv") }));
    }
    let stem = name.strip_suffix(".wmv").unwrap_or(name);
    Ok((stem.to_string(), format!("movie/jp/{stem}.wmv")))
}

fn list(disc: &Disc, out: &Path) -> Result<()> {
    let mut names: Vec<String> = disc
        .list("movie/jp")
        .into_iter()
        .filter(|n| n.ends_with(".wmv"))
        .collect();
    names.sort();
    for asset in names {
        let file = asset.rsplit('/').next().unwrap_or(&asset);
        let stem = file.trim_end_matches(".wmv");
        let manifest = out.join(stem).join("manifest.json");
        let state = match std::fs::read(&manifest).ok().and_then(|b| serde_json::from_slice::<Manifest>(&b).ok()) {
            Some(m) => format!("cached {} frames, {:.2}s ({}x{} @ {:.3})", m.frames, m.duration, m.width, m.height, m.fps),
            None => "not cached".to_string(),
        };
        let size = disc.size(&asset).unwrap_or(0);
        println!("{stem:<24} {:>10.1} MiB  {state}", size as f64 / (1024.0 * 1024.0));
    }
    Ok(())
}

fn cache(
    disc: &Disc,
    root: &Path,
    out: &Path,
    name: &str,
    quality: u32,
    audio: bool,
    force: bool,
) -> Result<()> {
    ensure!((1..=31).contains(&quality), "--quality must be 1..31");
    let (stem, asset) = asset_stem(name)?;
    ensure!(disc.exists(&asset), "no `{asset}` on the 360 disc (see `acvd-movies list`)");
    let dir = out.join(&stem);
    let manifest_path = dir.join("manifest.json");
    if manifest_path.exists() && !force {
        println!("{} is already cached; pass --force to rebuild", dir.display());
        return Ok(());
    }
    let wmv = root.join("private/tmp").join(format!("acvd-movies-{stem}.wmv"));
    std::fs::create_dir_all(wmv.parent().unwrap()).context("creating private/tmp")?;
    std::fs::create_dir_all(dir.join("frames")).with_context(|| format!("creating {}", dir.display()))?;
    let data = disc.read(&asset).with_context(|| format!("reading {asset}"))?;
    std::fs::write(&wmv, &data).with_context(|| format!("writing {}", wmv.display()))?;
    println!("{asset}: {:.1} MiB -> {}", data.len() as f64 / 1048576.0, dir.display());

    let probe = probe(&wmv)?;
    let fps = probe.fps.unwrap_or(30.0);
    ensure!(fps > 0.0, "ffprobe reported fps {fps}");
    let ffmpeg = "ffmpeg";
    let frames_pattern = dir.join("frames").join("%05d.jpg");
    let status = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(&wmv)
        .args(["-an", "-vsync", "0", "-start_number", "0", "-q:v", &quality.to_string()])
        .arg(&frames_pattern)
        .status()
        .context("running ffmpeg (is it on PATH?)")?;
    ensure!(status.success(), "ffmpeg frame extraction failed ({status})");

    let audio_name = if audio {
        let wav = dir.join("audio.wav");
        let status = Command::new(ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(&wmv)
            .args(["-vn", "-ac", "2", "-ar", "48000", "-c:a", "pcm_s16le"])
            .arg(&wav)
            .status()
            .context("running ffmpeg (is it on PATH?)")?;
        ensure!(status.success(), "ffmpeg audio extraction failed ({status})");
        Some("audio.wav".to_string())
    } else {
        None
    };

    let frames = std::fs::read_dir(dir.join("frames"))?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jpg"))
        .count() as u32;
    ensure!(frames > 0, "ffmpeg wrote no frames");
    let duration = probe.duration.unwrap_or(frames as f64 / fps);
    let manifest = Manifest {
        name: stem.clone(),
        source: asset.clone(),
        width: probe.width,
        height: probe.height,
        fps,
        frames,
        duration,
        audio: audio_name.clone(),
        audio_seconds: probe.audio_seconds,
        generated_by: format!(
            "acvd-movies ffmpeg {frames} frames q{quality}{}; source {asset}",
            if audio_name.is_some() { " + stereo 48k PCM" } else { "" }
        ),
    };
    std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?).with_context(|| format!("writing {}", manifest_path.display()))?;
    let _ = std::fs::remove_file(&wmv);
    println!(
        "cached {stem}: {} frames, {:.2}s, {}x{} @ {:.3} fps{} -> {}",
        frames,
        duration,
        probe.width,
        probe.height,
        fps,
        if audio_name.is_some() { ", stereo WAV" } else { "" },
        dir.display()
    );
    Ok(())
}

struct Probe {
    width: u32,
    height: u32,
    fps: Option<f64>,
    duration: Option<f64>,
    audio_seconds: Option<f64>,
}

fn probe(wmv: &Path) -> Result<Probe> {
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-show_streams", "-show_format", "-of", "json"])
        .arg(wmv)
        .output()
        .context("running ffprobe (is it on PATH?)")?;
    ensure!(output.status.success(), "ffprobe failed: {}", String::from_utf8_lossy(&output.stderr));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).context("parsing ffprobe JSON")?;
    let streams = json["streams"].as_array().context("ffprobe streams")?;
    let video = streams.iter().find(|s| s["codec_type"] == "video").context("no video stream")?;
    let audio = streams.iter().find(|s| s["codec_type"] == "audio");
    let fps = video["r_frame_rate"]
        .as_str()
        .and_then(parse_ratio)
        .or_else(|| video["avg_frame_rate"].as_str().and_then(parse_ratio));
    let duration = json["format"]["duration"].as_str().and_then(|d| d.parse().ok());
    Ok(Probe {
        width: video["width"].as_u64().unwrap_or(0) as u32,
        height: video["height"].as_u64().unwrap_or(0) as u32,
        fps,
        duration,
        audio_seconds: audio.and_then(|a| a["duration"].as_str().and_then(|d| d.parse().ok())),
    })
}

fn parse_ratio(s: &str) -> Option<f64> {
    let (a, b) = s.split_once('/')?;
    let (a, b): (f64, f64) = (a.parse().ok()?, b.parse().ok()?);
    (b > 0.0).then_some(a / b)
}
