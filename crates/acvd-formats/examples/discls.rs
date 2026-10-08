//! Lists a disc's files under a prefix with their sizes, or writes one file out:
//! `discls [--disc PATH] PREFIX` / `discls [--disc PATH] --get FILE OUT` /
//! `discls [--disc PATH] --extract PREFIX DIR` (raw files under PREFIX) /
//! `discls [--disc PATH] --members ASSET DIR` (every binder entry, expanded; default disc: the 360 ISO).
use acvd_formats::vfs;

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
    if args.first().map(String::as_str) == Some("--get") {
        let data = disc.asset(&args[1])?;
        std::fs::write(&args[2], &data)?;
        println!("{} bytes", data.len());
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--members") {
        let binder = disc.asset(&args[1])?;
        let out = std::path::PathBuf::from(&args[2]);
        let entries = acvd_formats::bnd3::read(&binder)?.entries;
        for (i, e) in entries.iter().enumerate() {
            let name = e.name.clone().unwrap_or_else(|| format!("{i}"));
            let path = name
                .rsplit_once(':')
                .map_or(name.as_str(), |(_, p)| p)
                .replace('\\', "/");
            let dest = out.join(path.trim_start_matches('/'));
            std::fs::create_dir_all(dest.parent().unwrap())?;
            std::fs::write(&dest, vfs::entry(&binder, &format!("#{i}"))?)?;
        }
        println!("{} members -> {}", entries.len(), out.display());
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("--extract") {
        let (prefix, out) = (
            args[1].to_ascii_lowercase(),
            std::path::PathBuf::from(&args[2]),
        );
        let mut n = 0;
        for file in disc
            .files()
            .into_iter()
            .filter(|f| f.to_ascii_lowercase().starts_with(&prefix))
        {
            let dest = out.join(&file);
            std::fs::create_dir_all(dest.parent().unwrap())?;
            std::fs::write(&dest, disc.read(&file)?)?;
            n += 1;
        }
        println!("{n} files -> {}", out.display());
        return Ok(());
    }
    let prefix = args
        .first()
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    for file in disc
        .files()
        .into_iter()
        .filter(|f| f.to_ascii_lowercase().starts_with(&prefix))
    {
        println!(
            "{:>10} {file}",
            disc.size(&file)
                .map(|s| s.to_string())
                .unwrap_or_else(|_| "?".into())
        );
    }
    Ok(())
}
