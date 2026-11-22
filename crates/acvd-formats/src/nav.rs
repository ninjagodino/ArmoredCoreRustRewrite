//! Navigation mesh (`model/map/ch_nav/*.hnav`) and height/AABB hierarchy tree
//! (`model/map/ch_nav/*.htr`) readers and A* pathfinder.
//!
//! 360 binary layout traced from `ACV2.pe`:
//! - `HNAV` magic/version check: `0x823fbf38` (`"HNAV"` at `0x8201a19c`, `"0002"` at `0x8201a194`).
//! - `HNAV` loader: `0x823ef980` → `0x823fbfc0` (copies transform header at `+0x10`),
//!   `0x823fc068` (populates node table from `+0x14`), `0x823fc5a8` (reads macro node entries at
//!   `+0x08`, stride `0x12`, child node ID at `+0x18`).
//! - `HTR` (`"HTR\0"` + `"0001"`): 8-ary quantized AABB hierarchy tree indexing `HNAV` macro
//!   regions in the same grid space (`world_to_grid` / `grid_to_world` 4×4 matrices).
//!
//! All world coordinates are in FLVER/game axes (before Bevy's X-mirror).

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use anyhow::{bail, ensure, Context, Result};

use crate::reader::Be;

/// Cursor wrapper around [`Be`] for sequential big-endian reads.
struct Cursor<'a> {
    be: Be<'a>,
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            be: Be(data),
            pos: 0,
        }
    }
    fn seek(&mut self, at: usize) -> Result<()> {
        ensure!(at <= self.be.len(), "seek {at:#x} past end {:#x}", self.be.len());
        self.pos = at;
        Ok(())
    }
    fn u8(&mut self) -> Result<u8> {
        let v = self.be.u8(self.pos)?;
        self.pos += 1;
        Ok(v)
    }
    fn u16(&mut self) -> Result<u16> {
        let v = self.be.u16(self.pos)?;
        self.pos += 2;
        Ok(v)
    }
    fn u32(&mut self) -> Result<u32> {
        let v = self.be.u32(self.pos)?;
        self.pos += 4;
        Ok(v)
    }
    fn f32(&mut self) -> Result<f32> {
        let v = self.be.f32(self.pos)?;
        self.pos += 4;
        Ok(v)
    }
}

/// 4×4 row-major transform matrix stored in `HNAV` and `HTR` headers.
/// Transforms a row vector `[x, y, z, 1] * M`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat4([f32; 16]);

impl Mat4 {
    fn read(r: &mut Cursor<'_>) -> Result<Self> {
        let mut m = [0.0f32; 16];
        for v in &mut m {
            *v = r.f32()?;
        }
        Ok(Self(m))
    }

    /// Transforms a 3D point `[x, y, z]` by `[x, y, z, 1] * M`.
    #[inline]
    pub fn transform_point(&self, p: [f32; 3]) -> [f32; 3] {
        let m = &self.0;
        [
            p[0] * m[0] + p[1] * m[4] + p[2] * m[8] + m[12],
            p[0] * m[1] + p[1] * m[5] + p[2] * m[9] + m[13],
            p[0] * m[2] + p[1] * m[6] + p[2] * m[10] + m[14],
        ]
    }

    pub fn raw(&self) -> &[f32; 16] {
        &self.0
    }
}

/// One 18-byte macro region box in an `HNAV` `kind == 1` node (`+0x08`, stride `0x12`,
/// `0x823fc5a8`). Coordinates are quantized in grid space (`grid_to_world` maps to world metres).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacroRegion {
    pub x: u16,
    pub y: u16,
    pub z: u16,
    pub dx: u16,
    pub dy: u16,
    pub dz: u16,
    pub flags: u16,
    pub attr: u16,
    /// `HNAV` micro-node ID (`1..=node_count`) holding this region's sub-cells.
    pub child_node_id: u16,
}

