//! Scratch: print FLVER texture UV scale/unknowns and material GX offsets.
use acvd_formats::flver;
fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("flv path");
    let d = std::fs::read(path)?;
    let f = flver::read(&d)?;
    for (i, m) in f.materials.iter().enumerate() {
        println!(
            "mat {i:2} {:16} mtd={:28} tex_index={} count={} gx={:#x} flags={:#x} unk18={}",
            m.name,
            m.mtd.rsplit(['\\', '/']).next().unwrap_or(""),
            m.texture_index,
            m.texture_count,
            m.gx_offset,
            m.flags,
            m.unk18
        );
    }
    for (i, t) in f.textures.iter().enumerate() {
        println!("tex {i:2} path={:28} kind={:22} scale=({:.4},{:.4}) unk10={} unk11={} unk14={} unk18={} unk1c={}", t.path, t.kind, t.scale[0], t.scale[1], t.unk10, t.unk11, t.unk14, t.unk18, t.unk1c);
    }
    Ok(())
}
