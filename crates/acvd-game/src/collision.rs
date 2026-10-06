//! Map collision from disc `.hmd` hit models, placed by MSB `PARTS_PARAM_ST`. `--plane` keeps
//! the old infinite floor; `--water <y>` still adds a water plane. Vertices are mirrored on X
//! after the part transform, same as FLVER → Bevy (`acvd-render`).

use std::collections::HashMap;
use std::path::Path;

use acvd_formats::{flver, hmd, msb, vfs};
use anyhow::{Context, Result};
use bevy::prelude::*;

/// Half the length of the eye-floor ray, above and below its start (360 0x8371914c).
pub const RAY_HALF: f32 = 50.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layer {
    /// What the AC stands on. Filter 0x100002 at 360 0x829e7b68.
    Ground,
    /// The layer of the eye-floor ray (filter 0x200002, 360 0x829e7be0); water by the
    /// `water_stop_y_offset` param that pairs with it.
    Water,
}

#[derive(Clone, Copy, Debug)]
pub struct Plane {
    pub y: f32,
    pub layer: Layer,
}

#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub a: Vec3,
    pub b: Vec3,
    pub c: Vec3,
    pub layer: Layer,
}

#[derive(Resource, Default)]
pub struct Collision {
    pub planes: Vec<Plane>,
    pub hits: Vec<Hit>,
}

impl Collision {
    /// Height of the first `layer` surface a ray straight down from `from` meets before `to`.
    pub fn ray_down(&self, from: Vec3, to: f32, layer: Layer) -> Option<f32> {
        let plane = self
            .planes
            .iter()
            .filter(|p| p.layer == layer && (to..=from.y).contains(&p.y))
            .map(|p| p.y)
            .reduce(f32::max);
        let mesh = self
            .hits
            .iter()
            .filter(|h| h.layer == layer)
            .filter_map(|h| tri_y(h.a, h.b, h.c, from.x, from.z, from.y, to))
            .reduce(f32::max);
        match (plane, mesh) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }

    /// The ground the AC lands on falling from `at`: the highest ground hit at or below it.
    pub fn ground_below(&self, at: Vec3) -> Option<f32> {
        self.ray_down(at, f32::NEG_INFINITY, Layer::Ground)
    }
}

/// Vertical ray (0, −1, 0) against a triangle; hit y in `[to, from]` or `None`.
fn tri_y(a: Vec3, b: Vec3, c: Vec3, x: f32, z: f32, from: f32, to: f32) -> Option<f32> {
    let dir = Vec3::new(0.0, -1.0, 0.0);
    let orig = Vec3::new(x, from, z);
    let e1 = b - a;
    let e2 = c - a;
    let pvec = dir.cross(e2);
    let det = e1.dot(pvec);
    if det.abs() < 1e-8 {
        return None;
    }
    let inv = 1.0 / det;
    let tvec = orig - a;
    let u = tvec.dot(pvec) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let qvec = tvec.cross(e1);
    let v = dir.dot(qvec) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(qvec) * inv;
    let y = from - t;
    (t >= 0.0 && (to..=from).contains(&y)).then_some(y)
}

fn layer_of(name: &str) -> Layer {
    if name.to_ascii_lowercase().contains("water") {
        Layer::Water
    } else {
        Layer::Ground
    }
}

fn mirror(v: [f32; 3]) -> Vec3 {
    Vec3::new(-v[0], v[1], v[2])
}

/// Load `{map}_map.msb` (else `{map}.msb`) and every part whose `{model}_h.hmd` is in the map binder.
pub fn load_map(usrdir: &Path, map: &str) -> Result<Collision> {
    let folder = format!("model/map/{map}");
    let msb_path = {
        let named = format!("{folder}/{map}_map.msb");
        if usrdir.join(&named).is_file() {
            named
        } else {
            format!("{folder}/{map}.msb")
        }
    };
    let binder = format!("{folder}/{map}_m.dcx.bnd");
    let parts = msb::parts(&vfs::open(usrdir, &msb_path)?)?;
    let mut cache: HashMap<String, hmd::Hmd> = HashMap::new();
    let mut hits = Vec::new();
    for part in &parts {
        let h = match cache.get(&part.model) {
            Some(h) => h,
            None => {
                let asset = format!("{binder}|{}_h.hmd", part.model);
                let Ok(bytes) = vfs::open(usrdir, &asset) else {
                    continue;
                };
                cache.insert(
                    part.model.clone(),
                    hmd::read(&bytes).with_context(|| asset)?,
                );
                cache.get(&part.model).unwrap()
            }
        };
        let rot = part.rotation_deg.map(f32::to_radians);
        let xf = flver::Xform::local(part.translation, rot, part.scale);
        for tri in &h.triangles {
            let [a, b, c] = tri.verts.map(|i| mirror(xf.apply(h.vertices[i as usize])));
            let mat = h
                .materials
                .get(tri.material as usize)
                .map(String::as_str)
                .unwrap_or("");
            hits.push(Hit {
                a,
                b,
                c,
                layer: layer_of(mat),
            });
        }
    }
    Ok(Collision {
        planes: Vec::new(),
        hits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downward_ray_hits_triangle_and_plane() {
        let tri = Hit {
            a: Vec3::new(0.0, 1.0, 0.0),
            b: Vec3::new(2.0, 1.0, 0.0),
            c: Vec3::new(0.0, 1.0, 2.0),
            layer: Layer::Ground,
        };
        let c = Collision {
            planes: vec![Plane {
                y: 0.5,
                layer: Layer::Ground,
            }],
            hits: vec![tri],
        };
        assert_eq!(c.ground_below(Vec3::new(0.25, 10.0, 0.25)), Some(1.0));
        assert_eq!(
            c.ray_down(Vec3::new(0.25, 0.8, 0.25), f32::NEG_INFINITY, Layer::Ground),
            Some(0.5)
        );
        assert_eq!(c.ground_below(Vec3::new(10.0, 10.0, 10.0)), Some(0.5));
    }
}