impl MacroRegion {
    /// World-space AABB `(min, max)` of this region in FLVER/game axes.
    pub fn world_bounds(&self, g2w: &Mat4) -> ([f32; 3], [f32; 3]) {
        let a = g2w.transform_point([f32::from(self.x), f32::from(self.y), f32::from(self.z)]);
        let b = g2w.transform_point([
            f32::from(self.x) + f32::from(self.dx),
            f32::from(self.y) + f32::from(self.dy),
            f32::from(self.z) + f32::from(self.dz),
        ]);
        (
            [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])],
            [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])],
        )
    }

    /// World-space center of this region's floor (`y = min_y`) in FLVER/game axes.
    pub fn world_center(&self, g2w: &Mat4) -> [f32; 3] {
        let (min, max) = self.world_bounds(g2w);
        [
            0.5 * (min[0] + max[0]),
            min[1],
            0.5 * (min[2] + max[2]),
        ]
    }
}

/// One 10-byte local sub-cell inside an `HNAV` `kind == 0` micro node.
/// Coordinates are local offsets within the owning `MacroRegion`'s `(x, y, z)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MicroCell {
    pub x: u8,
    pub y: u8,
    pub z: u8,
    pub dx: u8,
    pub dy: u8,
    pub dz: u8,
    pub flags: u16,
    pub attr: u16,
}

/// Top-level macro graph node (`kind == 1`, usually node ID `0`).
#[derive(Debug, Clone)]
pub struct MacroNode {
    pub id: u16,
    pub regions: Vec<MacroRegion>,
    /// CSR adjacency offsets (`len == regions.len() + 1`).
    pub csr_offsets: Vec<u32>,
    /// Adjacent macro region indices (`0..regions.len()`).
    pub neighbors: Vec<u16>,
}

impl MacroNode {
    /// Adjacent region indices of `region_idx`.
    pub fn neighbors_of(&self, region_idx: usize) -> &[u16] {
        let start = self.csr_offsets[region_idx] as usize;
        let end = self.csr_offsets[region_idx + 1] as usize;
        &self.neighbors[start..end]
    }
}

/// Leaf micro-cell graph node (`kind == 0`, one per macro region).
#[derive(Debug, Clone)]
pub struct MicroNode {
    pub id: u16,
    pub cells: Vec<MicroCell>,
    /// CSR adjacency offsets (`len == cells.len() + 1`).
    pub csr_offsets: Vec<u16>,
    /// Adjacent local cell indices (`< 0xFF00`) or external border portals (`>= 0xFF00`).
    pub neighbors: Vec<u16>,
}

/// Parsed `model/map/ch_nav/*.hnav` navigation mesh.
#[derive(Debug, Clone)]
pub struct Hnav {
    pub grid_to_world: Mat4,
    pub world_to_grid: Mat4,
    pub macro_nodes: Vec<MacroNode>,
    pub micro_nodes: Vec<MicroNode>,
}

