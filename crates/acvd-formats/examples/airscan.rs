//! Temporary: every animated map-object placement across `model/map/**/*.msb`.
use acvd_formats::{msb, vfs};
use std::collections::BTreeMap;

fn main() -> anyhow::Result<()> {
    let root = vfs::repo_root();
    let disc = vfs::Disc::open(&vfs::default_disc(&root))?;
    let mut per_model: BTreeMap<String, (usize, bool, bool)> = BTreeMap::new();
    for asset in disc.files() {
        if !asset.ends_with(".msb") || !asset.starts_with("model/map/") {
            continue;
        }
        let Ok(data) = vfs::open(&disc, &asset) else {
            continue;
        };
        let Ok(parts) = msb::parts(&data) else {
            continue;
        };
        for p in parts {
            if p.kind != 1 || !p.model.starts_with('o') {
                continue;
            }
            let tae = format!("tae/obj/{}/own000100.tae", p.model);
            let a = format!("model/obj/{0}/{0}_a.bnd.dcx", p.model);
            let has_tae = disc.exists(&tae);
            let has_a = disc.exists(&a);
            if !has_tae && !has_a {
                continue;
            }
            println!(
                "{} name={} model={} param={} layer={:?} t=({:.1},{:.1},{:.1}) rot=({:.1},{:.1},{:.1}) tae={} a={}",
                asset,
                p.name,
                p.model,
                p.param_index,
                p.layer,
                p.translation[0],
                p.translation[1],
                p.translation[2],
                p.rotation_deg[0],
                p.rotation_deg[1],
                p.rotation_deg[2],
                has_tae,
                has_a
            );
            let e = per_model
                .entry(p.model.clone())
                .or_insert((0, has_tae, has_a));
            e.0 += 1;
        }
    }
    eprintln!("--- per model (count, tae, a) ---");
    for (m, (n, t, a)) in per_model {
        eprintln!("{m} {n} tae={t} a={a}");
    }
    Ok(())
}
