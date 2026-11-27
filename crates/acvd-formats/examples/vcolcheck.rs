//! Temporary: per-mesh vertex-colour alpha ranges for a raw .flv.
use acvd_formats::flver;

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("flv path");
    let d = std::fs::read(path)?;
    let f = flver::read(&d)?;
    for (i, m) in f.meshes.iter().enumerate() {
        let mat = &f.materials[m.material as usize];
        let mtd = mat.mtd.rsplit(['\\', '/']).next().unwrap_or("");
        let v = match f.vertices(&d, m) {
            Ok(v) => v,
            Err(_) => {
                println!("mesh {i:3} mat {:3} {mtd:24} <no vertices>", m.material);
                continue;
            }
        };
        let alphas: Vec<f32> = v
            .colors
            .iter()
            .flat_map(|c| c.iter().map(|k| k[3]))
            .collect();
        let rgb = v
            .colors
            .iter()
            .flat_map(|c| c.iter().map(|k| k[0].max(k[1]).max(k[2])))
            .fold(0.0f32, f32::max);
        if alphas.is_empty() {
            println!(
                "mesh {i:3} mat {:3} {mtd:24} verts {:5} colors 0",
                m.material,
                v.positions.len()
            );
        } else {
            let min = alphas.iter().copied().fold(f32::INFINITY, f32::min);
            let max = alphas.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let mean = alphas.iter().sum::<f32>() / alphas.len() as f32;
            let below = alphas.iter().filter(|a| **a < 0.999).count();
            println!(
                "mesh {i:3} mat {:3} {mtd:24} verts {:5} colors {} alpha min {min:.3} mean {mean:.3} max {max:.3} below1 {below}/{} rgbmax {rgb:.3}",
                m.material,
                v.positions.len(),
                v.colors.len(),
                alphas.len()
            );
        }
    }
    Ok(())
}