/// Parses a big-endian 360 `HNAV` file (`HNAV0002`).
pub fn read_hnav(data: &[u8]) -> Result<Hnav> {
    ensure!(data.len() >= 0x20, "HNAV too short ({} bytes)", data.len());
    ensure!(&data[0..4] == b"HNAV", "HNAV bad magic: {:?}", &data[0..4]);
    ensure!(
        &data[4..8] == b"0002",
        "HNAV unsupported version: {:?}",
        &data[4..8]
    );

    let mut r = Cursor::new(data);
    r.seek(0x10)?;
    let header_off = r.u32()? as usize;
    let nodes_off = r.u32()? as usize;

    ensure!(
        header_off + 0x80 <= data.len(),
        "HNAV header offset {header_off:#x} out of range"
    );
    r.seek(header_off)?;
    let grid_to_world = Mat4::read(&mut r)?;
    let world_to_grid = Mat4::read(&mut r)?;

    ensure!(
        nodes_off + 4 <= data.len(),
        "HNAV nodes offset {nodes_off:#x} out of range"
    );
    r.seek(nodes_off)?;
    let node_count = r.u32()? as usize;
    let mut rel_offs = Vec::with_capacity(node_count);
    for _ in 0..node_count {
        rel_offs.push(r.u32()? as usize);
    }

    let mut macro_nodes = Vec::new();
    let mut micro_nodes = Vec::new();

    for (idx, &rel) in rel_offs.iter().enumerate() {
        let start = nodes_off
            .checked_add(rel)
            .context("HNAV node offset overflow")?;
        let end = rel_offs
            .get(idx + 1)
            .map(|&n| nodes_off + n)
            .unwrap_or(data.len());
        ensure!(
            start <= end && end <= data.len(),
            "HNAV node {idx} bounds {start:#x}..{end:#x} out of range"
        );
        let slice = &data[start..end];
        let mut nr = Cursor::new(slice);
        let kind = nr.u16()?;
        let id = nr.u16()?;
        let count = nr.u16()? as usize;
        let _pad = nr.u16()?;

        match kind {
            1 => {
                let mut regions = Vec::with_capacity(count);
                for _ in 0..count {
                    regions.push(MacroRegion {
                        x: nr.u16()?,
                        y: nr.u16()?,
                        z: nr.u16()?,
                        dx: nr.u16()?,
                        dy: nr.u16()?,
                        dz: nr.u16()?,
                        flags: nr.u16()?,
                        attr: nr.u16()?,
                        child_node_id: nr.u16()?,
                    });
                }
                let mut csr_offsets = Vec::with_capacity(count + 1);
                for _ in 0..=count {
                    csr_offsets.push(nr.u32()?);
                }
                let total_adj = *csr_offsets.last().unwrap_or(&0) as usize;
                let mut neighbors = Vec::with_capacity(total_adj);
                for _ in 0..total_adj {
                    let n = nr.u16()?;
                    ensure!(
                        (n as usize) < count,
                        "HNAV macro node {id} neighbor {n} >= count {count}"
                    );
                    neighbors.push(n);
                }
                macro_nodes.push(MacroNode {
                    id,
                    regions,
                    csr_offsets,
                    neighbors,
                });
            }
            0 => {
                let mut cells = Vec::with_capacity(count);
                for _ in 0..count {
                    cells.push(MicroCell {
                        x: nr.u8()?,
                        y: nr.u8()?,
                        z: nr.u8()?,
                        dx: nr.u8()?,
                        dy: nr.u8()?,
                        dz: nr.u8()?,
                        flags: nr.u16()?,
                        attr: nr.u16()?,
                    });
                }
                let mut csr_offsets = Vec::with_capacity(count + 1);
                for _ in 0..=count {
                    csr_offsets.push(nr.u16()?);
                }
                let total_adj = *csr_offsets.last().unwrap_or(&0) as usize;
                let mut neighbors = Vec::with_capacity(total_adj);
                for _ in 0..total_adj {
                    neighbors.push(nr.u16()?);
                }
                micro_nodes.push(MicroNode {
                    id,
                    cells,
                    csr_offsets,
                    neighbors,
                });
            }
            other => bail!("HNAV node {idx} unknown kind {other}"),
        }
    }

    Ok(Hnav {
        grid_to_world,
        world_to_grid,
        macro_nodes,
        micro_nodes,
    })
}

/// One 112-byte (`0x70`) 8-ary quantized AABB node in an `HTR` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HtrNode {
    pub min_x: [u16; 8],
    pub min_y: [u16; 8],
    pub min_z: [u16; 8],
    pub max_x: [u16; 8],
    pub max_y: [u16; 8],
    pub max_z: [u16; 8],
    /// `0xFFFF` = empty slot; `c & internal_bit != 0` = internal node `c & !internal_bit`;
    /// otherwise leaf macro-region index `c`.
    pub children: [u16; 8],
}

/// Parsed `model/map/ch_nav/*.htr` 8-ary quantized AABB hierarchy tree.
#[derive(Debug, Clone)]
pub struct Htr {
    pub world_to_grid: Mat4,
    pub grid_to_world: Mat4,
    pub internal_bit: u16,
    pub nodes: Vec<HtrNode>,
}

