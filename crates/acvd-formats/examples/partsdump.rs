//! Temporary: print every MSB part (model, kind, layer, position) of a disc asset.
use acvd_formats::{msb, vfs};

fn main() -> anyhow::Result<()> {
    let root = vfs::repo_root();
    let disc = vfs::Disc::open(&vfs::default_disc(&root))?;
    let path = std::env::args().nth(1).expect("asset path");
    let data = vfs::open(&disc, &path)?;
    for p in msb::parts(&data)? {
        println!(
            "kind={} layer={:?} name={} model={} t=({:.1},{:.1},{:.1}) rot=({:.1},{:.1},{:.1})",
            p.kind,
            p.layer.as_deref().unwrap_or("-"),
            p.name,
            p.model,
            p.translation[0],
            p.translation[1],
            p.translation[2],
            p.rotation_deg[0],
            p.rotation_deg[1],
            p.rotation_deg[2]
        );
    }
    Ok(())
}
