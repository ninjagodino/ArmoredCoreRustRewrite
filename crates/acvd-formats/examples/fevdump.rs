//! Prints each event of an FEV project with its layers' sound defs and their first wave.
//! `fevdump [--disc PATH] [PROJECT]` (default `acv2_se_booster`, read from `sound/<name>.fev`).
use acvd_formats::{fev, vfs};

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
    let name = args
        .first()
        .cloned()
        .unwrap_or_else(|| "acv2_se_booster".into());
    let project = fev::read(&disc.read(&format!("sound/{name}.fev"))?)?;
    for event in &project.events {
        let layers: Vec<String> = event
            .layers
            .iter()
            .map(|&i| match project.defs.get(i as usize) {
                Some(d) => match &d.wave {
                    Some(w) => format!("{} ({}#{})", d.name, w.bank, w.index),
                    None => d.name.clone(),
                },
                None => format!("def {i}?"),
            })
            .collect();
        println!("{}\t{}", event.name, layers.join(" | "));
    }
    Ok(())
}