/// Parses a big-endian 360 `HTR` file (`HTR\0` + `"0001"`).
pub fn read_htr(data: &[u8]) -> Result<Htr> {
    ensure!(data.len() >= 0x98, "HTR too short ({} bytes)", data.len());
    ensure!(&data[0..4] == b"HTR\0", "HTR bad magic: {:?}", &data[0..4]);
    ensure!(
        &data[4..8] == b"0001",
        "HTR unsupported version: {:?}",
        &data[4..8]
    );

    let mut r = Cursor::new(data);
    r.seek(0x08)?;
    let _branching = r.u32()?;
    let world_to_grid = Mat4::read(&mut r)?;
    let grid_to_world = Mat4::read(&mut r)?;
    let internal_bit = r.u32()? as u16;
    let _unk = r.u32()?;
    let node_count = r.u32()? as usize;

    let expected = 0x98 + node_count * 0x70;
    ensure!(
        data.len() >= expected,
        "HTR truncated: have {} bytes, need {expected} for {node_count} nodes",
        data.len()
    );

    let mut nodes = Vec::with_capacity(node_count);
    let read_u16x8 = |r: &mut Cursor<'_>| -> Result<[u16; 8]> {
        let mut a = [0u16; 8];
        for v in &mut a {
            *v = r.u16()?;
        }
        Ok(a)
    };
    for _ in 0..node_count {
        let min_x = read_u16x8(&mut r)?;
        let min_y = read_u16x8(&mut r)?;
        let min_z = read_u16x8(&mut r)?;
        let max_x = read_u16x8(&mut r)?;
        let max_y = read_u16x8(&mut r)?;
        let max_z = read_u16x8(&mut r)?;
        let children = read_u16x8(&mut r)?;
        nodes.push(HtrNode {
            min_x,
            min_y,
            min_z,
            max_x,
            max_y,
            max_z,
            children,
        });
    }

    Ok(Htr {
        world_to_grid,
        grid_to_world,
        internal_bit,
        nodes,
    })
}

impl Htr {
    /// Queries the 8-ary AABB tree for all leaf macro-region indices whose quantized XZ box
    /// contains `grid_pos` (within optional `margin` grid cells).
    pub fn query_xz(&self, grid_pos: [f32; 3], margin: f32) -> Vec<usize> {
        let mut out = Vec::new();
        if self.nodes.is_empty() {
            return out;
        }
        let gx = grid_pos[0];
        let gz = grid_pos[2];
        let mut stack = vec![0usize];
        while let Some(ni) = stack.pop() {
            let Some(node) = self.nodes.get(ni) else {
                continue;
            };
            for k in 0..8 {
                let ch = node.children[k];
                if ch == 0xFFFF {
                    continue;
                }
                if gx >= f32::from(node.min_x[k]) - margin
                    && gx <= f32::from(node.max_x[k]) + margin
                    && gz >= f32::from(node.min_z[k]) - margin
                    && gz <= f32::from(node.max_z[k]) + margin
                {
                    if (ch & self.internal_bit) != 0 {
                        stack.push((ch & !self.internal_bit) as usize);
                    } else {
                        out.push(ch as usize);
                    }
                }
            }
        }
        out
    }
}

/// Combined `HNAV` + `HTR` navigation mesh with spatial lookup and A* pathfinding.
#[derive(Debug, Clone)]
pub struct NavMesh {
    pub hnav: Hnav,
    pub htr: Option<Htr>,
    /// Cached world-space `(min, max)` AABB and floor center for each macro region.
    bounds: Vec<([f32; 3], [f32; 3])>,
    centers: Vec<[f32; 3]>,
}

impl NavMesh {
    pub fn new(hnav: Hnav, htr: Option<Htr>) -> Self {
        let (bounds, centers) = if let Some(m) = hnav.macro_nodes.first() {
            let b: Vec<_> = m
                .regions
                .iter()
                .map(|r| r.world_bounds(&hnav.grid_to_world))
                .collect();
            let c: Vec<_> = b
                .iter()
                .map(|(min, max)| [0.5 * (min[0] + max[0]), min[1], 0.5 * (min[2] + max[2])])
                .collect();
            (b, c)
        } else {
            (Vec::new(), Vec::new())
        };
        Self {
            hnav,
            htr,
            bounds,
            centers,
        }
    }

