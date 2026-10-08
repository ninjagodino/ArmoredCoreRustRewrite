//! Hit model (`.hmd`) inside map/part binders (`*_h.hmd`). Big-endian, magic `0x10000117`.
//!
//! Header (0x30): `u32 magic, u32 0x4A9, u32 file size, u32 material count, u32 mesh count,
//! u32 vertex count, u32 0, u32 node count, u32 triangle count, u32 0x40 (string-offset table),
//! u32 first byte after the material strings, u32 triangle offset`. The table at 0x40 holds two
//! offsets per material (name, empty companion).
//!
//! Mesh records (0x3c each) follow the strings, 4-aligned: `f32x3 translation, f32x3 rotation,
//! f32x3 scale, i16 index, i16, i16 parent, i16 first child, i16 next sibling, i16, u32 node
//! offset, u32 vertex offset, u32 0`. A record with node offset 0 carries no geometry (the root
//! of multi-mesh object hit models, all zero). Each mesh owns the nodes from its node offset to
//! the next mesh's (the last up to the triangles) and the vertices from its vertex offset to
//! the next mesh's (the last up to the file end); its triangles are one contiguous run of the
//! file's triangle array and index its own vertices.
//!
//! Each node is `i16 triangle count, i16 first triangle, i16 left, i16 right, f32x3 AABB min,
//! f32x3 AABB max, f32x3 centre, f32x2 radii`. A triangle is five `u16`: three vertex fields
//! (`index = value >> 1`, low bit kept), an unknown field, and a material index. Vertices are
//! `f32x3`. Triangles are padded to 4 bytes before the vertices.

use anyhow::{bail, ensure, Context, Result};
use serde::Serialize;

use crate::flver::Xform;
use crate::reader::Be;

pub const MAGIC: u32 = 0x1000_0117;
pub const VERSION: u32 = 0x4A9;
pub const HEADER: usize = 0x30;
pub const NODE: usize = 0x34;
pub const TRIANGLE: usize = 10;
pub const VERTEX: usize = 12;
pub const MESH: usize = 0x3C;

pub type Vec3 = [f32; 3];

