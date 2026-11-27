//! Temporary: walk a directory of `.msb` files and print airborne object parts.
use std::path::Path;

use acvd_formats::msb;

fn walk(dir: &Path, f: &mut impl FnMut(&Path, msb::Part)) -> anyhow::Result<()> {
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let p = e.path();
        if p.is_dir() {
            walk(&p, f)?;
        } else if p.extension().and_then(|s| s.to_str()) == Some("msb") {
            let Ok(data) = std::fs::read(&p) else {
                continue;
            };
            let Ok(parts) = msb::parts(&data) else {
                continue;
            };
            for part in parts {
                f(&p, part);
            }
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "private/tmp/acv_maps".into());
    walk(Path::new(&dir), &mut |p, part| {
        let m = part.model.to_ascii_lowercase();
        if part.kind == 1
            && (m.starts_with("o92") || m.starts_with("o93"))
            && part.translation[1] > 10.0
        {
            println!(
                "{} layer={:?} {} t=({:.1},{:.1},{:.1}) rot=({:.1},{:.1},{:.1})",
                p.display(),
                part.layer.as_deref().unwrap_or("-"),
                part.model,
                part.translation[0],
                part.translation[1],
                part.translation[2],
                part.rotation_deg[0],
                part.rotation_deg[1],
                part.rotation_deg[2]
            );
        }
    })?;
    Ok(())
}