    pub fn region_count(&self) -> usize {
        self.hnav
            .macro_nodes
            .first()
            .map_or(0, |m| m.regions.len())
    }

    pub fn region_bounds(&self, idx: usize) -> Option<([f32; 3], [f32; 3])> {
        self.bounds.get(idx).copied()
    }

    pub fn region_center(&self, idx: usize) -> Option<[f32; 3]> {
        self.centers.get(idx).copied()
    }

    /// Finds the best macro region index containing or nearest to `world_pos` (FLVER axes).
    /// Uses `HTR` when available, then falls back to nearest region AABB if slightly outside.
    pub fn find_region(&self, world_pos: [f32; 3]) -> Option<usize> {
        let macro_node = self.hnav.macro_nodes.first()?;
        if macro_node.regions.is_empty() {
            return None;
        }
        let g = self.hnav.world_to_grid.transform_point(world_pos);
        if let Some(htr) = &self.htr {
            let candidates = htr.query_xz(g, 0.5);
            if !candidates.is_empty() {
                let best = candidates
                    .into_iter()
                    .filter(|&i| i < self.bounds.len())
                    .min_by(|&a, &b| {
                        let da = point_box_dist_sq(world_pos, self.bounds[a]);
                        let db = point_box_dist_sq(world_pos, self.bounds[b]);
                        da.total_cmp(&db)
                    });
                if best.is_some() {
                    return best;
                }
            }
        }
        // Fallback: nearest macro region AABB in 3D (weighted slightly toward Y match).
        (0..self.bounds.len()).min_by(|&a, &b| {
            let da = point_box_dist_sq(world_pos, self.bounds[a]);
            let db = point_box_dist_sq(world_pos, self.bounds[b]);
            da.total_cmp(&db)
        })
    }

