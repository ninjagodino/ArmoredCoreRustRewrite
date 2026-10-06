//! Decodes every sample (XMA or 16-bit PCM) of the 360 FSB4 banks: `xmacheck [bank stem ...] [--wav DIR]`
//! (default: every `sound/*.fsb` on the 360 ISO). Prints per-bank sample, channel and error
//! counts and the peak level; `--wav` writes each decoded sample of the named banks as
//! `DIR/<bank>_<index>.wav`, plus the raw payload (`_<ch>ch_<rate>.xma`) and the float PCM
//! (`.f32`) for comparison with a reference decoder.
use std::path::PathBuf;
use std::time::Instant;

use acvd_formats::{fsb, vfs};

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let wav = args.iter().position(|a| a == "--wav").map(|i| {
        let dir = PathBuf::from(args.remove(i + 1));
        args.remove(i);
        dir
    });
    let disc = vfs::Disc::open(&vfs::repo_root().join(vfs::X360_ISO))?;
    let banks: Vec<String> = if args.is_empty() {
        disc.list("sound").into_iter().filter(|f| f.ends_with(".fsb")).collect()
    } else {
        args.iter().map(|a| format!("sound/{a}.fsb")).collect()
    };
    let (mut samples, mut errors, mut failed) = (0, 0, 0);
    for bank_path in &banks {
        let start = Instant::now();
        let bank = fsb::read(&disc.read(bank_path)?)?;
        let stem = bank_path.trim_start_matches("sound/").trim_end_matches(".fsb");
        let (mut bank_errors, mut peak, mut channels) = (0, 0f32, std::collections::BTreeSet::new());
        for (i, s) in bank.samples.iter().enumerate() {
            channels.insert(s.channels);
            match s.decode() {
                Ok(pcm) => {
                    bank_errors += pcm.errors;
                    peak = pcm.samples.iter().fold(peak, |p, v| p.max(v.abs()));
                    if let Some(dir) = &wav {
                        std::fs::create_dir_all(dir)?;
                        std::fs::write(dir.join(format!("{stem}_{i:03}.wav")), wav_bytes(&pcm.to_i16(), pcm.channels, s.frequency))?;
                        std::fs::write(dir.join(format!("{stem}_{i:03}_{}ch_{}.xma", s.channels, s.frequency)), &s.data)?;
                        let f32s: Vec<u8> = pcm.samples.iter().flat_map(|v| v.to_le_bytes()).collect();
                        std::fs::write(dir.join(format!("{stem}_{i:03}.f32")), f32s)?;
                    }
                }
                Err(e) => {
                    failed += 1;
                    println!("FAIL {stem}#{i} {}: {e:#}", s.name);
                }
            }
        }
        samples += bank.samples.len();
        errors += bank_errors;
        println!("{stem}: {} samples, channels {channels:?}, {bank_errors} frame errors, peak {peak:.3}, {:.1} s", bank.samples.len(), start.elapsed().as_secs_f32());
    }
    println!("{} banks, {samples} samples, {failed} failed, {errors} frame errors", banks.len());
    Ok(())
}

fn wav_bytes(samples: &[i16], channels: u16, rate: u32) -> Vec<u8> {
    let data = samples.len() * 2;
    let block = channels as u32 * 2;
    let mut out = Vec::with_capacity(44 + data);
    out.extend(b"RIFF");
    out.extend((36 + data as u32).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(channels.to_le_bytes());
    out.extend(rate.to_le_bytes());
    out.extend((rate * block).to_le_bytes());
    out.extend((block as u16).to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend((data as u32).to_le_bytes());
    for s in samples {
        out.extend(s.to_le_bytes());
    }
    out
}
