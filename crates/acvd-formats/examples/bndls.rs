//! Temporary: list a binder's entries by name and size.
use acvd_formats::{bnd3, vfs};

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let disc = if args.first().map(String::as_str) == Some("--disc") {
        let p = std::path::PathBuf::from(args.remove(1));
        args.remove(0);
        vfs::Disc::open(&p)?
    } else {
        vfs::Disc::open(&vfs::repo_root().join(vfs::X360_ISO))?
    };
    for asset in args {
        let data = vfs::open(&disc, &asset)?;
        let b = bnd3::read(&data)?;
        println!(
            "== {asset} ({} entries, {} bytes)",
            b.entries.len(),
            data.len()
        );
        for (i, e) in b.entries.iter().enumerate() {
            let name = e.name.clone().unwrap_or_else(|| format!("#{i}"));
            let size = e.contents(&data).map(|c| c.len()).unwrap_or(0);
            println!("  {i:3} {size:9} {name}");
        }
    }
    Ok(())
}
