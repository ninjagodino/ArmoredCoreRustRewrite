//! Scratch: list texture names in disc TPF packs whose name matches a filter.
use acvd_formats::vfs;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let prefix = args
        .first()
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_else(|| "lang/en".into());
    let disc = vfs::Disc::open(&vfs::repo_root().join(vfs::X360_ISO))?;
    for file in disc.files().into_iter() {
        let low = file.to_ascii_lowercase();
        if !low.starts_with(&prefix) || !low.ends_with(".tpf.dcx") {
            continue;
        }
        let Ok(data) = disc.read(&file) else { continue };
        let unpacked = if data.starts_with(b"DCX") {
            match acvd_formats::dcx::decompress(&data) {
                Ok(d) => d,
                Err(_) => continue,
            }
        } else {
            data
        };
        let Ok(tpf) = acvd_formats::tpf::read(&unpacked) else {
            continue;
        };
        for t in &tpf.textures {
            println!("{file}\t{}", t.name);
        }
    }
    Ok(())
}
