//! Scratch: connected components of one FLVER mesh, by vertex position.
use acvd_formats::flver;
fn find(p: &mut Vec<usize>, x: usize) -> usize {
    let mut r = x;
    while p[r] != r {
        r = p[r];
    }
    let mut c = x;
    while p[c] != c {
        let n = p[c];
        p[c] = r;
        c = n;
    }
    r
}
fn union(p: &mut Vec<usize>, a: usize, b: usize) {
    let (ra, rb) = (find(p, a), find(p, b));
    if ra != rb {
        p[ra] = rb;
    }
}
fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("flv");
    let mesh_i: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let d = std::fs::read(&path)?;
    let f = flver::read(&d)?;
    let m = &f.meshes[mesh_i];
    let v = f.vertices(&d, m)?;
    let n = v.positions.len();
    let mut p: Vec<usize> = (0..n).collect();
    for &fsi in &m.face_sets {
        let fs = &f.face_sets[fsi];
        let idx = f.indices(&d, fs)?;
        let step = if fs.strip != 0 { 1 } else { 3 };
        let ok = |i: u32| (i as usize) < n;
        if step == 1 {
            for w in idx.windows(2) {
                if ok(w[0]) && ok(w[1]) {
                    union(&mut p, w[0] as usize, w[1] as usize);
                }
            }
        } else {
            for t in idx.chunks(3) {
                if t.len() == 3 {
                    if ok(t[0]) && ok(t[1]) {
                        union(&mut p, t[0] as usize, t[1] as usize);
                    }
                    if ok(t[1]) && ok(t[2]) {
                        union(&mut p, t[1] as usize, t[2] as usize);
                    }
                }
            }
        }
    }
    use std::collections::HashMap;
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        let r = find(&mut p, i);
        groups.entry(r).or_default().push(i);
    }
    let mut list: Vec<_> = groups.into_values().collect();
    list.sort_by_key(|g| std::cmp::Reverse(g.len()));
    println!("mesh {mesh_i}: {n} verts, {} components", list.len());
    for (k, g) in list.iter().enumerate() {
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for &i in g {
            for a in 0..3 {
                lo[a] = lo[a].min(v.positions[i][a]);
                hi[a] = hi[a].max(v.positions[i][a]);
            }
        }
        let ext = (0..3).map(|a| hi[a] - lo[a]).fold(0.0f32, f32::max);
        if k < 40
            || ext > 1000.0
            || lo[0].abs() > 1000.0
            || hi[0].abs() > 1000.0
            || lo[2].abs() > 1000.0
            || hi[2].abs() > 1000.0
        {
            println!("comp {k:2}: {:5} verts ext={ext:9.2} min=({:9.2},{:9.2},{:9.2}) max=({:9.2},{:9.2},{:9.2})", g.len(), lo[0],lo[1],lo[2],hi[0],hi[1],hi[2]);
        }
    }
    Ok(())
}
