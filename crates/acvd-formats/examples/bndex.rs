//! Scratch: extract a BND3 entry by index to a file.
use acvd_formats::{bnd3, vfs};
fn main() -> anyhow::Result<()> {
    let asset = std::env::args().nth(1).expect("asset");
    let idx: usize = std::env::args().nth(2).expect("index").parse()?;
    let out = std::env::args().nth(3).expect("out");
    let disc = vfs::Disc::default_for(&vfs::repo_root())?;
    let data = vfs::open(&disc, &asset)?;
    let b = bnd3::read(&data)?;
    let e = b.entries.get(idx).expect("index");
    std::fs::write(&out, e.contents(&data)?.as_ref())?;
    println!("{} {} -> {}", idx, e.name.as_deref().unwrap_or("?"), out);
    Ok(())
}