    /// Computes a world-space waypoint path (FLVER axes) from `start` to `goal` using A* over
    /// `HNAV` macro regions followed by portal-clamped waypoint smoothing.
    ///
    /// Returns an empty `Vec` if no path exists, or a list of waypoints ending at `goal`
    /// (excluding `start` itself).
    pub fn find_path(&self, start: [f32; 3], goal: [f32; 3]) -> Vec<[f32; 3]> {
        let Some(macro_node) = self.hnav.macro_nodes.first() else {
            return Vec::new();
        };
        let n = macro_node.regions.len();
        if n == 0 {
            return Vec::new();
        }
        let Some(start_idx) = self.find_region(start) else {
            return Vec::new();
        };
        let Some(goal_idx) = self.find_region(goal) else {
            return Vec::new();
        };
        if start_idx == goal_idx {
            return vec![goal];
        }

        // Standard A* over macro region graph.
        let mut g_score = vec![f32::INFINITY; n];
        let mut came_from = vec![u32::MAX; n];
        let mut heap = BinaryHeap::new();
        g_score[start_idx] = 0.0;
        heap.push(AStarNode {
            idx: start_idx,
            f: dist_3d(self.centers[start_idx], self.centers[goal_idx]),
        });

        let mut reached = false;
        while let Some(AStarNode { idx, f }) = heap.pop() {
            if idx == goal_idx {
                reached = true;
                break;
            }
            let cur_g = g_score[idx];
            if f > cur_g + dist_3d(self.centers[idx], self.centers[goal_idx]) + 1e-3 {
                continue;
            }
            for &nb_u16 in macro_node.neighbors_of(idx) {
                let nb = nb_u16 as usize;
                let step = dist_3d(self.centers[idx], self.centers[nb]);
                let tent_g = cur_g + step;
                if tent_g < g_score[nb] {
                    g_score[nb] = tent_g;
                    came_from[nb] = idx as u32;
                    let h = dist_3d(self.centers[nb], self.centers[goal_idx]);
                    heap.push(AStarNode {
                        idx: nb,
                        f: tent_g + h,
                    });
                }
            }
        }

        if !reached {
            return Vec::new();
        }

        // Reconstruct region chain start_idx -> ... -> goal_idx.
        let mut chain = Vec::new();
        let mut cur = goal_idx;
        chain.push(cur);
        while cur != start_idx {
            let prev = came_from[cur];
            if prev == u32::MAX {
                return Vec::new();
            }
            cur = prev as usize;
            chain.push(cur);
        }
        chain.reverse();

        // Build portal waypoints clamped to the shared overlap of consecutive region AABBs,
        // pulling each waypoint toward the line between its neighbors (iterative string-pulling).
        let mut portals: Vec<([f32; 3], [f32; 3])> = Vec::with_capacity(chain.len().saturating_sub(1));
        for w in chain.windows(2) {
            let (a_min, a_max) = self.bounds[w[0]];
            let (b_min, b_max) = self.bounds[w[1]];
            let p_min = [
                a_min[0].max(b_min[0]),
                a_min[1].min(b_min[1]),
                a_min[2].max(b_min[2]),
            ];
            let p_max = [
                a_max[0].min(b_max[0]),
                a_max[1].max(b_max[1]),
                a_max[2].min(b_max[2]),
            ];
            // If two adjacent regions touch with a tiny gap, clamp min<=max at their midpoint.
            let fix = |lo: f32, hi: f32| -> (f32, f32) {
                if lo <= hi {
                    (lo, hi)
                } else {
                    let mid = 0.5 * (lo + hi);
                    (mid, mid)
                }
            };
            let (x0, x1) = fix(p_min[0], p_max[0]);
            let (y0, y1) = fix(p_min[1], p_max[1]);
            let (z0, z1) = fix(p_min[2], p_max[2]);
            // Inset slightly from portal edges so ground units don't scrape walls.
            let inset_x = ((x1 - x0) * 0.15).min(2.0);
            let inset_z = ((z1 - z0) * 0.15).min(2.0);
            portals.push((
                [x0 + inset_x, y0, z0 + inset_z],
                [x1 - inset_x, y1, z1 - inset_z],
            ));
        }

        let mut pts: Vec<[f32; 3]> = portals
            .iter()
            .map(|(lo, hi)| [0.5 * (lo[0] + hi[0]), lo[1], 0.5 * (lo[2] + hi[2])])
            .collect();

        // 4 passes of clamped string-pulling tighten the path around corners inside each portal.
        for _ in 0..4 {
            for i in 0..pts.len() {
                let prev = if i == 0 { start } else { pts[i - 1] };
                let next = if i + 1 < pts.len() { pts[i + 1] } else { goal };
                let mid = [
                    0.5 * (prev[0] + next[0]),
                    0.5 * (prev[1] + next[1]),
                    0.5 * (prev[2] + next[2]),
                ];
                let (lo, hi) = portals[i];
                pts[i] = [
                    mid[0].clamp(lo[0], hi[0]),
                    mid[1].clamp(lo[1], hi[1]),
                    mid[2].clamp(lo[2], hi[2]),
                ];
            }
        }

        // Drop redundant waypoints closer than 1.5 m to the previous point.
        let mut out = Vec::with_capacity(pts.len() + 1);
        let mut last = start;
        for p in pts {
            if dist_xz(last, p) > 1.5 {
                out.push(p);
                last = p;
            }
        }
        out.push(goal);
        out
    }
}

#[derive(Clone, Copy)]
struct AStarNode {
    idx: usize,
    f: f32,
}

impl PartialEq for AStarNode {
    fn eq(&self, other: &Self) -> bool {
        self.idx == other.idx && self.f.to_bits() == other.f.to_bits()
    }
}
impl Eq for AStarNode {}

impl Ord for AStarNode {
    fn cmp(&self, other: &Self) -> Ordering {
        other.f.total_cmp(&self.f).then_with(|| self.idx.cmp(&other.idx))
    }
}

impl PartialOrd for AStarNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[inline]
fn dist_3d(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    let dz = a[2] - b[2];
    (dx * dx + dy * dy + dz * dz).sqrt()
}

#[inline]
fn dist_xz(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dx = a[0] - b[0];
    let dz = a[2] - b[2];
    (dx * dx + dz * dz).sqrt()
}

