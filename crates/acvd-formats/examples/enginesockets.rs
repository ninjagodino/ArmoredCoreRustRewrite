//! Scratch: derive rear engine-socket centers from FLVER skin/geometry.
use acvd_formats::flver;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("flv");
    let mesh_i: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    let d = std::fs::read(path)?;
    let f = flver::read(&d)?;
    let m = &f.meshes[mesh_i];
    let v = f.vertices(&d, m)?;
    let bones = f.vertex_bones(m, &v);
    println!(
        "mesh {mesh_i}: {} verts, {} bones",
        v.positions.len(),
        f.bones.len()
    );
    for (bi, b) in f.bones.iter().enumerate() {
        let pts: Vec<_> = v
            .positions
            .iter()
            .zip(&bones)
            .filter(|(_, b)| **b == Some(bi))
            .map(|(p, _)| *p)
            .collect();
        if pts.is_empty() {
            continue;
        }
        let mut min = pts[0];
        let mut max = pts[0];
        for p in &pts {
            for a in 0..3 {
                min[a] = min[a].min(p[a]);
                max[a] = max[a].max(p[a]);
            }
        }
        if bi == 0 || max[2] - min[2] > 10.0 {
            println!(
                "bone {bi:2} {:<14} n={:5} min=({:8.2},{:7.2},{:8.2}) max=({:8.2},{:7.2},{:8.2})",
                b.name,
                pts.len(),
                min[0],
                min[1],
                min[2],
                max[0],
                max[1],
                max[2]
            );
        }
        if bi == 2 || bi == 7 || bi == 12 || bi == 17 {
            let rear: Vec<_> = pts.iter().filter(|p| p[2] > max[2] - 2.0).collect();
            let n = rear.len() as f32;
            let c = rear.iter().fold([0.0f32; 3], |mut a, p| {
                for k in 0..3 {
                    a[k] += p[k];
                }
                a
            });
            println!(
                "  rear n={:4} centroid=({:8.3},{:7.3},{:8.3})",
                rear.len(),
                c[0] / n,
                c[1] / n,
                c[2] / n
            );
        }
    }
    if mesh_i == 1 && f.meshes.len() > 6 {
        for &(xc, half, zmax) in &[
            (67.83f32, 7.83f32, 55.77f32),
            (91.34, 7.63, 17.64),
            (113.73, 6.92, 56.01),
            (139.00, 7.63, 44.05),
            (-67.83, 7.83, 55.77),
            (-91.34, 7.63, 17.64),
            (-113.73, 6.92, 56.01),
            (-139.00, 7.63, 44.05),
        ] {
            let pts: Vec<_> = v
                .positions
                .iter()
                .filter(|p| {
                    (p[0] - xc).abs() < half && p[1] > -9.2 && p[1] < -2.6 && p[2] > zmax - 2.0
                })
                .collect();
            let n = pts.len() as f32;
            let c = pts.iter().fold([0.0f32; 3], |mut a, p| {
                for k in 0..3 {
                    a[k] += p[k];
                }
                a
            });
            println!(
                "nacelle x={xc:8.2} zmax={zmax:7.2} n={:3} centroid=({:8.3},{:7.3},{:8.3})",
                pts.len(),
                c[0] / n,
                c[1] / n,
                c[2] / n
            );
        }
    }
    Ok(())
}
