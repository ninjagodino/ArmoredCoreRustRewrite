//! Temporary: FLVER bind bones and mesh bounds for map-object models.
use acvd_formats::{flver, vfs};

fn main() -> anyhow::Result<()> {
    let disc = vfs::Disc::open(&vfs::repo_root().join(vfs::X360_ISO))?;
    for model in std::env::args().skip(1) {
        let asset = if model.contains('|') {
            model.clone()
        } else {
            format!("model/obj/{model}/{model}_m.bnd.dcx|{model}.flv")
        };
        let data = disc.asset(&asset)?;
        let f = flver::read(&data)?;
        println!("== {model}");
        for (i, b) in f.bones.iter().enumerate() {
            println!(
                "{i:2} parent={:2} {:<14} t=({:.2},{:.2},{:.2})",
                b.parent, b.name, b.translation[0], b.translation[1], b.translation[2]
            );
        }
        for (i, m) in f.meshes.iter().enumerate() {
            let Ok(v) = f.vertices(&data, m) else {
                continue;
            };
            let bones = f.vertex_bones(m, &v);
            let mut counts = std::collections::HashMap::new();
            for b in bones.iter().flatten() {
                *counts.entry(*b).or_insert(0usize) += 1;
            }
            let mut counts: Vec<_> = counts.into_iter().collect();
            counts.sort_by_key(|&(b, _)| b);
            let summary: Vec<String> = counts
                .iter()
                .map(|&(b, n)| format!("{}:{}", f.bones[b].name, n))
                .collect();
            let defaults: Vec<String> = m
                .bone_indices
                .iter()
                .map(|&b| {
                    usize::try_from(b)
                        .ok()
                        .and_then(|b| f.bones.get(b))
                        .map_or("-".to_string(), |x| x.name.clone())
                })
                .collect();
            println!(
                "mesh {i} default={} idx=[{}] bones: {}",
                f.bones
                    .get(m.default_bone.max(0) as usize)
                    .map_or("-", |b| b.name.as_str()),
                defaults.join(","),
                summary.join(" ")
            );
            let mut min = v.positions[0];
            let mut max = v.positions[0];
            for p in &v.positions {
                for a in 0..3 {
                    min[a] = min[a].min(p[a]);
                    max[a] = max[a].max(p[a]);
                }
            }
            println!(
                "mesh {i:2} verts={:5} size=({:.2},{:.2},{:.2}) min=({:.2},{:.2},{:.2}) max=({:.2},{:.2},{:.2})",
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
    }
    Ok(())
}