#[derive(Debug, Clone, Serialize)]
pub struct Hmd {
    pub materials: Vec<String>,
    pub meshes: Vec<Mesh>,
    pub nodes: Vec<Node>,
    /// Vertex indices are into [`Hmd::vertices`] (each mesh's base already added).
    pub triangles: Vec<Triangle>,
    /// Mesh-local positions; [`Hmd::model_vertices`] applies the mesh hierarchy.
    pub vertices: Vec<Vec3>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Mesh {
    pub translation: Vec3,
    /// Read as radians, like FLVER bones; every rotation seen is 0 (to float noise).
    pub rotation: Vec3,
    pub scale: Vec3,
    pub parent: Option<usize>,
    pub first_vertex: usize,
    pub vertex_count: usize,
}

impl Hmd {
    /// Every vertex through its mesh's local transform and its parents'. All-zero records (the
    /// geometry-less root) count as identity.
    pub fn model_vertices(&self) -> Vec<Vec3> {
        let local = |m: &Mesh| {
            if m.scale == [0.0; 3] {
                Xform::IDENTITY
            } else {
                Xform::local(m.translation, m.rotation, m.scale)
            }
        };
        let world: Vec<Xform> = (0..self.meshes.len())
            .map(|i| {
                let mut xf = local(&self.meshes[i]);
                let mut at = self.meshes[i].parent;
                for _ in 0..self.meshes.len() {
                    let Some(p) = at else { break };
                    xf = local(&self.meshes[p]).then(&xf);
                    at = self.meshes[p].parent;
                }
                xf
            })
            .collect();
        let mut out = self.vertices.clone();
        for (m, xf) in self.meshes.iter().zip(&world) {
            for v in &mut out[m.first_vertex..m.first_vertex + m.vertex_count] {
                *v = xf.apply(*v);
            }
        }
        out
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Node {
    pub count: i16,
    pub first: i16,
    pub left: i16,
    pub right: i16,
    pub aabb_min: Vec3,
    pub aabb_max: Vec3,
    pub center: Vec3,
    pub radii: [f32; 2],
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Triangle {
    pub verts: [u32; 3],
    pub flags: [u8; 3],
    pub unk: u16,
    pub material: u16,
}

pub fn is_hmd(data: &[u8]) -> bool {
    data.len() >= 4 && u32::from_be_bytes(data[..4].try_into().unwrap()) == MAGIC
}

pub fn read(data: &[u8]) -> Result<Hmd> {
    let r = Be(data);
    ensure!(r.u32(0)? == MAGIC, "not an HMD");
    ensure!(
        r.u32(4)? == VERSION,
        "HMD version {:#x}, expected {VERSION:#x}",
        r.u32(4)?
    );
    let size = r.u32(8)? as usize;
    ensure!(
        size == data.len(),
        "HMD size {size:#x} != buffer {:#x}",
        data.len()
    );
    let nmat = r.u32(0x0C)? as usize;
    let nmesh = r.u32(0x10)? as usize;
    ensure!(nmesh <= 256, "HMD mesh count {nmesh}");
    let nv = r.u32(0x14)? as usize;
    ensure!(
        r.u32(0x18)? == 0,
        "HMD +0x18 is {:#x}, expected 0",
        r.u32(0x18)?
    );
    let nn = r.u32(0x1C)? as usize;
    let nt = r.u32(0x20)? as usize;
    ensure!(
        r.u32(0x24)? == 0x40,
        "HMD string table at {:#x}, expected 0x40",
        r.u32(0x24)?
    );
    let records = (r.u32(0x28)? as usize).next_multiple_of(4);
    let tri_off = r.u32(0x2C)? as usize;
    let node_off = tri_off
        .checked_sub(nn * NODE)
        .context("HMD triangle offset is before its nodes")?;
    let first_vert = (tri_off + nt * TRIANGLE).next_multiple_of(4);
    ensure!(
        first_vert + nv * VERTEX == size,
        "HMD vertices do not end at the file size"
    );
    ensure!(
        records + nmesh * MESH <= node_off,
        "HMD mesh records run into the nodes"
    );

    let mut materials = Vec::with_capacity(nmat);
    for i in 0..nmat {
        materials.push(r.cstr_sjis(r.u32(0x40 + 8 * i)? as usize)?);
    }

    // (record, node offset, vertex offset) of every mesh with geometry, in file order.
    let mut meshes = Vec::with_capacity(nmesh);
    let mut spans = Vec::new();
    for k in 0..nmesh {
        let o = records + k * MESH;
        let v3 = |at: usize| -> Result<Vec3> { Ok([r.f32(at)?, r.f32(at + 4)?, r.f32(at + 8)?]) };
        let parent = r.i16(o + 0x28)?;
        meshes.push(Mesh {
            translation: v3(o)?,
            rotation: v3(o + 0x0C)?,
            scale: v3(o + 0x18)?,
            parent: usize::try_from(parent)
                .ok()
                .filter(|&p| p < nmesh && p != k),
            first_vertex: 0,
            vertex_count: 0,
        });
        let (nodes_at, verts_at) = (r.u32(o + 0x30)? as usize, r.u32(o + 0x34)? as usize);
        if nodes_at != 0 {
            spans.push((k, nodes_at, verts_at));
        }
    }
    ensure!(
        spans
            .first()
            .is_none_or(|s| s.1 == node_off && s.2 == first_vert),
        "HMD first mesh does not start at the first node / vertex"
    );
    ensure!(
        spans
            .windows(2)
            .all(|w| w[0].1 < w[1].1 && w[0].2 <= w[1].2),
        "HMD mesh spans out of order"
    );
    let mut geometry = Vec::with_capacity(spans.len());
    for (i, &(k, nodes_at, verts_at)) in spans.iter().enumerate() {
        let (nodes_end, verts_end) = spans.get(i + 1).map_or((tri_off, size), |s| (s.1, s.2));
        ensure!(
            (nodes_end - nodes_at) % NODE == 0 && (verts_end - verts_at) % VERTEX == 0,
            "HMD mesh {k} span is not whole nodes / vertices"
        );
        let first = (verts_at - first_vert) / VERTEX;
        let count = (verts_end - verts_at) / VERTEX;
        (meshes[k].first_vertex, meshes[k].vertex_count) = (first, count);
        geometry.push((
            (nodes_at - node_off) / NODE,
            (nodes_end - node_off) / NODE,
            first,
            count,
        ));
    }

    let mut nodes = Vec::with_capacity(nn);
    for i in 0..nn {
        let o = node_off + i * NODE;
        nodes.push(Node {
            count: r.i16(o)?,
            first: r.i16(o + 2)?,
            left: r.i16(o + 4)?,
            right: r.i16(o + 6)?,
            aabb_min: [r.f32(o + 8)?, r.f32(o + 12)?, r.f32(o + 16)?],
            aabb_max: [r.f32(o + 20)?, r.f32(o + 24)?, r.f32(o + 28)?],
            center: [r.f32(o + 32)?, r.f32(o + 36)?, r.f32(o + 40)?],
            radii: [r.f32(o + 44)?, r.f32(o + 48)?],
        });
    }

    // Per triangle: (first vertex, vertex count) of the mesh whose leaf nodes own it.
    let mut owner: Vec<Option<(usize, usize)>> = vec![None; nt];
    for &(n0, n1, first, count) in &geometry {
        for n in &nodes[n0..n1] {
            let (start, len) = (
                usize::try_from(n.first).unwrap_or(0),
                usize::try_from(n.count).unwrap_or(0),
            );
            ensure!(
                start + len <= nt,
                "HMD node triangles {start}+{len} past {nt}"
            );
            for o in &mut owner[start..start + len] {
                *o = Some((first, count));
            }
        }
    }

    let mut triangles = Vec::with_capacity(nt);
    for (i, own) in owner.iter().enumerate() {
        let o = tri_off + i * TRIANGLE;
        let raw = [r.u16(o)?, r.u16(o + 2)?, r.u16(o + 4)?];
        let (base, count) = own.unwrap_or((0, nv));
        let local = raw.map(|v| usize::from(v >> 1));
        if let Some(&v) = local.iter().find(|&&v| v >= count) {
            bail!("HMD triangle {i} vertex {v} past its mesh's {count} vertices");
        }
        triangles.push(Triangle {
            verts: local.map(|v| (base + v) as u32),
            flags: raw.map(|v| (v & 1) as u8),
            unk: r.u16(o + 6)?,
            material: r.u16(o + 8)?,
        });
    }

    let mut vertices = Vec::with_capacity(nv);
    for i in 0..nv {
        let o = first_vert + i * VERTEX;
        vertices.push([r.f32(o)?, r.f32(o + 4)?, r.f32(o + 8)?]);
    }

    Ok(Hmd {
        materials,
        meshes,
        nodes,
        triangles,
        vertices,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_u32(d: &mut [u8], at: usize, v: u32) {
        d[at..at + 4].copy_from_slice(&v.to_be_bytes());
    }
    fn put_u16(d: &mut [u8], at: usize, v: u16) {
        d[at..at + 2].copy_from_slice(&v.to_be_bytes());
    }
    fn put_i16(d: &mut [u8], at: usize, v: i16) {
        d[at..at + 2].copy_from_slice(&v.to_be_bytes());
    }
    fn put_f32(d: &mut [u8], at: usize, v: f32) {
        d[at..at + 4].copy_from_slice(&v.to_be_bytes());
    }

    /// One sand triangle (0,0,0) (2,0,0) (0,0,2) under a single leaf node.
    fn sample() -> Vec<u8> {
        const TRI_OFF: usize = 0xC4;
        const VERT_OFF: usize = 0xD0;
        const SIZE: usize = VERT_OFF + 36;
        let mut d = vec![0u8; SIZE];
        put_u32(&mut d, 0, MAGIC);
        put_u32(&mut d, 4, VERSION);
        put_u32(&mut d, 8, SIZE as u32);
        put_u32(&mut d, 0x0C, 1);
        put_u32(&mut d, 0x10, 1);
        put_u32(&mut d, 0x14, 3);
        put_u32(&mut d, 0x1C, 1);
        put_u32(&mut d, 0x20, 1);
        put_u32(&mut d, 0x24, 0x40);
        put_u32(&mut d, 0x28, 0x52);
        put_u32(&mut d, 0x2C, TRI_OFF as u32);
        put_u32(&mut d, 0x40, 0x48);
        put_u32(&mut d, 0x44, 0x51);
        d[0x48..0x51].copy_from_slice(b"sand.tga\0");
        put_u32(&mut d, 0x60 + 0x24, 0x90);
        put_u32(&mut d, 0x60 + 0x28, VERT_OFF as u32);
        put_i16(&mut d, 0x90, 1);
        put_i16(&mut d, 0x92, 0);
        put_i16(&mut d, 0x94, -1);
        put_i16(&mut d, 0x96, -1);
        for (i, v) in [0.0, 0.0, 0.0, 2.0, 0.0, 2.0].into_iter().enumerate() {
            put_f32(&mut d, 0x98 + 4 * i, v);
        }
        put_u16(&mut d, TRI_OFF, 0);
        put_u16(&mut d, TRI_OFF + 2, 2);
        put_u16(&mut d, TRI_OFF + 4, 4);
        for (i, v) in [0.0f32, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 2.0]
            .into_iter()
            .enumerate()
        {
            put_f32(&mut d, VERT_OFF + 4 * i, v);
        }
        d
    }

    #[test]
    fn reads_one_triangle() {
        let h = read(&sample()).unwrap();
        assert_eq!(h.materials, ["sand.tga"]);
        assert_eq!(h.vertices.len(), 3);
        assert_eq!(h.triangles[0].verts, [0, 1, 2]);
        assert_eq!(h.nodes[0].count, 1);
        assert_eq!(h.vertices[1], [2.0, 0.0, 0.0]);
        assert_eq!(h.model_vertices(), h.vertices);
    }

    #[test]
    fn multi_mesh_object_from_disc() {
        let Some(disc) = crate::vfs::test_disc() else {
            return;
        };
        let h =
            read(&crate::vfs::open(&disc, "model/obj/o0006/o0006_m.bnd.dcx|o0006_h.hmd").unwrap())
                .unwrap();
        assert_eq!(h.meshes.len(), 5);
        assert_eq!(
            h.meshes.iter().map(|m| m.vertex_count).collect::<Vec<_>>(),
            [0, 16, 40, 32, 32]
        );
        assert_eq!(h.meshes[3].parent, Some(0));
        assert_eq!(h.triangles.len(), 148);
        let (m3, world) = (&h.meshes[3], h.model_vertices());
        let (local, placed) = (h.vertices[m3.first_vertex], world[m3.first_vertex]);
        assert_eq!(
            [placed[0] - local[0], placed[2] - local[2]],
            [22.0, -21.75592]
        );
    }
}
