//! Lists an FSB bank's samples or writes one decoded sample as raw interleaved i16.
//! `fsbdump [--disc PATH] BANK [--raw INDEX OUT]` (bank path `sound/<BANK>.fsb`).
use acvd_formats::{fsb, vfs};

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let disc_path = match args.iter().position(|a| a == "--disc") {
        Some(i) => {
            args.remove(i);
            std::path::PathBuf::from(args.remove(i))
        }
        None => vfs::repo_root().join(vfs::X360_ISO),
    };
    let raw = match args.iter().position(|a| a == "--raw") {
        Some(i) => {
            args.remove(i);
            let index = args.remove(i);
            let out = args.remove(i);
            Some((index.parse::<usize>()?, std::path::PathBuf::from(out)))
        }
        None => None,
    };
    let bank_name = args.first().cloned().unwrap_or_else(|| "se_booster".into());
    let disc = vfs::Disc::open(&disc_path)?;
    let bank = fsb::read(&disc.read(&format!("sound/{bank_name}.fsb"))?)?;
    match raw {
        Some((index, out)) => {
            let sample = bank
                .samples
                .get(index)
                .ok_or_else(|| anyhow::anyhow!("sample {index} out of range"))?;
            let pcm = sample.decode()?;
            let mono = pcm.to_i16();
            let mut bytes = Vec::with_capacity(mono.len() * 2);
            for v in &mono {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            std::fs::write(&out, &bytes)?;
            println!(
                "{}#{} {} {} Hz {} ch {} samples -> {}",
                bank_name,
                index,
                sample.name,
                sample.frequency,
                pcm.channels,
                mono.len(),
                out.display()
            );
        }
        None => {
            for (i, s) in bank.samples.iter().enumerate() {
                println!(
                    "{i}\t{}\t{} Hz\t{} ch\t{} samples\t{}",
                    s.name,
                    s.frequency,
                    s.channels,
                    s.length,
                    if s.xma() { "xma" } else { "pcm" }
                );
            }
        }
    }
    Ok(())
}
