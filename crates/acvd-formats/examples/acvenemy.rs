//! Temporary: print the main FLVER bbox of every ACV enemy model binder.
use acvd_formats::{bnd3, flver, vfs};

fn main() -> anyhow::Result<()> {
    let disc = vfs::Disc::open(&vfs::repo_root().join(vfs::X360_ISO_V))?;
    let ids = std::fs::read_to_string("private/tmp/acv_models.txt")?;
    for id in ids.split_whitespace() {
        if !id.starts_with('e') || id.len() != 5 {
            continue;
        }
        let binder_path = format!("model/ene/{id}/{id}_m.bnd.dcx");
        let Ok(binder) = disc.asset(&binder_path) else {
            continue;
        };
        let Ok(b) = bnd3::read(&binder) else { continue };
        for e in &b.entries {
            let Some(name) = e.name.as_deref() else {
                continue;
            };
            if !name.to_ascii_lowercase().ends_with(".flv") {
                continue;
            }
            let Ok(d) = e.contents(&binder) else { continue };
            let Ok(f) = flver::read(&d) else { continue };
            let dx = f.bbox_max[0] - f.bbox_min[0];
            let dy = f.bbox_max[1] - f.bbox_min[1];
            let dz = f.bbox_max[2] - f.bbox_min[2];
            let verts: usize = f
                .meshes
                .iter()
                .flat_map(|m| &m.vertex_buffers)
                .map(|&b| f.vertex_buffers[b].vertex_count.max(0) as usize)
                .sum();
            println!("{id} {name:16} dx={dx:7.2} dy={dy:7.2} dz={dz:7.2} verts={verts}");
        }
    }
    Ok(())
}
