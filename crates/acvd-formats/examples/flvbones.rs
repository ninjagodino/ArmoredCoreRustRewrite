//! Matches the vertices of FLVERs on the 360 ISO to the PS3 dump's copies by position and finds
//! the byte order of the 360 bone indices: `flvbones <asset path>...`.
//! Last run (e0310 e1013 e4020 e6050): order 3 2 1 0 matches every matched vertex.
use std::collections::{BTreeMap, HashMap};

use acvd_formats::{flver, vfs};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = vfs::repo_root();
    let x = vfs::Disc::open(&root.join(vfs::X360_ISO))?;
    let p = vfs::Disc::open(&root.join(vfs::PS3_DUMP))?;
    let perms: Vec<[usize; 4]> = {
        let mut v = Vec::new();
        for a in 0..4 {
            for b in 0..4 {
                for c in 0..4 {
                    for d in 0..4 {
                        let s = [a, b, c, d];
                        if (0..4).all(|i| s.contains(&i)) {
                            v.push(s);
                        }
                    }
                }
            }
        }
        v
    };
    let mut tally: BTreeMap<String, usize> = BTreeMap::new();
    for asset in &args {
        let (xd, pd) = (x.asset(asset)?, p.asset(asset)?);
        let (xf, pf) = (flver::read(&xd)?, flver::read(&pd)?);
        for (mi, (xm, pm)) in xf.meshes.iter().zip(&pf.meshes).enumerate() {
            let (xv, pv) = (xf.vertices(&xd, xm)?, pf.vertices(&pd, pm)?);
            if xv.bone_indices.is_empty() || pv.bone_indices.is_empty() {
                continue;
            }
            let key = |q: [f32; 3]| q.map(|c| (c * 1000.0).round() as i64);
            let mut by_pos: HashMap<[i64; 3], Vec<usize>> = HashMap::new();
            for (i, q) in pv.positions.iter().enumerate() {
                by_pos.entry(key((*q).into())).or_default().push(i);
            }
            let mut hits = vec![0usize; perms.len()];
            let mut weights_same = 0;
            let mut matched = 0;
            for (i, q) in xv.positions.iter().enumerate() {
                let Some(c) = by_pos.get(&key((*q).into())) else { continue };
                matched += 1;
                let xb = xv.bone_indices[i];
                for (k, s) in perms.iter().enumerate() {
                    let pb = s.map(|j| xb[j]);
                    if c.iter().any(|&j| pv.bone_indices[j] == pb) {
                        hits[k] += 1;
                    }
                }
                if c.iter().any(|&j| pv.bone_weights.get(j).zip(xv.bone_weights.get(i)).is_some_and(|(a, b)| a.iter().zip(b).all(|(u, v)| (u - v).abs() < 0.01))) {
                    weights_same += 1;
                }
            }
            let best = (0..perms.len()).max_by_key(|&k| hits[k]).unwrap();
            println!("{asset} mesh {mi}: {} vs {} vertices, {matched} matched; best order {:?} {} hits (identity {}); weights equal {weights_same}; sample 360 {:?} ps3 {:?}", xv.positions.len(), pv.positions.len(), perms[best], hits[best], hits[0], &xv.bone_indices[..3.min(xv.bone_indices.len())], &pv.bone_indices[..3.min(pv.bone_indices.len())]);
            *tally.entry(format!("{:?}", perms[best])).or_default() += 1;
        }
    }
    println!("{tally:?}");
    Ok(())
}
