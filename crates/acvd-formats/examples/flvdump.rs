//! Temporary: dump FLVER materials, textures and mesh sizes for a raw .flv.
use acvd_formats::flver;

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("flv path");
    let d = std::fs::read(path)?;
    let f = flver::read(&d)?;
    for (i, b) in f.bones.iter().enumerate() {
        println!(
            "bone {i:2} parent={:2} name={} t=({:.2},{:.2},{:.2}) r=({:.2},{:.2},{:.2})",
            b.parent,
            b.name,
            b.translation[0],
            b.translation[1],
            b.translation[2],
            b.rotation[0],
            b.rotation[1],
            b.rotation[2]
        );
    }
    for (i, m) in f.materials.iter().enumerate() {
        println!(
            "mat {i:2} {:20} {:28} tex_count={} tex_index={} flags={:#x}",
            m.name,
            m.mtd.rsplit(['\\', '/']).next().unwrap_or(""),
            m.texture_count,
            m.texture_index,
            m.flags
        );
    }
    for (i, t) in f.textures.iter().enumerate() {
        println!("tex {i:2} path={:34} kind={}", t.path, t.kind);
    }
    for (i, m) in f.meshes.iter().enumerate() {
        let Ok(v) = f.vertices(&d, m) else {
            println!("mesh {i:2} mat={:2} unreadable", m.material);
            continue;
        };
        let mut min = v.positions[0];
        let mut max = v.positions[0];
        for p in &v.positions {
            for a in 0..3 {
                min[a] = min[a].min(p[a]);
                max[a] = max[a].max(p[a]);
            }
        }
        println!(
            "mesh {i:2} mat={:2} verts={:5} size=({:.2},{:.2},{:.2}) min=({:.2},{:.2},{:.2}) max=({:.2},{:.2},{:.2})",
            m.material,
            v.positions.len(),
            max[0] - min[0],
            max[1] - min[1],
            max[2] - min[2],
            min[0],
            min[1],
            min[2],
            max[0],
            max[1],
            max[2]
        );
    }
    Ok(())
}
