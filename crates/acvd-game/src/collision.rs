//! Map collision from disc `.hmd` hit models, placed by MSB `PARTS_PARAM_ST`. `--plane` keeps
//! the old infinite floor; `--water <y>` still adds a water plane. Vertices are mirrored on X
//! after the part transform, same as FLVER → Bevy (`acvd-render`).

use std::collections::HashMap;

use acvd_formats::vfs::{self, Disc};
use acvd_formats::{hmd, msb};
use anyhow::Result;
use bevy::prelude::*;

/// Half the length of the eye-floor ray, above and below its start (360 0x8371914c).
pub const RAY_HALF: f32 = 50.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layer {
    /// What the AC stands on. Filter 0x100002: the AC's down rays are cast by the
    /// WorldRayCastManager pair 0x829ea0d8 (0x829e7b68 never runs in AC TEST,
    /// `private/xenia/wall_walk2.txt`).
    Ground,
    /// Filter 0x200002 (360 0x829e7be0; for the AC, 0x829ea0d8 site 0x829ea30c casts it 100 m
    /// down from 54.6 m above the feet); water by the
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
    /// Built by [`Collision::with_hits`]; empty means every query scans all of `hits`.
    grid: Grid,
}

/// Triangle indices per `CELL`-metre square of the XZ plane.
#[derive(Default)]
struct Grid {
    cells: HashMap<(i32, i32), Vec<u32>>,
}

/// Grid cell size in metres. Not game data.
const CELL: f32 = 16.0;

fn cell(v: f32) -> i32 {
    (v / CELL).floor() as i32
}

impl Collision {
    pub fn with_hits(planes: Vec<Plane>, hits: Vec<Hit>) -> Self {
        let mut cells: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
        for (i, h) in hits.iter().enumerate() {
            let (lo, hi) = (h.a.min(h.b).min(h.c), h.a.max(h.b).max(h.c));
            for x in cell(lo.x)..=cell(hi.x) {
                for z in cell(lo.z)..=cell(hi.z) {
                    cells.entry((x, z)).or_default().push(i as u32);
                }
            }
        }
        Self {
            planes,
            hits,
            grid: Grid { cells },
        }
    }

