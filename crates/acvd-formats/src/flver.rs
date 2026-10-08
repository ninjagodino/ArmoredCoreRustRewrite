//! FLVER2 model, big-endian (`"FLVER\0B\0"`), as on the 360 disc.
//!
//! Header (0x80 bytes): `"FLVER\0", "B\0", u32 version, u32 data_offset, u32 data_length,
//! i32 dummy/material/bone/mesh/vertex_buffer counts, f32x3 bbox_min, f32x3 bbox_max,
//! i32 true_faces, i32 total_faces, u8 index_size (16, 32), u8 unicode, u8 unk4a,
//! u8 unk4b, i32 unk4c, i32 face_set/layout/texture counts, u8 unk5c, u8 unk5d, u16 0, u32 0,
//! u32 0, i32 unk68, 5 x u32 0`.
//! Tables follow back to back: dummies 0x40, materials 0x20, bones 0x80, meshes 0x30,
//! face sets 0x20, vertex buffers 0x20, layouts 0x10, textures 0x20. Index and vertex data
//! offsets are relative to `data_offset`.
//!
//! Models store big-endian 16-bit strips (0xFFFF restarts; `unk4c` = 0xFFFF, `unk4a` and
//! `unk4b` = 1) over one interleaved buffer per mesh, with normals and tangents as signed
//! bytes ([`MemberKind::NormalS8`], [`MemberKind::TangentS8`]).

use anyhow::{bail, ensure, Context, Result};
use serde::Serialize;

use crate::reader::{utf16be, Be};

pub const MAGIC: &[u8; 8] = b"FLVER\0B\0";
pub const HEADER: usize = 0x80;

pub type Vec3 = [f32; 3];

/// Affine transform for column vectors: `p' = m * p + t`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Xform {
    pub m: [Vec3; 3],
    pub t: Vec3,
}

impl Xform {
    pub const IDENTITY: Self = Self {
        m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        t: [0.0; 3],
    };

