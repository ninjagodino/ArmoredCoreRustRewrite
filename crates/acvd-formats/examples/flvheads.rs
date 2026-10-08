//! Prints every FLVER of a model binder with its header unknowns and material MTDs:
//! `flvheads model/map/m4000/m4000_m.dcx.bnd`.
use acvd_formats::{bnd3, flver, vfs};

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("binder path");
    let disc = vfs::Disc::open(&vfs::repo_root().join(vfs::X360_ISO))?;
    let data = vfs::open(&disc, &path)?;
    let b = bnd3::read(&data)?;
    for e in &b.entries {
        let name = e.name.as_deref().unwrap_or_default();
        if !name.to_ascii_lowercase().contains(".flv") {
            continue;
        }
        let d = vfs::undcx(e.contents(&data)?.into_owned())?;
        let Ok(f) = flver::read(&d) else { continue };
        let mtds: std::collections::BTreeSet<&str> = f.materials.iter().map(|m| m.mtd.rsplit(['\\', '/']).next().unwrap_or("")).collect();
        println!(
            "{name:40} 4a={} 4b={} 4c={} 5c={} 5d={} 68={} bbox={:?}..{:?} {:?}",
            f.unk4a, f.unk4b, f.unk4c, f.unk5c, f.unk5d, f.unk68, f.bbox_min, f.bbox_max, mtds
        );
        if std::env::args().nth(2).is_some_and(|m| name.starts_with(&m)) {
            for m in &f.materials {
                println!("    mat {:24} {:24} flags={:#x} gx={:#x} unk18={}", m.name, m.mtd.rsplit(['\\', '/']).next().unwrap_or(""), m.flags, m.gx_offset, m.unk18);
            }
            if let Some(m) = f.materials.first() {
                let at = m.gx_offset as usize;
                let hex: Vec<String> = d[at..(at + 0x60).min(d.len())].chunks(4).map(|c| c.iter().map(|b| format!("{b:02x}")).collect()).collect();
                println!("    gx@{at:#x}: {}", hex.join(" "));
            }
            for m in &f.meshes {
                println!("    mesh dyn={} flags={:?} mat={} unk08={} bone={}", m.dynamic, m.flags, m.material, m.unk08, m.default_bone);
            }
        }
    }
    Ok(())
}
