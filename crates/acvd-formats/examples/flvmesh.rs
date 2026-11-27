//! Scratch: print FLVER mesh/material summary and connected cards for one mesh.
use acvd_formats::flver;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("flv");
    let mesh: Option<usize> = args.next().and_then(|s| s.parse().ok());
    let d = std::fs::read(path)?;
    let f = flver::read(&d)?;
    for (i, m) in f.meshes.iter().enumerate() {
        let mat = &f.materials[usize::try_from(m.material)?];
        let tex = if mat.texture_index >= 0 {
            f.textures
                .get(mat.texture_index as usize)
                .map(|t| t.path.as_str())
                .unwrap_or("?")
        } else {
            "?"
        };
        let v = f.vertices(&d, m)?;
        let bones = f.vertex_bones(m, &v);
        let mut min = v.positions[0];
        let mut max = v.positions[0];
        for p in &v.positions {
            for a in 0..3 {
                min[a] = min[a].min(p[a]);
                max[a] = max[a].max(p[a]);
            }
        }
        let mut tris = 0usize;
        for &fs in &m.face_sets {
            tris += f.triangles(&d, &f.face_sets[fs])?.len();
        }
        println!(
            "mesh {i:2} mat={:2} {:<5} tex={:<10} verts={:5} tris={:5} min=({:8.2},{:7.2},{:8.2}) max=({:8.2},{:7.2},{:8.2})",
            m.material, mat.name, tex, v.positions.len(), tris, min[0], min[1], min[2], max[0], max[1], max[2]
        );
        if Some(i) == mesh {
            for &fs in &m.face_sets {
                let set = &f.face_sets[fs];
                let tri = f.triangles(&d, set)?;
                println!(
                    "  face_set {fs} flags={:#x} strip={} cull={} idx={} tris={}",
                    set.flags,
                    set.strip,
                    set.cull_backfaces,
                    set.index_count,
                    tri.len()
                );
                if v.normals.len() == v.positions.len() {
                    let mut raw_neg = 0usize;
                    let mut mirrored_neg = 0usize;
                    for t in &tri {
                        let p: Vec<[f32; 3]> = t.iter().map(|&i| v.positions[i as usize]).collect();
                        let n: Vec<[f32; 3]> = t.iter().map(|&i| v.normals[i as usize]).collect();
                        let raw = cross(sub(p[1], p[0]), sub(p[2], p[0]));
                        let avg = [
                            n[0][0] + n[1][0] + n[2][0],
                            n[0][1] + n[1][1] + n[2][1],
                            n[0][2] + n[1][2] + n[2][2],
                        ];
                        if dot(raw, avg) < 0.0 {
                            raw_neg += 1;
                        }
                        let mp = [
                            [-p[0][0], p[0][1], p[0][2]],
                            [-p[1][0], p[1][1], p[1][2]],
                            [-p[2][0], p[2][1], p[2][2]],
                        ];
                        let mn = [
                            [-n[0][0], n[0][1], n[0][2]],
                            [-n[1][0], n[1][1], n[1][2]],
                            [-n[2][0], n[2][1], n[2][2]],
                        ];
                        let mir = cross(sub(mp[1], mp[0]), sub(mp[2], mp[0]));
                        let mavg = [
                            mn[0][0] + mn[1][0] + mn[2][0],
                            mn[0][1] + mn[1][1] + mn[2][1],
                            mn[0][2] + mn[1][2] + mn[2][2],
                        ];
                        if dot(mir, mavg) < 0.0 {
                            mirrored_neg += 1;
                        }
                    }
                    println!(
                        "  winding raw_dot_neg={raw_neg}/{} bevy_dot_neg={mirrored_neg}/{}",
                        tri.len(),
                        tri.len()
                    );
                }
                print_components(&v, &bones, &tri, i == 6);
            }
        }
    }
    let world = f.bone_transforms()?;
    for d in &f.dummies {
        if d.color[1] < 200 {
            continue;
        }
        let parent = usize::try_from(d.parent_bone)
            .ok()
            .filter(|&b| b < world.len());
        let wp = parent.map(|b| world[b].apply(d.position));
        println!(
            "dummy id={} parent={:?} pos={:?} world={:?} fwd={:?}",
            d.color[1], d.parent_bone, d.position, wp, d.forward
        );
    }
    Ok(())
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn print_components(v: &flver::Vertices, bones: &[Option<usize>], tri: &[[u32; 3]], verbose: bool) {
    let n = v.positions.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    for t in tri {
        let a = find(&mut parent, t[0] as usize);
        for &b in &t[1..] {
            let b = find(&mut parent, b as usize);
            if a != b {
                parent[b] = a;
            }
        }
    }
    let mut groups: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for &t in tri {
        for &i in &t {
            let r = find(&mut parent, i as usize);
            let g = groups.entry(r).or_default();
            if !g.contains(&(i as usize)) {
                g.push(i as usize);
            }
        }
    }
    for (k, (root, verts)) in groups.iter_mut().enumerate() {
        verts.sort_unstable();
        let mut min = v.positions[verts[0]];
        let mut max = min;
        for &i in verts.iter() {
            for a in 0..3 {
                min[a] = min[a].min(v.positions[i][a]);
                max[a] = max[a].max(v.positions[i][a]);
            }
        }
        let center = [
            (min[0] + max[0]) * 0.5,
            (min[1] + max[1]) * 0.5,
            (min[2] + max[2]) * 0.5,
        ];
        let mut cards = 0usize;
        for t in tri {
            if verts.contains(&(t[0] as usize)) {
                cards += 1;
            }
        }
        println!(
            "  card {k:2} root={root} tris={cards} verts={} min=({:8.2},{:7.2},{:8.2}) max=({:8.2},{:7.2},{:8.2}) size=({:7.2},{:6.2},{:7.2}) center=({:8.2},{:7.2},{:8.2})",
            verts.len(),
            min[0], min[1], min[2], max[0], max[1], max[2],
            max[0] - min[0], max[1] - min[1], max[2] - min[2],
            center[0], center[1], center[2]
        );
        if verbose {
            for &i in verts.iter() {
                let uv = v.uvs.first().and_then(|u| u.get(i)).copied();
                println!(
                    "    v{i:3} ({:8.2},{:7.2},{:8.2}) uv={:?} bone={:?}",
                    v.positions[i][0],
                    v.positions[i][1],
                    v.positions[i][2],
                    uv,
                    bones.get(i)
                );
            }
        }
    }
}
