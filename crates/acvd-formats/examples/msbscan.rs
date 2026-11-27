//! Temporary: list airborne map-object parts across every `.msb` on the disc.
use acvd_formats::{msb, vfs};

fn main() -> anyhow::Result<()> {
    let root = vfs::repo_root();
    let disc = vfs::Disc::open(&vfs::default_disc(&root))?;
    for asset in disc.files() {
        if !asset.ends_with(".msb") {
            continue;
        }
        let Ok(data) = vfs::open(&disc, &asset) else {
            continue;
        };
        let Ok(parts) = msb::parts(&data) else {
            continue;
        };
        for p in parts {
            if p.kind == 1
                && (p.model.starts_with("o9330")
                    || p.model.starts_with("o9341")
                    || p.model.starts_with("o9344"))
            {
                println!(
                    "{} layer={:?} {} b={:?} t=({:.1},{:.1},{:.1}) rot=({:.1},{:.1},{:.1}) s=({:.4},{:.4},{:.4})",
                    asset,
                    p.layer.as_deref().unwrap_or("-"),
                    p.model,
                    p.behavior.as_deref().unwrap_or("-"),
                    p.translation[0],
                    p.translation[1],
                    p.translation[2],
                    p.rotation_deg[0],
                    p.rotation_deg[1],
                    p.rotation_deg[2],
                    p.scale[0],
                    p.scale[1],
                    p.scale[2]
                );
            }
        }
    }
    Ok(())
}