    /// `translate * rot_y * rot_z * rot_x * scale`.
    pub fn local(translation: Vec3, rotation: Vec3, scale: Vec3) -> Self {
        let (sx, cx) = rotation[0].sin_cos();
        let (sy, cy) = rotation[1].sin_cos();
        let (sz, cz) = rotation[2].sin_cos();
        let rx = Self {
            m: [[1.0, 0.0, 0.0], [0.0, cx, -sx], [0.0, sx, cx]],
            t: [0.0; 3],
        };
        let ry = Self {
            m: [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]],
            t: [0.0; 3],
        };
        let rz = Self {
            m: [[cz, -sz, 0.0], [sz, cz, 0.0], [0.0, 0.0, 1.0]],
            t: [0.0; 3],
        };
        let s = Self {
            m: [
                [scale[0], 0.0, 0.0],
                [0.0, scale[1], 0.0],
                [0.0, 0.0, scale[2]],
            ],
            t: [0.0; 3],
        };
        let mut out = ry.then(&rz).then(&rx).then(&s);
        out.t = translation;
        out
    }

    /// `self * inner`: applies `inner` first.
    pub fn then(&self, inner: &Self) -> Self {
        let mut m = [[0.0; 3]; 3];
        for (r, row) in m.iter_mut().enumerate() {
            for (c, v) in row.iter_mut().enumerate() {
                *v = (0..3).map(|k| self.m[r][k] * inner.m[k][c]).sum();
            }
        }
        Self {
            m,
            t: self.apply(inner.t),
        }
    }

    pub fn apply(&self, p: Vec3) -> Vec3 {
        let m = &self.m;
        [0, 1, 2].map(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + self.t[r])
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Flver {
    pub version: u32,
    pub data_offset: u32,
    pub data_length: u32,
    pub bbox_min: Vec3,
    pub bbox_max: Vec3,
    pub true_faces: i32,
    pub total_faces: i32,
    pub index_size: u8,
    pub unicode: bool,
    pub unk4a: u8,
    pub unk4b: u8,
    pub unk4c: i32,
    pub unk5c: u8,
    pub unk5d: u8,
    pub unk68: i32,
    pub dummies: Vec<Dummy>,
    pub materials: Vec<Material>,
    pub bones: Vec<Bone>,
    pub meshes: Vec<Mesh>,
    pub face_sets: Vec<FaceSet>,
    pub vertex_buffers: Vec<VertexBuffer>,
    pub layouts: Vec<Layout>,
    pub textures: Vec<Texture>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Dummy {
    /// Relative to `parent_bone`.
    pub position: Vec3,
    /// On AC parts, byte 0 is the attach socket id (`sheets/assembly_slots.csv`) and byte 1 an
    /// effect point id.
    pub color: [u8; 4],
    pub forward: Vec3,
    pub reference_id: i16,
    pub parent_bone: i16,
    pub upward: Vec3,
    pub attach_bone: i16,
    pub flag1: u8,
    pub use_upward: u8,
    pub unk30: i32,
    pub unk34: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Material {
    pub name: String,
    pub mtd: String,
    pub texture_count: i32,
    pub texture_index: i32,
    pub flags: i32,
    pub gx_offset: i32,
    pub unk18: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Bone {
    pub name: String,
    pub translation: Vec3,
    pub rotation: Vec3,
    pub scale: Vec3,
    pub parent: i16,
    pub child: i16,
    pub next_sibling: i16,
    pub previous_sibling: i16,
    pub bbox_min: Vec3,
    pub unk3c: i32,
    pub bbox_max: Vec3,
}

#[derive(Debug, Clone, Serialize)]
pub struct Mesh {
    pub dynamic: u8,
    pub flags: [u8; 3],
    pub material: i32,
    pub unk08: i32,
    pub default_bone: i32,
    pub bone_indices: Vec<i32>,
    pub bbox: Option<(Vec3, Vec3)>,
    pub face_sets: Vec<usize>,
    pub vertex_buffers: Vec<usize>,
}

pub const FS_LOD1: u32 = 0x0100_0000;
pub const FS_LOD2: u32 = 0x0200_0000;
pub const FS_MOTION_BLUR: u32 = 0x8000_0000;

#[derive(Debug, Clone, Serialize)]
pub struct FaceSet {
    pub flags: u32,
    pub strip: u8,
    pub cull_backfaces: u8,
    pub unk06: i16,
    pub index_count: i32,
    pub index_offset: u32,
    pub index_length: u32,
    pub index_size: i32,
    pub unk1c: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct VertexBuffer {
    pub buffer_index: i32,
    pub layout: usize,
    pub vertex_size: i32,
    pub vertex_count: i32,
    pub unk10: i32,
    pub unk14: i32,
    pub length: u32,
    pub offset: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Layout {
    pub members: Vec<Member>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Member {
    pub unk00: i32,
    pub offset: u32,
    pub kind: u32,
    pub semantic: u32,
    pub index: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Texture {
    pub path: String,
    pub kind: String,
    pub scale: [f32; 2],
    pub unk10: u8,
    pub unk11: u8,
    pub unk14: f32,
    pub unk18: f32,
    pub unk1c: f32,
}

pub fn is_flver(data: &[u8]) -> bool {
    data.starts_with(b"FLVER\0")
}

fn vec3(r: Be, at: usize) -> Result<Vec3> {
    Ok([r.f32(at)?, r.f32(at + 4)?, r.f32(at + 8)?])
}

fn count(r: Be, at: usize, what: &str) -> Result<usize> {
    let n = r.i32(at)?;
    ensure!(n >= 0, "negative {what} count {n}");
    Ok(n as usize)
}

fn offset(r: Be, at: usize, what: &str) -> Result<usize> {
    let n = r.i32(at)?;
    ensure!(n >= 0, "negative {what} offset {n}");
    Ok(n as usize)
}

fn zero(r: Be, at: usize, len: usize, what: &str) -> Result<()> {
    ensure!(
        r.bytes(at, len)?.iter().all(|&b| b == 0),
        "{what} at {at:#x} is nonzero"
    );
    Ok(())
}

fn indices(r: Be, at: usize, n: usize, what: &str) -> Result<Vec<i32>> {
    (0..n)
        .map(|i| r.i32(at + 4 * i))
        .collect::<Result<_>>()
        .with_context(|| format!("{what} list"))
}

pub fn read(data: &[u8]) -> Result<Flver> {
    let r = Be(data);
    ensure!(r.bytes(0, 8)? == MAGIC, "not a big-endian FLVER");
    let version = r.u32(8)?;
    ensure!(
        (0x20000..0x30000).contains(&version),
        "FLVER version {version:#x} is not FLVER2"
    );
    let n_dummies = count(r, 0x14, "dummy")?;
    let n_materials = count(r, 0x18, "material")?;
    let n_bones = count(r, 0x1C, "bone")?;
    let n_meshes = count(r, 0x20, "mesh")?;
    let n_buffers = count(r, 0x24, "vertex buffer")?;
    let index_size = r.u8(0x48)?;
    ensure!(
        matches!(index_size, 0 | 16 | 32),
        "header index size {index_size}"
    );
    let unicode = match r.u8(0x49)? {
        0 => false,
        1 => true,
        u => bail!("unicode flag {u}"),
    };
    let n_face_sets = count(r, 0x50, "face set")?;
    let n_layouts = count(r, 0x54, "layout")?;
    let n_textures = count(r, 0x58, "texture")?;
    zero(r, 0x5E, 10, "header 0x5E")?;
    zero(r, 0x6C, 0x14, "header 0x6C")?;

    let text = |at: usize| -> Result<String> {
        if unicode {
            let tail = r.bytes(at, data.len().saturating_sub(at))?;
            Ok(utf16be(tail))
        } else {
            r.cstr_sjis(at)
        }
    };

    let mut at = HEADER;
    let mut dummies = Vec::with_capacity(n_dummies);
    for _ in 0..n_dummies {
        zero(r, at + 0x38, 8, "dummy 0x38")?;
        dummies.push(Dummy {
            position: vec3(r, at)?,
            color: r.bytes(at + 0x0C, 4)?.try_into()?,
            forward: vec3(r, at + 0x10)?,
            reference_id: r.i16(at + 0x1C)?,
            parent_bone: r.i16(at + 0x1E)?,
            upward: vec3(r, at + 0x20)?,
            attach_bone: r.i16(at + 0x2C)?,
            flag1: r.u8(at + 0x2E)?,
            use_upward: r.u8(at + 0x2F)?,
            unk30: r.i32(at + 0x30)?,
            unk34: r.i32(at + 0x34)?,
        });
        at += 0x40;
    }

    let mut materials = Vec::with_capacity(n_materials);
    for _ in 0..n_materials {
        zero(r, at + 0x1C, 4, "material 0x1C")?;
        materials.push(Material {
            name: text(offset(r, at, "material name")?)?,
            mtd: text(offset(r, at + 4, "material mtd")?)?,
            texture_count: r.i32(at + 8)?,
            texture_index: r.i32(at + 0xC)?,
            flags: r.i32(at + 0x10)?,
            gx_offset: r.i32(at + 0x14)?,
            unk18: r.i32(at + 0x18)?,
        });
        at += 0x20;
    }

    let mut bones = Vec::with_capacity(n_bones);
    for _ in 0..n_bones {
        zero(r, at + 0x4C, 0x34, "bone 0x4C")?;
        bones.push(Bone {
            translation: vec3(r, at)?,
            name: text(offset(r, at + 0x0C, "bone name")?)?,
            rotation: vec3(r, at + 0x10)?,
            parent: r.i16(at + 0x1C)?,
            child: r.i16(at + 0x1E)?,
            scale: vec3(r, at + 0x20)?,
            next_sibling: r.i16(at + 0x2C)?,
            previous_sibling: r.i16(at + 0x2E)?,
            bbox_min: vec3(r, at + 0x30)?,
            unk3c: r.i32(at + 0x3C)?,
            bbox_max: vec3(r, at + 0x40)?,
        });
        at += 0x80;
    }

    let mut meshes = Vec::with_capacity(n_meshes);
    for m in 0..n_meshes {
        let bbox_at = offset(r, at + 0x18, "mesh bbox")?;
        let bone_n = count(r, at + 0x14, "mesh bone")?;
        let fs_n = count(r, at + 0x20, "mesh face set")?;
        let vb_n = count(r, at + 0x28, "mesh vertex buffer")?;
        ensure!(r.i32(at + 0x0C)? == 0, "mesh {m} 0x0C is nonzero");
        let to_usize = |v: Vec<i32>, what: &str| -> Result<Vec<usize>> {
            v.into_iter()
                .map(|i| usize::try_from(i).with_context(|| format!("mesh {m} {what} index {i}")))
                .collect()
        };
        meshes.push(Mesh {
            dynamic: r.u8(at)?,
            flags: r.bytes(at + 1, 3)?.try_into()?,
            material: r.i32(at + 4)?,
            unk08: r.i32(at + 8)?,
            default_bone: r.i32(at + 0x10)?,
            bone_indices: indices(r, offset(r, at + 0x1C, "mesh bones")?, bone_n, "bone")?,
            bbox: if bbox_at == 0 {
                None
            } else {
                Some((vec3(r, bbox_at)?, vec3(r, bbox_at + 12)?))
            },
            face_sets: to_usize(
                indices(r, offset(r, at + 0x24, "mesh face sets")?, fs_n, "face set")?,
                "face set",
            )?,
            vertex_buffers: to_usize(
                indices(
                    r,
                    offset(r, at + 0x2C, "mesh vertex buffers")?,
                    vb_n,
                    "vertex buffer",
                )?,
                "vertex buffer",
            )?,
        });
        at += 0x30;
    }

    let mut face_sets = Vec::with_capacity(n_face_sets);
    for i in 0..n_face_sets {
        ensure!(r.i32(at + 0x14)? == 0, "face set {i} 0x14 is nonzero");
        face_sets.push(FaceSet {
            flags: r.u32(at)?,
            strip: r.u8(at + 4)?,
            cull_backfaces: r.u8(at + 5)?,
            unk06: r.i16(at + 6)?,
            index_count: r.i32(at + 8)?,
            index_offset: r.u32(at + 0xC)?,
            index_length: r.u32(at + 0x10)?,
            index_size: r.i32(at + 0x18)?,
            unk1c: r.i32(at + 0x1C)?,
        });
        at += 0x20;
    }

    let mut vertex_buffers = Vec::with_capacity(n_buffers);
    for i in 0..n_buffers {
        let layout = r.i32(at + 4)?;
        ensure!(
            (0..n_layouts as i32).contains(&layout),
            "vertex buffer {i} layout {layout} of {n_layouts}"
        );
        vertex_buffers.push(VertexBuffer {
            buffer_index: r.i32(at)?,
            layout: layout as usize,
            vertex_size: r.i32(at + 8)?,
            vertex_count: r.i32(at + 0xC)?,
            unk10: r.i32(at + 0x10)?,
            unk14: r.i32(at + 0x14)?,
            length: r.u32(at + 0x18)?,
            offset: r.u32(at + 0x1C)?,
        });
        at += 0x20;
    }

    let mut layouts = Vec::with_capacity(n_layouts);
    for i in 0..n_layouts {
        let n = count(r, at, "layout member")?;
        ensure!(
            r.u32(at + 4)? == 0 && r.u32(at + 8)? == 0,
            "layout {i} 0x04..0x0C is nonzero"
        );
        let members_at = offset(r, at + 0xC, "layout members")?;
        let members = (0..n)
            .map(|k| {
                let m = members_at + 0x14 * k;
                Ok(Member {
                    unk00: r.i32(m)?,
                    offset: r.u32(m + 4)?,
                    kind: r.u32(m + 8)?,
                    semantic: r.u32(m + 0xC)?,
                    index: r.i32(m + 0x10)?,
                })
            })
            .collect::<Result<_>>()
            .with_context(|| format!("layout {i}"))?;
        layouts.push(Layout { members });
        at += 0x10;
    }

    let mut textures = Vec::with_capacity(n_textures);
    for _ in 0..n_textures {
        zero(r, at + 0x12, 2, "texture 0x12")?;
        textures.push(Texture {
            path: text(offset(r, at, "texture path")?)?,
            kind: text(offset(r, at + 4, "texture type")?)?,
            scale: [r.f32(at + 8)?, r.f32(at + 0xC)?],
            unk10: r.u8(at + 0x10)?,
            unk11: r.u8(at + 0x11)?,
            unk14: r.f32(at + 0x14)?,
            unk18: r.f32(at + 0x18)?,
            unk1c: r.f32(at + 0x1C)?,
        });
        at += 0x20;
    }

    let flver = Flver {
        version,
        data_offset: r.u32(0x0C)?,
        data_length: r.u32(0x10)?,
        bbox_min: vec3(r, 0x28)?,
        bbox_max: vec3(r, 0x34)?,
        true_faces: r.i32(0x40)?,
        total_faces: r.i32(0x44)?,
        index_size,
        unicode,
        unk4a: r.u8(0x4A)?,
        unk4b: r.u8(0x4B)?,
        unk4c: r.i32(0x4C)?,
        unk5c: r.u8(0x5C)?,
        unk5d: r.u8(0x5D)?,
        unk68: r.i32(0x68)?,
        dummies,
        materials,
        bones,
        meshes,
        face_sets,
        vertex_buffers,
        layouts,
        textures,
    };
    flver.check_references()?;
    Ok(flver)
}

impl Flver {
    /// Meshes may alias another mesh's face sets and vertex buffers wholesale (48 retail models
    /// repeat a mesh this way); every table entry must still belong to some mesh.
    fn check_references(&self) -> Result<()> {
        let mut fs_used = vec![false; self.face_sets.len()];
        let mut vb_used = vec![false; self.vertex_buffers.len()];
        for (m, mesh) in self.meshes.iter().enumerate() {
            for &f in &mesh.face_sets {
                ensure!(f < fs_used.len(), "mesh {m} face set {f} is missing");
                fs_used[f] = true;
            }
            for &v in &mesh.vertex_buffers {
                ensure!(v < vb_used.len(), "mesh {m} vertex buffer {v} is missing");
                vb_used[v] = true;
            }
        }
        ensure!(fs_used.iter().all(|&u| u), "orphaned face sets");
        ensure!(vb_used.iter().all(|&u| u), "orphaned vertex buffers");
        Ok(())
    }

    /// Effective index size of a face set: its own field, else the header's.
    pub fn index_size(&self, fs: &FaceSet) -> i32 {
        if fs.index_size != 0 {
            fs.index_size
        } else {
            self.index_size as i32
        }
    }

    /// Mesh-relative vertex indices of a face set, as stored (triangle list or strip).
    pub fn indices(&self, data: &[u8], fs: &FaceSet) -> Result<Vec<u32>> {
        let at = self.data_offset as usize + fs.index_offset as usize;
        let r = Be(data);
        let n = usize::try_from(fs.index_count).context("negative index count")?;
        match self.index_size(fs) {
            16 => (0..n).map(|i| r.u16(at + 2 * i).map(u32::from)).collect(),
            32 => (0..n).map(|i| r.u32(at + 4 * i)).collect(),
            s => bail!("index size {s}"),
        }
    }

    /// Triangle list for a face set, unrolling strips (0xFFFF restarts, degenerates dropped).
    pub fn triangles(&self, data: &[u8], fs: &FaceSet) -> Result<Vec<[u32; 3]>> {
        let idx = self.indices(data, fs)?;
        if fs.strip == 0 {
            ensure!(
                idx.len().is_multiple_of(3),
                "triangle list of {} indices",
                idx.len()
            );
            return Ok(idx.as_chunks::<3>().0.to_vec());
        }
        let mut out = Vec::new();
        let mut flip = false;
        for w in idx.windows(3) {
            if w.contains(&0xFFFF) {
                flip = false;
                continue;
            }
            if w[0] != w[1] && w[1] != w[2] && w[0] != w[2] {
                out.push(if flip {
                    [w[2], w[1], w[0]]
                } else {
                    [w[0], w[1], w[2]]
                });
            }
            flip = !flip;
        }
        Ok(out)
    }

    /// The face set to draw for a mesh: highest detail, no motion blur copy.
    /// Each bone's rest transform in model space. A local transform scales, rotates about X,
    /// then Z, then Y (radians), then translates; it composes onto the parent's.
    pub fn bone_transforms(&self) -> Result<Vec<Xform>> {
        let n = self.bones.len();
        let mut world: Vec<Option<Xform>> = vec![None; n];
        for i in 0..n {
            let (mut chain, mut at) = (Vec::new(), Some(i));
            while let Some(b) = at.filter(|&b| world[b].is_none()) {
                ensure!(chain.len() < n, "bone {i} has a parent cycle");
                chain.push(b);
                at = match usize::try_from(self.bones[b].parent) {
                    Ok(p) => {
                        ensure!(p < n, "bone {b} has parent {p} past {n} bones");
                        Some(p)
                    }
                    Err(_) => None,
                };
            }
            let mut acc = at.and_then(|b| world[b]).unwrap_or(Xform::IDENTITY);
            for &b in chain.iter().rev() {
                let bone = &self.bones[b];
                acc = acc.then(&Xform::local(bone.translation, bone.rotation, bone.scale));
                world[b] = Some(acc);
            }
        }
        Ok(world.into_iter().flatten().collect())
    }

    /// Bone of every vertex: its heaviest bone index when the layout has them, else the
    /// normal's fourth byte; either goes through the mesh's bone table when it has one.
    /// `None` when the index leaves the table.
    pub fn vertex_bones(&self, mesh: &Mesh, v: &Vertices) -> Vec<Option<usize>> {
        (0..v.positions.len())
            .map(|k| {
                let local = match v.bone_indices.get(k) {
                    Some(b) => {
                        let heaviest = v.bone_weights.get(k).map_or(0, |w| {
                            (0..4).fold(0, |best, j| if w[j] > w[best] { j } else { best })
                        });
                        Some(b[heaviest] as usize)
                    }
                    None => v.normal_w.get(k).map(|&w| w as usize),
                };
                let bone = match local {
                    Some(l) if !mesh.bone_indices.is_empty() => mesh.bone_indices.get(l).copied(),
                    Some(l) => Some(l as i32),
                    None => Some(mesh.default_bone),
                };
                bone.and_then(|b| usize::try_from(b).ok())
                    .filter(|&b| b < self.bones.len())
            })
            .collect()
    }

    /// Non-dynamic meshes store each vertex relative to its bone; this moves positions and
    /// normals into model space with the bones' rest transforms (`bone_transforms`).
    pub fn to_model_space(
        &self,
        mesh: &Mesh,
        v: &mut Vertices,
        bones: &[Option<usize>],
        world: &[Xform],
    ) {
        if mesh.dynamic != 0 {
            return;
        }
        for (k, bone) in bones.iter().enumerate() {
            let Some(x) = bone.and_then(|b| world.get(b)) else {
                continue;
            };
            if let Some(p) = v.positions.get_mut(k) {
                *p = x.apply(*p);
            }
            if let Some(n) = v.normals.get_mut(k) {
                let r = Xform {
                    m: x.m,
                    t: [0.0; 3],
                }
                .apply(*n);
                let len = (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt();
                *n = if len > 0.0 { r.map(|c| c / len) } else { r };
            }
        }
    }

    /// Root bone of every bone (itself for a root).
    pub fn bone_roots(&self) -> Vec<usize> {
        (0..self.bones.len())
            .map(|mut i| {
                for _ in 0..self.bones.len() {
                    match usize::try_from(self.bones[i].parent) {
                        Ok(p) if p < self.bones.len() => i = p,
                        _ => break,
                    }
                }
                i
            })
            .collect()
    }

    pub fn main_face_set(&self, mesh: &Mesh) -> Option<&FaceSet> {
        let sets = mesh.face_sets.iter().map(|&i| &self.face_sets[i]);
        sets.clone()
            .find(|f| f.flags & (FS_LOD1 | FS_LOD2 | FS_MOTION_BLUR) == 0)
            .or_else(|| sets.clone().next())
    }

    /// Decodes the mesh's vertex attributes from every buffer it references.
    pub fn vertices(&self, data: &[u8], mesh: &Mesh) -> Result<Vertices> {
        let r = Be(data);
        let mut v = Vertices::default();
        let count = match mesh.vertex_buffers.first() {
            Some(&b) => self.vertex_buffers[b].vertex_count.max(0) as usize,
            None => return Ok(v),
        };
        let uv_scale = if self.version >= 0x2000F {
            2048.0
        } else {
            1024.0
        };
        for &b in &mesh.vertex_buffers {
            let vb = &self.vertex_buffers[b];
            ensure!(
                vb.vertex_count as usize == count,
                "vertex buffer {b} has {} vertices, mesh has {count}",
                vb.vertex_count
            );
            let layout = &self.layouts[vb.layout];
            let size = layout.size()?;
            ensure!(
                size == vb.vertex_size as usize,
                "layout {} is {size} bytes, buffer {b} says {}",
                vb.layout,
                vb.vertex_size
            );
            let base = self.data_offset as usize + vb.offset as usize;
            r.bytes(base, size * count)
                .with_context(|| format!("vertex buffer {b}"))?;
            for m in &layout.members {
                let kind = MemberKind::of(m)?;
                for i in 0..count {
                    let at = base + size * i + m.offset as usize;
                    v.push(kind, m, r, at, uv_scale)?;
                }
            }
        }
        Ok(v)
    }
}

impl Layout {
    pub fn size(&self) -> Result<usize> {
        let mut end = 0;
        for m in &self.members {
            ensure!(
                m.offset as usize == end,
                "member at {:#x}, expected {end:#x}",
                m.offset
            );
            end += MemberKind::of(m)?.size();
        }
        Ok(end)
    }
}

/// The (type, semantic) pairs that occur on the disc, and how each decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberKind {
    /// 0x02 Float3 position.
    Position,
    /// 0x12 normal: signed bytes stored w, z, y, x; `s / 127` for x, y, z, raw w (the vertex's
    /// bone on meshes without bone indices). Checked on am0010 against the PS3 copy: mean dot 0.994.
    NormalS8,
    /// 0x14 tangent: signed bytes stored w, z, y, x, each `s / 127` (w = ±1 handedness).
    TangentS8,
    /// 0x12 bone indices: four bytes stored in reverse order.
    BoneIndicesRev,
    /// 0x10 Byte4A color: RGBA bytes / 255.
    Color,
    /// 0x15 UV: i16 u, v / uv_scale.
    Uv,
    /// 0x16 UVPair: two UVs.
    UvPair,
    /// 0x1A Short4toFloat4A bone weights: i16 / 32767.
    BoneWeights,
}

impl MemberKind {
    pub fn of(m: &Member) -> Result<Self> {
        Ok(match (m.kind, m.semantic) {
            (0x02, 0) => Self::Position,
            (0x12, 2) => Self::BoneIndicesRev,
            (0x12, 3) => Self::NormalS8,
            (0x14, 6) => Self::TangentS8,
            (0x10, 10) => Self::Color,
            (0x15, 5) => Self::Uv,
            (0x16, 5) => Self::UvPair,
            (0x1A, 1) => Self::BoneWeights,
            (k, s) => bail!("layout member type {k:#x} semantic {s} is not supported"),
        })
    }

    pub fn size(self) -> usize {
        match self {
            Self::NormalS8 | Self::TangentS8 | Self::Color | Self::Uv | Self::BoneIndicesRev => 4,
            Self::UvPair | Self::BoneWeights => 8,
            Self::Position => 12,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Vertices {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub tangents: Vec<Vec<[f32; 4]>>,
    pub colors: Vec<Vec<[f32; 4]>>,
    pub uvs: Vec<Vec<[f32; 2]>>,
    pub bone_indices: Vec<[u8; 4]>,
    pub bone_weights: Vec<[f32; 4]>,
    /// Fourth byte of each normal: the vertex's bone on meshes without bone indices.
    pub normal_w: Vec<u8>,
}

fn snorm(b: u8) -> f32 {
    b as i8 as f32 / 127.0
}

fn channel<T>(sets: &mut Vec<Vec<T>>, index: usize) -> &mut Vec<T> {
    if sets.len() <= index {
        sets.resize_with(index + 1, Vec::new);
    }
    &mut sets[index]
}

impl Vertices {
    fn push(
        &mut self,
        kind: MemberKind,
        m: &Member,
        r: Be,
        at: usize,
        uv_scale: f32,
    ) -> Result<()> {
        let idx = m.index.max(0) as usize;
        let b = |k: usize| r.u8(at + k);
        let uv = |k: usize| -> Result<[f32; 2]> {
            Ok([
                r.i16(at + k)? as f32 / uv_scale,
                r.i16(at + k + 2)? as f32 / uv_scale,
            ])
        };
        match kind {
            MemberKind::Position => self.positions.push(vec3(r, at)?),
            MemberKind::NormalS8 => {
                self.normals
                    .push([snorm(b(3)?), snorm(b(2)?), snorm(b(1)?)]);
                self.normal_w.push(b(0)?);
            }
            MemberKind::TangentS8 => channel(&mut self.tangents, idx).push([
                snorm(b(3)?),
                snorm(b(2)?),
                snorm(b(1)?),
                snorm(b(0)?),
            ]),
            MemberKind::Color => channel(&mut self.colors, idx).push([
                b(0)? as f32 / 255.0,
                b(1)? as f32 / 255.0,
                b(2)? as f32 / 255.0,
                b(3)? as f32 / 255.0,
            ]),
            MemberKind::Uv => channel(&mut self.uvs, idx).push(uv(0)?),
            MemberKind::UvPair => {
                channel(&mut self.uvs, 2 * idx).push(uv(0)?);
                channel(&mut self.uvs, 2 * idx + 1).push(uv(4)?);
            }
            MemberKind::BoneIndicesRev => self.bone_indices.push([b(3)?, b(2)?, b(1)?, b(0)?]),
            MemberKind::BoneWeights => {
                let w = |k: usize| -> Result<f32> { Ok(r.i16(at + 2 * k)? as f32 / 32767.0) };
                self.bone_weights.push([w(0)?, w(1)?, w(2)?, w(3)?])
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn local_rotates_x_then_z_then_y() {
        let half = std::f32::consts::FRAC_PI_2;
        // X by 90 takes +Y to +Z; Y by 90 then takes +Z to +X.
        let x = Xform::local([1.0, 2.0, 3.0], [half, half, 0.0], [1.0; 3]);
        assert!(close(x.apply([0.0, 1.0, 0.0]), [2.0, 2.0, 3.0]));
        // Z by 90 takes +X to +Y before Y acts on it.
        let z = Xform::local([0.0; 3], [0.0, half, half], [2.0; 3]);
        assert!(close(z.apply([1.0, 0.0, 0.0]), [0.0, 2.0, 0.0]));
    }

    #[test]
    fn x360_flver_decodes_strips_and_signed_normals() {
        let path = crate::vfs::repo_root().join(crate::vfs::X360_ISO);
        if !path.is_file() {
            return;
        }
        let disc = crate::vfs::Disc::open(&path).unwrap();
        let data = disc
            .asset("model/ac/parts/arm/am0010/am0010_m.bnd.dcx|am0010.flv")
            .unwrap();
        let f = read(&data).unwrap();
        assert_eq!(
            (f.index_size, f.unk4a, f.unk4b, f.unk4c),
            (16, 1, 1, 0xFFFF)
        );
        let mesh = &f.meshes[0];
        let v = f.vertices(&data, mesh).unwrap();
        assert_eq!(v.positions.len(), 10273);
        assert!(v
            .normals
            .iter()
            .all(|n| (n.iter().map(|c| c * c).sum::<f32>().sqrt() - 1.0).abs() < 0.03));
        let tris = f.triangles(&data, f.main_face_set(mesh).unwrap()).unwrap();
        assert!(tris
            .iter()
            .flatten()
            .all(|&i| (i as usize) < v.positions.len()));
        assert!(!tris.is_empty());
    }

    #[test]
    fn composes_onto_parent() {
        let parent = Xform::local([0.0, 1.0, 0.0], [0.0, std::f32::consts::PI, 0.0], [1.0; 3]);
        let child = parent.then(&Xform::local([1.0, 0.0, 0.0], [0.0; 3], [1.0; 3]));
        assert!(close(child.apply([0.0; 3]), [-1.0, 1.0, 0.0]));
    }
}
