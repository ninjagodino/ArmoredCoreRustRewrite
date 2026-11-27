//! Temporary: list map parts nearest a world XZ point.
use acvd_formats::{msb, vfs};

fn main() -> anyhow::Result<()> {
    let root = vfs::repo_root();
    let disc = vfs::Disc::open(&vfs::default_disc(&root))?;
    let data = vfs::open(&disc, "model/map/m4000/m4000_map.msb")?;
    let cx: f32 = std::env::args()
        .nth(1)
        .map_or(-303.65, |v| v.parse().unwrap());
    let cz: f32 = std::env::args()
        .nth(2)
        .map_or(655.5, |v| v.parse().unwrap());
    let mut parts: Vec<_> = msb::parts(&data)?
        .into_iter()
        .map(|p| {
            let d = ((p.translation[0] - cx).powi(2) + (p.translation[2] - cz).powi(2)).sqrt();
            (d, p)
        })
        .collect();
    parts.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (d, p) in parts.iter().take(80) {
        println!(
            "{d:7.1} {:10} kind={} layer={:?} t=({:.1},{:.1},{:.1})",
            p.model,
            p.kind,
            p.layer.as_deref().unwrap_or("-"),
            p.translation[0],
            p.translation[1],
            p.translation[2]
        );
    }
    Ok(())
}
