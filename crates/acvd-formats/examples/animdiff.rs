//! Prints bones a `.ani` clip actually moves: `animdiff <disc> <asset path> [asset path ...]`.
//! Bone names come from the binder's `a00_000.ani` skeleton clip.
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let disc = acvd_formats::vfs::Disc::open(std::path::Path::new(&args[0]))?;
    for asset in &args[1..] {
        let (binder, _) = asset.split_once('|').unwrap_or((asset, ""));
        let skel_path = format!("{binder}|a00_000.ani");
        let skel_names: Vec<String> = if skel_path == *asset {
            Vec::new()
        } else if let Ok(bytes) = disc.asset(&skel_path) {
            acvd_formats::ani::read(&bytes)
                .map(|a| {
                    a.bones
                        .iter()
                        .map(|b| {
                            b.rest
                                .as_ref()
                                .map_or_else(|| "?".to_string(), |r| r.name.clone())
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let anim = acvd_formats::ani::read(&disc.asset(asset)?)?;
        println!(
            "\n=== {asset}  {} frames, {} bones, euler {} ===",
            anim.frames,
            anim.bones.len(),
            anim.euler
        );
        for (i, b) in anim.bones.iter().enumerate() {
            let keys = &b.track.keys;
            let Some(first) = keys.first() else { continue };
            let mut rot_dev = 0.0f32;
            let mut tr_dev = 0.0f32;
            for k in &keys[1..] {
                for c in 0..3 {
                    rot_dev = rot_dev.max((k.rotation[c] - first.rotation[c]).abs());
                }
                if let (Some(a), Some(z)) = (k.translation, first.translation) {
                    for c in 0..3 {
                        tr_dev = tr_dev.max((a[0][c] - z[0][c]).abs());
                    }
                }
            }
            if rot_dev < 1e-4 && tr_dev < 1e-4 {
                continue;
            }
            let name = b
                .rest
                .as_ref()
                .map(|r| r.name.clone())
                .or_else(|| skel_names.get(i).cloned())
                .unwrap_or_else(|| "?".into());
            println!(
                "{i:3} {:<22} keys {:2} rotdev {:.4} trdev {:.4}",
                name,
                keys.len(),
                rot_dev,
                tr_dev
            );
            for k in keys {
                println!(
                    "      f{:3} rot {:?} tr {:?}",
                    k.frame,
                    k.rotation.map(|v| (v * 1000.0).round() / 1000.0),
                    k.translation
                        .map(|t| t[0].map(|v| (v * 1000.0).round() / 1000.0)),
                );
            }
        }
    }
    Ok(())
}
