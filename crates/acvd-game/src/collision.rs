//! Map collision from disc `.hmd` hit models, placed by MSB `PARTS_PARAM_ST`. `--plane` keeps
//! the old infinite floor; `--water <y>` still adds a water plane. Vertices are mirrored on X
//! after the part transform, same as FLVER → Bevy (`acvd-render`).

use std::collections::HashMap;

use acvd_data::find;
use acvd_data::generated::ac_unit::PARAM_ACOBJECTPARAM_BIN;
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

/// Raycast filter matching the 360 HMD collision filter words (`sheets/hmd.csv`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RayFilter {
    /// FCS `LockCheckRay` mode 1 (`0x829c5e30` `0x80100006` + `0x829c5c00` -> `0x829aabc8`
    /// `AcObjectParam.camera_collidable`): solid terrain (`mask == 0x00`), excludes water/wire/
    /// camera/enehit/plyerhit/ackickrhit and objects with `camera_collidable == 0`.
    FcsLos,
    /// AI sightline `LockCheckRay` mode 2 (`0x829c5cd0` -> `0x829aacb8`
    /// `AcObjectParam.b_look_line_penetrate`): same as `FcsLos`, also skipping objects with
    /// `b_look_line_penetrate != 0`.
    AiLos,
    /// Bullet / missile terrain raycast (`0x828eea28` `0x00100004`): solid terrain and water,
    /// excluding wire (`0x04`) and invisible `camera`/`enehit`/`plyerhit`/`ackickrhit`.
    Bullet,
    /// Any `Layer::Ground` triangle.
    #[allow(dead_code)]
    Ground,
}

#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub a: Vec3,
    pub b: Vec3,
    pub c: Vec3,
    pub layer: Layer,
    /// Whether `WallJumpHitChecker` (`0x828aa500`, filter `0x100042`) can kick off this triangle.
    /// False for `water`/`ashore` (mask `0x02`), `camera` (`0x08`), `enehit` (`0x10`),
    /// `ackickrhit` (`0x60`, bit `0x40`), and objects with `AcObjectParam.enable_wall_jump == 0`
    /// (360 HMD material table `0x8371d460`, `0x82b9dc38` -> `0x82c52498`).
    pub wall_kickable: bool,
    /// Blocks FCS `LockCheckRay` mode 1 (`0x829c5e30` `0x80100006` + `0x829c5c00`).
    pub blocks_fcs_los: bool,
    /// Blocks AI `LockCheckRay` mode 2 (`0x829c5cd0` + `0x829aacb8`).
    pub blocks_ai_los: bool,
    /// Blocks projectiles and missiles (`0x828eea28` `0x00100004`).
    pub blocks_bullet: bool,
}

impl Hit {
    /// Convenience constructor for a solid ground/wall triangle (used in tests).
    #[cfg(test)]
    pub fn solid(a: Vec3, b: Vec3, c: Vec3) -> Self {
        Self {
            a,
            b,
            c,
            layer: Layer::Ground,
            wall_kickable: true,
            blocks_fcs_los: true,
            blocks_ai_los: true,
            blocks_bullet: true,
        }
    }

    #[inline]
    fn matches(&self, filter: RayFilter) -> bool {
        match filter {
            RayFilter::FcsLos => self.blocks_fcs_los,
            RayFilter::AiLos => self.blocks_ai_los,
            RayFilter::Bullet => self.blocks_bullet,
            RayFilter::Ground => self.layer == Layer::Ground,
        }
    }
}

#[derive(Resource, Default)]
pub struct Collision {
    pub planes: Vec<Plane>,
    pub hits: Vec<Hit>,
    /// Built by [`Collision::with_hits`]; empty means every query scans all of `hits`.
    grid: Grid,
    /// Flat 3D BVH built by [`Collision::with_hits`] for fast arbitrary 3D segment raycasts.
    bvh: Bvh,
}

/// Triangle indices per `CELL`-metre square of the XZ plane.
#[derive(Default)]
struct Grid {
    cells: HashMap<(i32, i32), Vec<u32>>,
}

/// Flat binary BVH over `Collision::hits`.
#[derive(Default)]
struct Bvh {
    nodes: Vec<BvhNode>,
    indices: Vec<u32>,
}

