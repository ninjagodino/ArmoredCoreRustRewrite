//! Prints every message of an FMG bank as `id<TAB>text`.
//! `fmgdump [--disc PATH] [ASSET]` (default `lang/en/text/menu/manual.fmg`).
use acvd_formats::{fmg, vfs};

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let path = match args.iter().position(|a| a == "--disc") {
        Some(i) => {
            args.remove(i);
            std::path::PathBuf::from(args.remove(i))
        }
        None => vfs::repo_root().join(vfs::X360_ISO),
    };
    let disc = vfs::Disc::open(&path)?;
    let asset = args
        .first()
        .cloned()
        .unwrap_or_else(|| "lang/en/text/menu/manual.fmg".into());
    let bank = fmg::read(&disc.asset(&asset)?)?;
    for (id, text) in &bank.entries {
        if let Some(t) = text {
            println!("{id}\t{}", t.replace('\n', "\\n"));
        }
    }
    Ok(())
}