    /// Triangles whose XZ bounds may contain the point (`x`, `z`).
    fn near(&self, x: f32, z: f32) -> Box<dyn Iterator<Item = &Hit> + '_> {
        if self.grid.cells.is_empty() {
            return Box::new(self.hits.iter());
        }
        let ids = self
            .grid
            .cells
            .get(&(cell(x), cell(z)))
            .map_or(&[][..], Vec::as_slice);
        Box::new(ids.iter().map(|&i| &self.hits[i as usize]))
    }

    /// Height of the first `layer` surface a ray straight down from `from` meets before `to`.
    pub fn ray_down(&self, from: Vec3, to: f32, layer: Layer) -> Option<f32> {
        let plane = self
            .planes
            .iter()
            .filter(|p| p.layer == layer && (to..=from.y).contains(&p.y))
            .map(|p| p.y)
            .reduce(f32::max);
        let mesh = self
            .near(from.x, from.z)
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

    /// Ground triangles whose XZ bounds may come within `r` of (`x`, `z`), each once.
    fn near_disc(&self, x: f32, z: f32, r: f32) -> Vec<&Hit> {
        if self.grid.cells.is_empty() {
            return self.hits.iter().filter(|h| h.layer == Layer::Ground).collect();
        }
        let mut ids: Vec<u32> = Vec::new();
        for cx in cell(x - r)..=cell(x + r) {
            for cz in cell(z - r)..=cell(z + r) {
                if let Some(v) = self.grid.cells.get(&(cx, cz)) {
                    ids.extend_from_slice(v);
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter()
            .map(|i| &self.hits[i as usize])
            .filter(|h| h.layer == Layer::Ground)
            .collect()
    }

    /// Moves `feet` sideways until none of the AC body spheres overlaps a wall. Returns the
    /// unit XZ normal of each push, for removing the velocity into the wall.
    pub fn push_out(&self, feet: &mut Vec3) -> Vec<Vec2> {
        let reach = BODY.iter().map(|s| s.radius).fold(0.0, f32::max);
        // Each pass moves the feet at most one radius.
        let tris = self.near_disc(feet.x, feet.z, reach * (1 + PUSH_PASSES) as f32);
        let mut normals = Vec::new();
        for _ in 0..PUSH_PASSES {
            let mut deepest: Option<(f32, Vec2)> = None;
            for s in &BODY {
                let centre = *feet + Vec3::Y * s.height;
                for h in &tris {
                    let n = (h.b - h.a).cross(h.c - h.a);
                    let len = n.length();
                    if len < 1e-8 || (n.y / len).abs() >= FLOOR_NORMAL_Y {
                        continue;
                    }
                    let d = centre - closest_on_tri(centre, h.a, h.b, h.c);
                    let flat = Vec2::new(d.x, d.z);
                    let dist = d.length();
                    if dist >= s.radius || flat.length_squared() < 1e-10 {
                        continue;
                    }
                    // Horizontal push that clears the sphere at this contact height.
                    let need = (s.radius * s.radius - d.y * d.y).max(0.0).sqrt() - flat.length();
                    if need > deepest.map_or(0.0, |(a, _)| a) {
                        deepest = Some((need, flat.normalize()));
                    }
                }
            }
            let Some((need, dir)) = deepest else { break };
            feet.x += dir.x * need;
            feet.z += dir.y * need;
            normals.push(dir);
        }
        normals
    }
}

/// One AC body sphere: radius, centre height above the feet.
#[derive(Clone, Copy, Debug)]
pub struct BodySphere {
    pub radius: f32,
    pub height: f32,
}

/// The AC's map spheres, built by 0x82863b60 (from 0x82858460) out of tuning record 0
/// (0x82330c38): lower = radius +0x22c at height +0x234 (filter 0x100022), upper = +0x230
/// (filter 0x4100022). Swept old -> new position each frame by the AC hit update 0x828651d0
/// through 0x82c441f8 (radius at sphere +0x8, offset +0x10). Values from the Xenia probe
/// `private/xenia/wall_walk4.txt` in AC TEST: lower 3.0 at 3.0, upper 5.5 at 6.0.
pub const BODY: [BodySphere; 2] = [
    BodySphere {
        radius: 3.0,
        height: 3.0,
    },
    BodySphere {
        radius: 5.5,
        height: 6.0,
    },
];

/// Triangles with |normal.y| at or above this are floors / ceilings, left to the ground ray.
/// Not game data.
const FLOOR_NORMAL_Y: f32 = 0.7;

/// Resolve passes per tick; the 360 sweeps the lower sphere up to three times a frame
/// (`private/xenia/wall_walk4.jsonl`).
const PUSH_PASSES: usize = 3;

/// Closest point to `p` on triangle `abc` (Ericson, Real-Time Collision Detection 5.1.5).
fn closest_on_tri(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Vec3 {
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return a + ab * (d1 / (d1 - d3));
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return a + ac * (d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && d4 - d3 >= 0.0 && d5 - d6 >= 0.0 {
        return b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let denom = 1.0 / (va + vb + vc);
    a + ab * (vb * denom) + ac * (vc * denom)
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

/// Load the terrain MSB (`map::terrain_msb`) and every map piece or object whose `{model}_h.hmd`
/// is in its binder (`map::binder`).
pub fn load_map(disc: &Disc, map: &str) -> Result<Collision> {
    let parts = msb::parts(&vfs::open(disc, &crate::map::terrain_msb(disc, map))?)?;
    let mut cache: HashMap<String, Option<(hmd::Hmd, Vec<hmd::Vec3>)>> = HashMap::new();
    let mut hits = Vec::new();
    for part in &parts {
        let Some(binder) = crate::map::binder(map, part) else {
            continue;
        };
        let asset = format!("{binder}|{}_h.hmd", part.model);
        if !cache.contains_key(&asset) {
            let h = match vfs::open(disc, &asset) {
                Ok(bytes) => match hmd::read(&bytes) {
                    Ok(h) => {
                        let verts = h.model_vertices();
                        Some((h, verts))
                    }
                    Err(e) => {
                        warn!("{asset}: {e:#}");
                        None
                    }
                },
                Err(_) => None,
            };
            cache.insert(asset.clone(), h);
        }
        let Some((h, verts)) = cache[&asset].as_ref() else {
            continue;
        };
        let xf = crate::map::xform(part);
        for tri in &h.triangles {
            let [a, b, c] = tri.verts.map(|i| mirror(xf.apply(verts[i as usize])));
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
    Ok(Collision::with_hits(Vec::new(), hits))
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
        let c = Collision::with_hits(
            vec![Plane {
                y: 0.5,
                layer: Layer::Ground,
            }],
            vec![tri],
        );
        assert_eq!(c.ground_below(Vec3::new(0.25, 10.0, 0.25)), Some(1.0));
        assert_eq!(
            c.ray_down(Vec3::new(0.25, 0.8, 0.25), f32::NEG_INFINITY, Layer::Ground),
            Some(0.5)
        );
        assert_eq!(c.ground_below(Vec3::new(10.0, 10.0, 10.0)), Some(0.5));
    }

    #[test]
    fn body_spheres_push_out_of_wall_not_floor() {
        // Wall in the plane x = 0, 0..20 m tall; floor at y = 0.
        let wall = |a: [f32; 3], b: [f32; 3], c: [f32; 3]| Hit {
            a: Vec3::from(a),
            b: Vec3::from(b),
            c: Vec3::from(c),
            layer: Layer::Ground,
        };
        let c = Collision::with_hits(
            Vec::new(),
            vec![
                wall([0.0, 0.0, -50.0], [0.0, 20.0, -50.0], [0.0, 0.0, 50.0]),
                wall([0.0, 20.0, -50.0], [0.0, 20.0, 50.0], [0.0, 0.0, 50.0]),
                wall([-50.0, 0.0, -50.0], [50.0, 0.0, -50.0], [-50.0, 0.0, 50.0]),
            ],
        );
        let mut feet = Vec3::new(2.0, 0.0, 0.0);
        let normals = c.push_out(&mut feet);
        assert!((feet.x - 5.5).abs() < 1e-4, "{feet}");
        assert_eq!((feet.y, feet.z), (0.0, 0.0));
        assert!(normals.iter().all(|n| (n.x - 1.0).abs() < 1e-5));
        let mut clear = Vec3::new(8.0, 0.0, 3.0);
        assert!(c.push_out(&mut clear).is_empty());
        assert_eq!(clear, Vec3::new(8.0, 0.0, 3.0));
    }
}