#[derive(Clone, Copy, Debug)]
struct BvhNode {
    min: Vec3,
    max: Vec3,
    /// When `count > 0`, first index into `Bvh::indices`; when `count == 0`, index of left child
    /// (right child is `left_or_first + 1`).
    left_or_first: u32,
    count: u32,
}

const BVH_LEAF_SIZE: usize = 8;

impl Bvh {
    fn build(hits: &[Hit]) -> Self {
        if hits.is_empty() {
            return Self::default();
        }
        let n = hits.len();
        let mut bounds: Vec<(Vec3, Vec3, Vec3)> = Vec::with_capacity(n);
        for h in hits {
            let lo = h.a.min(h.b).min(h.c);
            let hi = h.a.max(h.b).max(h.c);
            bounds.push((lo, hi, (lo + hi) * 0.5));
        }
        let mut indices: Vec<u32> = (0..n as u32).collect();
        let mut nodes = Vec::with_capacity(n / 2 + 1);
        nodes.push(BvhNode {
            min: Vec3::ZERO,
            max: Vec3::ZERO,
            left_or_first: 0,
            count: 0,
        });
        Self::build_node(&bounds, &mut indices, &mut nodes, 0, 0, n);
        Self { nodes, indices }
    }

    fn build_node(
        bounds: &[(Vec3, Vec3, Vec3)],
        indices: &mut [u32],
        nodes: &mut Vec<BvhNode>,
        node_idx: usize,
        start: usize,
        end: usize,
    ) {
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        let mut cmin = Vec3::splat(f32::INFINITY);
        let mut cmax = Vec3::splat(f32::NEG_INFINITY);
        for &i in &indices[start..end] {
            let (lo, hi, c) = bounds[i as usize];
            min = min.min(lo);
            max = max.max(hi);
            cmin = cmin.min(c);
            cmax = cmax.max(c);
        }
        let count = end - start;
        if count <= BVH_LEAF_SIZE {
            nodes[node_idx] = BvhNode {
                min,
                max,
                left_or_first: start as u32,
                count: count as u32,
            };
            return;
        }
        let ext = cmax - cmin;
        let axis = if ext.x >= ext.y && ext.x >= ext.z {
            0
        } else if ext.y >= ext.z {
            1
        } else {
            2
        };
        let split_val = 0.5 * (cmin[axis] + cmax[axis]);
        let mut mid = start;
        for i in start..end {
            let c = bounds[indices[i] as usize].2[axis];
            if c < split_val {
                indices.swap(i, mid);
                mid += 1;
            }
        }
        if mid == start || mid == end {
            mid = start + count / 2;
        }
        let left_idx = nodes.len();
        nodes.push(BvhNode {
            min: Vec3::ZERO,
            max: Vec3::ZERO,
            left_or_first: 0,
            count: 0,
        });
        nodes.push(BvhNode {
            min: Vec3::ZERO,
            max: Vec3::ZERO,
            left_or_first: 0,
            count: 0,
        });
        nodes[node_idx] = BvhNode {
            min,
            max,
            left_or_first: left_idx as u32,
            count: 0,
        };
        Self::build_node(bounds, indices, nodes, left_idx, start, mid);
        Self::build_node(bounds, indices, nodes, left_idx + 1, mid, end);
    }
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
        let bvh = Bvh::build(&hits);
        Self {
            planes,
            hits,
            grid: Grid { cells },
            bvh,
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

    /// Wall-kick sphere query (`WallJumpHitChecker`, 360 `0x828aa500` filter `0x100042`, sphere
    /// built by `0x82858f10` from `NewAcBehavior +0x628/+0x62c` = radius 6.0 m at height 3.0 m).
    /// Returns the unit outward normal of the closest `wall_kickable` triangle within `radius`
    /// whose normal satisfies `|n.y| <= max_normal_y` (`0x828ab428`, `NewAcBehavior +0x4f8` x
    /// `deg2rad` = 0.7854).
    pub fn wall_kick_contact(
        &self,
        feet: Vec3,
        radius: f32,
        height: f32,
        max_normal_y: f32,
    ) -> Option<Vec3> {
        let centre = feet + Vec3::Y * height;
        let r2 = radius * radius;
        let mut best: Option<(f32, Vec3)> = None;
        for h in self.near_disc(feet.x, feet.z, radius) {
            if !h.wall_kickable {
                continue;
            }
            let raw_n = (h.b - h.a).cross(h.c - h.a);
            let len = raw_n.length();
            if len < 1e-8 {
                continue;
            }
            let q = closest_on_tri(centre, h.a, h.b, h.c);
            let d = centre - q;
            let dist2 = d.length_squared();
            if dist2 >= r2 {
                continue;
            }
            let mut n = raw_n / len;
            if n.dot(d) < 0.0 {
                n = -n;
            }
            if n.y.abs() > max_normal_y {
                continue;
            }
            if best.is_none_or(|(b, _)| dist2 < b) {
                best = Some((dist2, n));
            }
        }
        best.map(|(_, n)| n)
    }

    /// 3D segment raycast from `origin` to `end` against the BVH-accelerated HMD mesh and
    /// any synthetic planes, respecting `filter`.
    ///
    /// Returns `Some((t, hit_point, normal))` for the earliest intersection with `t` in `0.0..=1.0`,
    /// where `normal` is a unit vector facing `origin`.
    pub fn raycast(&self, origin: Vec3, end: Vec3, filter: RayFilter) -> Option<(f32, Vec3, Vec3)> {
        let delta = end - origin;
        if delta.length_squared() < 1e-12 {
            return None;
        }
        let mut best_t = 1.0_f32;
        let mut best: Option<(f32, Vec3, Vec3)> = None;

        // Test synthetic planes (`--plane` / `--water`).
        for p in &self.planes {
            let plane_ok = match filter {
                RayFilter::Bullet => true,
                RayFilter::FcsLos | RayFilter::AiLos | RayFilter::Ground => {
                    p.layer == Layer::Ground
                }
            };
            if !plane_ok || delta.y.abs() < 1e-8 {
                continue;
            }
            let t = (p.y - origin.y) / delta.y;
            if t >= 0.0 && t <= best_t {
                best_t = t;
                let n = if delta.y < 0.0 { Vec3::Y } else { Vec3::NEG_Y };
                best = Some((t, origin + delta * t, n));
            }
        }

        // Traverse the flat BVH (or linear scan if `bvh` is empty, e.g. manually populated `hits`).
        if self.bvh.nodes.is_empty() {
            for h in &self.hits {
                if !h.matches(filter) {
                    continue;
                }
                if let Some((t, n)) = segment_hits_tri(origin, delta, h.a, h.b, h.c, best_t) {
                    best_t = t;
                    best = Some((t, origin + delta * t, n));
                }
            }
            return best;
        }

        let mut stack = [0usize; 64];
        let mut sp = 0usize;
        if segment_hits_aabb(origin, delta, self.bvh.nodes[0].min, self.bvh.nodes[0].max, best_t)
            .is_some()
        {
            stack[sp] = 0;
            sp += 1;
        }
        while sp > 0 {
            sp -= 1;
            let node = self.bvh.nodes[stack[sp]];
            if node.count > 0 {
                let first = node.left_or_first as usize;
                let last = first + node.count as usize;
                for &idx in &self.bvh.indices[first..last] {
                    let h = &self.hits[idx as usize];
                    if !h.matches(filter) {
                        continue;
                    }
                    if let Some((t, n)) = segment_hits_tri(origin, delta, h.a, h.b, h.c, best_t) {
                        best_t = t;
                        best = Some((t, origin + delta * t, n));
                    }
                }
            } else {
                let left = node.left_or_first as usize;
                let right = left + 1;
                let tl = segment_hits_aabb(
                    origin,
                    delta,
                    self.bvh.nodes[left].min,
                    self.bvh.nodes[left].max,
                    best_t,
                );
                let tr = segment_hits_aabb(
                    origin,
                    delta,
                    self.bvh.nodes[right].min,
                    self.bvh.nodes[right].max,
                    best_t,
                );
                match (tl, tr) {
                    (Some(a), Some(b)) => {
                        // Push farther child first so closer child is popped first.
                        if a <= b {
                            if sp + 2 <= stack.len() {
                                stack[sp] = right;
                                stack[sp + 1] = left;
                                sp += 2;
                            }
                        } else if sp + 2 <= stack.len() {
                            stack[sp] = left;
                            stack[sp + 1] = right;
                            sp += 2;
                        }
                    }
                    (Some(_), None) => {
                        if sp < stack.len() {
                            stack[sp] = left;
                            sp += 1;
                        }
                    }
                    (None, Some(_)) => {
                        if sp < stack.len() {
                            stack[sp] = right;
                            sp += 1;
                        }
                    }
                    (None, None) => {}
                }
            }
        }
        best
    }

    /// Returns `true` when there is an unobstructed line of sight between `origin` and `end`
    /// under `filter` (`LockCheckRay` `0x829c5f20`). Early-outs on the first occluding triangle.
    pub fn has_los(&self, origin: Vec3, end: Vec3, filter: RayFilter) -> bool {
        let delta = end - origin;
        if delta.length_squared() < 1e-12 {
            return true;
        }
        const EPS: f32 = 1e-4;
        let t_max = 1.0 - EPS;

        for p in &self.planes {
            let plane_ok = match filter {
                RayFilter::Bullet => true,
                RayFilter::FcsLos | RayFilter::AiLos | RayFilter::Ground => {
                    p.layer == Layer::Ground
                }
            };
            if !plane_ok || delta.y.abs() < 1e-8 {
                continue;
            }
            let t = (p.y - origin.y) / delta.y;
            if t > EPS && t < t_max {
                return false;
            }
        }

        if self.bvh.nodes.is_empty() {
            for h in &self.hits {
                if !h.matches(filter) {
                    continue;
                }
                if let Some((t, _)) = segment_hits_tri(origin, delta, h.a, h.b, h.c, t_max) {
                    if t > EPS {
                        return false;
                    }
                }
            }
            return true;
        }

        let mut stack = [0usize; 64];
        let mut sp = 0usize;
        if segment_hits_aabb(origin, delta, self.bvh.nodes[0].min, self.bvh.nodes[0].max, t_max)
            .is_some()
        {
            stack[sp] = 0;
            sp += 1;
        }
        while sp > 0 {
            sp -= 1;
            let node = self.bvh.nodes[stack[sp]];
            if node.count > 0 {
                let first = node.left_or_first as usize;
                let last = first + node.count as usize;
                for &idx in &self.bvh.indices[first..last] {
                    let h = &self.hits[idx as usize];
                    if !h.matches(filter) {
                        continue;
                    }
                    if let Some((t, _)) = segment_hits_tri(origin, delta, h.a, h.b, h.c, t_max) {
                        if t > EPS {
                            return false;
                        }
                    }
                }
            } else {
                let left = node.left_or_first as usize;
                let right = left + 1;
                if segment_hits_aabb(
                    origin,
                    delta,
                    self.bvh.nodes[left].min,
                    self.bvh.nodes[left].max,
                    t_max,
                )
                .is_some()
                    && sp < stack.len()
                {
                    stack[sp] = left;
                    sp += 1;
                }
                if segment_hits_aabb(
                    origin,
                    delta,
                    self.bvh.nodes[right].min,
                    self.bvh.nodes[right].max,
                    t_max,
                )
                .is_some()
                    && sp < stack.len()
                {
                    stack[sp] = right;
                    sp += 1;
                }
            }
        }
        true
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

/// Segment `origin + t * delta` (`t` in `[0, t_max]`) vs axis-aligned box `[min, max]`.
/// Returns entry parameter `t_enter` if the segment intersects the box.
#[inline]
fn segment_hits_aabb(origin: Vec3, delta: Vec3, min: Vec3, max: Vec3, t_max: f32) -> Option<f32> {
    let mut t0 = 0.0_f32;
    let mut t1 = t_max;
    for axis in 0..3 {
        let d = delta[axis];
        let o = origin[axis];
        if d.abs() < 1e-12 {
            if o < min[axis] || o > max[axis] {
                return None;
            }
        } else {
            let inv = 1.0 / d;
            let mut a = (min[axis] - o) * inv;
            let mut b = (max[axis] - o) * inv;
            if a > b {
                std::mem::swap(&mut a, &mut b);
            }
            t0 = t0.max(a);
            t1 = t1.min(b);
            if t0 > t1 {
                return None;
            }
        }
    }
    Some(t0)
}

/// Two-sided Möller–Trumbore segment-vs-triangle test for `t` in `[0, t_max]`.
/// Returns `(t, unit_normal)` with `normal` oriented to face `origin`.
#[inline]
fn segment_hits_tri(
    origin: Vec3,
    delta: Vec3,
    a: Vec3,
    b: Vec3,
    c: Vec3,
    t_max: f32,
) -> Option<(f32, Vec3)> {
    let e1 = b - a;
    let e2 = c - a;
    let pvec = delta.cross(e2);
    let det = e1.dot(pvec);
    if det.abs() < 1e-8 {
        return None;
    }
    let inv = 1.0 / det;
    let tvec = origin - a;
    let u = tvec.dot(pvec) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let qvec = tvec.cross(e1);
    let v = delta.dot(qvec) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(qvec) * inv;
    if t < 0.0 || t > t_max {
        return None;
    }
    let raw_n = e1.cross(e2);
    let len = raw_n.length();
    let mut n = if len > 1e-8 { raw_n / len } else { Vec3::Y };
    if n.dot(delta) > 0.0 {
        n = -n;
    }
    Some((t, n))
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

/// HMD material filter mask from the 360 table at `0x8371d460` (`0x82b9db78` / `0x82b9dc38` ->
/// `0x82c52498`): `water`/`ashore` = `0x02`, `camera` = `0x08`, `enehit` = `0x10`,
/// `plyerhit` = `0x20`, `ackickrhit` = `0x60` (`0x20 | 0x40`), all other surfaces = `0x00`.
pub fn material_mask(name: &str) -> u8 {
    let lower = name.to_ascii_lowercase();
    if lower.contains("ackickrhit") {
        0x60
    } else if lower.contains("plyerhit") {
        0x20
    } else if lower.contains("enehit") {
        0x10
    } else if lower.contains("camera") {
        0x08
    } else if lower.contains("bottomwater") {
        0x00
    } else if lower.contains("water") || lower.contains("ashore") {
        0x02
    } else {
        0x00
    }
}

fn layer_of(mask: u8) -> Layer {
    if mask & 0x02 != 0 {
        Layer::Water
    } else {
        Layer::Ground
    }
}

/// Object collision flags from `param/acobjectparam.bin` (default row 0 for map pieces or
/// objects without a dedicated row):
/// - `enable_wall_jump` (`+0x03`, `WallJumpHitChecker` `0x828aa500`)
/// - `camera_collidable` (`+0x02`, `LockCheckRay` mode 1 `0x829c5c00` -> `0x829aabc8`)
/// - `b_look_line_penetrate` (`+0x0c`, `LockCheckRay` mode 2 `0x829c5cd0` -> `0x829aacb8`)
fn object_flags(model: &str) -> (bool, bool, bool) {
    let Some(id) = model
        .strip_prefix('o')
        .or_else(|| model.strip_prefix('O'))
        .and_then(|s| s.parse::<u32>().ok())
    else {
        return (true, true, false);
    };
    find(PARAM_ACOBJECTPARAM_BIN, id)
        .or_else(|| find(PARAM_ACOBJECTPARAM_BIN, 0))
        .map_or((true, true, false), |r| {
            (
                r.data.enable_wall_jump != 0,
                r.data.camera_collidable != 0,
                r.data.b_look_line_penetrate != 0,
            )
        })
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
        let (obj_kickable, obj_cam_collidable, obj_look_penetrate) = object_flags(&part.model);
        for tri in &h.triangles {
            let [a, b, c] = tri.verts.map(|i| mirror(xf.apply(verts[i as usize])));
            let mat = h
                .materials
                .get(tri.material as usize)
                .map(String::as_str)
                .unwrap_or("");
            let mask = material_mask(mat);
            let blocks_fcs_los = (mask & 0x7e) == 0 && obj_cam_collidable;
            hits.push(Hit {
                a,
                b,
                c,
                layer: layer_of(mask),
                wall_kickable: obj_kickable && (mask & 0x5a) == 0,
                blocks_fcs_los,
                blocks_ai_los: blocks_fcs_los && !obj_look_penetrate,
                blocks_bullet: (mask & 0x7c) == 0,
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
        let tri = Hit::solid(
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(2.0, 1.0, 0.0),
            Vec3::new(0.0, 1.0, 2.0),
        );
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
        let wall = |a: [f32; 3], b: [f32; 3], c: [f32; 3]| {
            Hit::solid(Vec3::from(a), Vec3::from(b), Vec3::from(c))
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

    #[test]
    fn wall_kick_contact_filters_material_and_angle() {
        let max_ny = 45.0_f32.to_radians();
        let make = |a: [f32; 3], b: [f32; 3], c: [f32; 3], mat: &str| {
            let mask = material_mask(mat);
            let blocks_fcs_los = (mask & 0x7e) == 0;
            Hit {
                a: Vec3::from(a),
                b: Vec3::from(b),
                c: Vec3::from(c),
                layer: layer_of(mask),
                wall_kickable: (mask & 0x5a) == 0,
                blocks_fcs_los,
                blocks_ai_los: blocks_fcs_los,
                blocks_bullet: (mask & 0x7c) == 0,
            }
        };
        let concrete_wall = Collision::with_hits(
            Vec::new(),
            vec![
                make([0.0, 0.0, -50.0], [0.0, 20.0, -50.0], [0.0, 0.0, 50.0], "uconcrete.tga"),
                make([-50.0, 0.0, -50.0], [50.0, 0.0, -50.0], [-50.0, 0.0, 50.0], "uconcrete.tga"),
            ],
        );
        // Within 6.0 m of x = 0 wall at height 3.0 m: returns outward normal (+1, 0, 0).
        let n = concrete_wall
            .wall_kick_contact(Vec3::new(5.5, 1.0, 0.0), 6.0, 3.0, max_ny)
            .expect("should hit concrete wall");
        assert!((n - Vec3::X).length() < 1e-4, "{n}");
        // Beyond 6.0 m from the wall (only floor is within 6.0 m): floor rejected by |n.y| > 0.7854.
        assert!(concrete_wall
            .wall_kick_contact(Vec3::new(7.0, 1.0, 0.0), 6.0, 3.0, max_ny)
            .is_none());

        // `ackickrhit` (mask 0x60) is rejected by WallJumpHitChecker filter 0x100042.
        let blocked_wall = Collision::with_hits(
            Vec::new(),
            vec![make(
                [0.0, 0.0, -50.0],
                [0.0, 20.0, -50.0],
                [0.0, 0.0, 50.0],
                "ackickrhit.tga",
            )],
        );
        assert!(blocked_wall
            .wall_kick_contact(Vec3::new(3.0, 1.0, 0.0), 6.0, 3.0, max_ny)
            .is_none());
    }

    fn raycast_brute_force(
        c: &Collision,
        origin: Vec3,
        end: Vec3,
        filter: RayFilter,
    ) -> Option<(f32, Vec3, Vec3)> {
        let delta = end - origin;
        if delta.length_squared() < 1e-12 {
            return None;
        }
        let mut best_t = 1.0_f32;
        let mut best: Option<(f32, Vec3, Vec3)> = None;
        for h in &c.hits {
            if !h.matches(filter) {
                continue;
            }
            if let Some((t, n)) = segment_hits_tri(origin, delta, h.a, h.b, h.c, best_t) {
                best_t = t;
                best = Some((t, origin + delta * t, n));
            }
        }
        best
    }

    #[test]
    fn raycast_3d_hits_walls_respects_filters_and_matches_brute_force() {
        // Two walls at z = -10 (plyerhit, invisible to LOS/bullets) and z = -20 (concrete, solid).
        let make_quad = |z: f32, mat: &str| {
            let mask = material_mask(mat);
            let blocks_fcs_los = (mask & 0x7e) == 0;
            let tri = |a: [f32; 3], b: [f32; 3], c: [f32; 3]| Hit {
                a: Vec3::from(a),
                b: Vec3::from(b),
                c: Vec3::from(c),
                layer: layer_of(mask),
                wall_kickable: (mask & 0x5a) == 0,
                blocks_fcs_los,
                blocks_ai_los: blocks_fcs_los,
                blocks_bullet: (mask & 0x7c) == 0,
            };
            [
                tri([-10.0, 0.0, z], [10.0, 0.0, z], [-10.0, 20.0, z]),
                tri([10.0, 0.0, z], [10.0, 20.0, z], [-10.0, 20.0, z]),
            ]
        };
        let mut hits = Vec::new();
        hits.extend(make_quad(-10.0, "plyerhit.tga"));
        hits.extend(make_quad(-20.0, "uconcrete.tga"));
        // Add 60 extra scattered solid triangles so the BVH splits into multiple levels.
        for i in 0..30 {
            let x = (i as f32 - 15.0) * 7.0;
            let z = -30.0 - (i as f32) * 3.0;
            hits.push(Hit::solid(
                Vec3::new(x, 0.0, z),
                Vec3::new(x + 4.0, 0.0, z),
                Vec3::new(x + 2.0, 8.0, z + 1.0),
            ));
            hits.push(Hit::solid(
                Vec3::new(x, 8.0, z),
                Vec3::new(x + 4.0, 8.0, z),
                Vec3::new(x + 2.0, 14.0, z - 1.0),
            ));
        }
        let c = Collision::with_hits(Vec::new(), hits);

        // Ray from (0, 5, 0) to (0, 5, -40) passes through plyerhit (-10) and hits concrete (-20).
        let (t, pos, normal) = c
            .raycast(Vec3::new(0.0, 5.0, 0.0), Vec3::new(0.0, 5.0, -40.0), RayFilter::Bullet)
            .expect("should hit concrete wall");
        assert!((t - 0.5).abs() < 1e-4, "t={t}");
        assert!((pos - Vec3::new(0.0, 5.0, -20.0)).length() < 1e-4, "pos={pos}");
        assert!((normal - Vec3::Z).length() < 1e-4, "normal facing origin: {normal}");
        assert!(!c.has_los(Vec3::new(0.0, 5.0, 0.0), Vec3::new(0.0, 5.0, -40.0), RayFilter::FcsLos));
        assert!(c.has_los(Vec3::new(0.0, 5.0, 0.0), Vec3::new(0.0, 5.0, -15.0), RayFilter::FcsLos));

        // Verify BVH matches brute force on a sweep of rays.
        for i in -12..=12 {
            let orig = Vec3::new(i as f32 * 3.5, 4.0, 5.0);
            let end = Vec3::new(i as f32 * 3.5 + 1.5, 6.0, -120.0);
            let bvh = c.raycast(orig, end, RayFilter::Bullet);
            let bf = raycast_brute_force(&c, orig, end, RayFilter::Bullet);
            match (bvh, bf) {
                (Some((tb, pb, _)), Some((tf, pf, _))) => {
                    assert!((tb - tf).abs() < 1e-4, "ray {i}: bvh t={tb} vs bf t={tf}");
                    assert!((pb - pf).length() < 1e-3, "ray {i}: bvh {pb} vs bf {pf}");
                }
                (None, None) => {}
                (a, b) => panic!("ray {i} mismatch: bvh={a:?} bf={b:?}"),
            }
        }
    }

    #[test]
    fn m4000_bvh_raycast_matches_brute_force_and_hits_start_wall() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let Ok(disc) = Disc::open(&vfs::default_disc(&root)) else {
            return;
        };
        let c = load_map(&disc, "m4000").expect("load m4000 collision");
        assert!(c.hits.len() > 40_000, "m4000 hits = {}", c.hits.len());
        assert!(!c.bvh.nodes.is_empty());

        // Start point on m4000 is Bevy (324.65, 29.0, 649.5), facing +X toward a wall.
        let origin = Vec3::new(324.65, 33.5, 649.5);
        let dirs = [
            Vec3::X,
            Vec3::NEG_X,
            Vec3::Z,
            Vec3::NEG_Z,
            Vec3::new(1.0, -0.3, 0.5).normalize(),
            Vec3::new(-0.7, 0.2, -0.7).normalize(),
            Vec3::NEG_Y,
        ];
        for dir in dirs {
            let end = origin + dir * 200.0;
            let bvh = c.raycast(origin, end, RayFilter::FcsLos);
            let bf = raycast_brute_force(&c, origin, end, RayFilter::FcsLos);
            match (bvh, bf) {
                (Some((tb, pb, _)), Some((tf, pf, _))) => {
                    assert!((tb - tf).abs() < 1e-4, "dir {dir}: bvh t={tb} vs bf t={tf}");
                    assert!((pb - pf).length() < 1e-2, "dir {dir}: bvh {pb} vs bf {pf}");
                }
                (None, None) => {}
                (a, b) => panic!("dir {dir} mismatch: bvh={a:?} bf={b:?}"),
            }
        }
        // Straight down must hit the m4000 ground under the start point.
        assert!(
            c.raycast(origin, origin + Vec3::NEG_Y * 50.0, RayFilter::Bullet)
                .is_some(),
            "downward ray from m4000 start should hit ground"
        );
    }
}
