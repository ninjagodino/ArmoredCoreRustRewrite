//! Hit model (`.hmd`) inside map/part binders (`*_h.hmd`). Big-endian, magic `0x10000117`.
//!
//! Header (0x30): `u32 magic, u32 0x4A9, u32 file size, u32 material count, u32 1, u32 vertex
//! count, u32 0, u32 node count, u32 triangle count, u32 0x40 (string-offset table), u32 first
//! byte after the material strings, u32 triangle offset`. The table at 0x40 holds two offsets
//! per material (name, empty companion). Nodes of 0x34 bytes start at `triangle_offset -
//! node_count * 0x34`; a 0x30-byte mesh record just before them holds the vertex offset.
//! Each node is `i16 triangle count, i16 first triangle, i16 left, i16 right, f32x3 AABB min,
//! f32x3 AABB max, f32x3 centre, f32x2 radii`. A triangle is five `u16`: three vertex fields
//! (`index = value >> 1`, low bit kept), an unknown field, and a material index. Vertices are
//! `f32x3`. Triangles are padded to 4 bytes before the vertices.

use anyhow::{bail, ensure, Result};
use serde::Serialize;

use crate::reader::Be;

pub const MAGIC: u32 = 0x1000_0117;
pub const VERSION: u32 = 0x4A9;
pub const HEADER: usize = 0x30;
pub const NODE: usize = 0x34;
pub const TRIANGLE: usize = 10;
pub const VERTEX: usize = 12;
pub const MESH: usize = 0x30;

pub type Vec3 = [f32; 3];

#[derive(Debug, Clone, Serialize)]
pub struct Hmd {
    pub materials: Vec<String>,
    pub nodes: Vec<Node>,
    pub triangles: Vec<Triangle>,
    pub vertices: Vec<Vec3>,
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
    pub verts: [u16; 3],
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
    ensure!(r.u32(4)? == VERSION, "HMD version {:#x}, expected {VERSION:#x}", r.u32(4)?);
    let size = r.u32(8)? as usize;
    ensure!(size == data.len(), "HMD size {size:#x} != buffer {:#x}", data.len());
    let nmat = r.u32(0x0C)? as usize;
    ensure!(r.u32(0x10)? == 1, "HMD +0x10 is {:#x}, expected 1", r.u32(0x10)?);
    let nv = r.u32(0x14)? as usize;
    ensure!(r.u32(0x18)? == 0, "HMD +0x18 is {:#x}, expected 0", r.u32(0x18)?);
    let nn = r.u32(0x1C)? as usize;
    let nt = r.u32(0x20)? as usize;
    ensure!(r.u32(0x24)? == 0x40, "HMD string table at {:#x}, expected 0x40", r.u32(0x24)?);
    let tri_off = r.u32(0x2C)? as usize;
    let node_off = tri_off.checked_sub(nn * NODE).ok_or_else(|| anyhow::anyhow!("HMD triangle offset {tri_off:#x} is before {nn} nodes"))?;
    let mesh = node_off.checked_sub(MESH).ok_or_else(|| anyhow::anyhow!("HMD node offset {node_off:#x} has no mesh record"))?;
    ensure!(r.u32(mesh + 0x24)? as usize == node_off, "HMD mesh record node offset does not match");
    let vert_off = r.u32(mesh + 0x28)? as usize;
    ensure!(vert_off == (tri_off + nt * TRIANGLE).next_multiple_of(4), "HMD vertex offset {vert_off:#x} is not after {nt} triangles");
    ensure!(vert_off + nv * VERTEX == size, "HMD vertices do not end at the file size");

    let mut materials = Vec::with_capacity(nmat);
    for i in 0..nmat {
        materials.push(r.cstr_sjis(r.u32(0x40 + 8 * i)? as usize)?);
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

    let mut triangles = Vec::with_capacity(nt);
    for i in 0..nt {
        let o = tri_off + i * TRIANGLE;
        let raw = [r.u16(o)?, r.u16(o + 2)?, r.u16(o + 4)?];
        let verts = raw.map(|v| v >> 1);
        if let Some(&v) = verts.iter().find(|&&v| v as usize >= nv) {
            bail!("HMD triangle {i} vertex {v} past {nv} vertices");
        }
        triangles.push(Triangle {
            verts,
            flags: raw.map(|v| (v & 1) as u8),
            unk: r.u16(o + 6)?,
            material: r.u16(o + 8)?,
        });
    }

    let mut vertices = Vec::with_capacity(nv);
    for i in 0..nv {
        let o = vert_off + i * VERTEX;
        vertices.push([r.f32(o)?, r.f32(o + 4)?, r.f32(o + 8)?]);
    }

    Ok(Hmd { materials, nodes, triangles, vertices })
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
        for (i, v) in [0.0f32, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 2.0].into_iter().enumerate() {
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
    }
}