#[inline]
fn point_box_dist_sq(p: [f32; 3], (min, max): ([f32; 3], [f32; 3])) -> f32 {
    let dx = if p[0] < min[0] {
        min[0] - p[0]
    } else if p[0] > max[0] {
        p[0] - max[0]
    } else {
        0.0
    };
    let dy = if p[1] < min[1] {
        min[1] - p[1]
    } else if p[1] > max[1] {
        p[1] - max[1]
    } else {
        0.0
    };
    let dz = if p[2] < min[2] {
        min[2] - p[2]
    } else if p[2] > max[2] {
        p[2] - max[2]
    } else {
        0.0
    };
    // Weight vertical mismatch slightly higher so multi-story overlaps pick the floor we're on.
    dx * dx + (dy * dy) * 4.0 + dz * dz
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs;

    #[test]
    fn disc_all_hnav_and_htr_parse() {
        let Ok(disc) = vfs::Disc::open(&vfs::default_disc(&vfs::repo_root())) else {
            return;
        };
        let files = disc.files();
        let hnavs: Vec<_> = files
            .iter()
            .filter(|f| f.starts_with("model/map/ch_nav/") && f.ends_with(".hnav"))
            .collect();
        let htrs: Vec<_> = files
            .iter()
            .filter(|f| f.starts_with("model/map/ch_nav/") && f.ends_with(".htr"))
            .collect();
        assert_eq!(hnavs.len(), 59, "expected 59 .hnav files on 360 ISO");
        assert_eq!(htrs.len(), 59, "expected 59 .htr files on 360 ISO");

        for path in &hnavs {
            let bytes = disc.read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
            let hnav = read_hnav(&bytes).unwrap_or_else(|e| panic!("parse {path}: {e}"));
            assert!(
                !hnav.macro_nodes.is_empty(),
                "{path} has no macro node"
            );
            assert_eq!(
                hnav.macro_nodes[0].regions.len(),
                hnav.micro_nodes.len(),
                "{path} macro region count != micro node count"
            );
        }

        for path in &htrs {
            let bytes = disc.read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
            let htr = read_htr(&bytes).unwrap_or_else(|e| panic!("parse {path}: {e}"));
            assert!(!htr.nodes.is_empty(), "{path} has 0 HTR nodes");
            // Verify every internal node index is in bounds.
            for (ni, node) in htr.nodes.iter().enumerate() {
                for &ch in &node.children {
                    if ch != 0xFFFF && (ch & htr.internal_bit) != 0 {
                        let target = (ch & !htr.internal_bit) as usize;
                        assert!(
                            target < htr.nodes.len(),
                            "{path} node {ni} internal child {target} >= {}",
                            htr.nodes.len()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn m4000_navmesh_pathfinding() {
        let Ok(disc) = vfs::Disc::open(&vfs::default_disc(&vfs::repo_root())) else {
            return;
        };
        let hnav = read_hnav(&disc.read("model/map/ch_nav/m4000.hnav").unwrap()).unwrap();
        let htr = read_htr(&disc.read("model/map/ch_nav/m4000.htr").unwrap()).unwrap();
        let nav = NavMesh::new(hnav, Some(htr));
        assert_eq!(nav.region_count(), 607);

        // AC TEST start point in FLVER axes (-324.65, 29.0, 649.5) to an arena point across the map.
        let start = [-324.65, 29.0, 649.5];
        let goal = [-620.0, 29.0, 520.0];
        let r_start = nav.find_region(start).expect("start region");
        let r_goal = nav.find_region(goal).expect("goal region");
        assert_ne!(r_start, r_goal, "start and goal should be in distinct regions");

        let path = nav.find_path(start, goal);
        assert!(
            path.len() >= 2,
            "expected multi-waypoint path across m4000, got {} points",
            path.len()
        );
        assert_eq!(*path.last().unwrap(), goal);
        // Every consecutive step should be finite and reasonable.
        for w in path.windows(2) {
            let d = dist_3d(w[0], w[1]);
            assert!(d.is_finite() && d < 300.0, "unreasonable segment length {d}");
        }
    }
}
