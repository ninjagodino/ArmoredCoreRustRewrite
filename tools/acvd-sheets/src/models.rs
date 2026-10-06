//! FLVER -> model sheet rows: tables, decoded face sets, and the evidence preflight checks
//! (index ranges, winding around stored normals, vertices no face set reaches).

use acvd_formats::flver;

use crate::model::*;

pub fn sheet(path: &str, data: &[u8]) -> ModelSheet {
    let mut sheet = ModelSheet { path: path.to_owned(), ..Default::default() };
    let f = match flver::read(data) {
        Ok(f) => f,
        Err(e) => {
            sheet.version = acvd_formats::reader::Be(data).u32(8).unwrap_or(0);
            sheet.error = Some(format!("{e:#}"));
            return sheet;
        }
    };
    sheet.version = f.version;
    sheet.bbox_min = f.bbox_min;
    sheet.bbox_max = f.bbox_max;
    sheet.dummies = f.dummies.len();
    sheet.bones = f.bones.len();
    let world = match f.bone_transforms() {
        Ok(w) => w,
        Err(e) => {
            sheet.error = Some(format!("{e:#}"));
            return sheet;
        }
    };
    sheet.skeleton = f
        .bones
        .iter()
        .zip(&world)
        .map(|(b, w)| BoneRow { name: b.name.clone(), parent: b.parent, translation: b.translation, rotation: b.rotation, scale: b.scale, origin: w.t })
        .collect();
    sheet.sockets = f
        .dummies
        .iter()
        .map(|d| {
            let frame = usize::try_from(d.parent_bone).ok().and_then(|b| world.get(b)).copied().unwrap_or(flver::Xform::IDENTITY);
            SocketRow { color: d.color, bone: d.parent_bone, position: frame.apply(d.position) }
        })
        .collect();
    sheet.materials = f
        .materials
        .iter()
        .map(|m| {
            let start = m.texture_index.max(0) as usize;
            let textures = f.textures.iter().skip(start).take(m.texture_count.max(0) as usize).map(|t| (t.kind.clone(), t.path.clone())).collect();
            MaterialRow { name: m.name.clone(), mtd: m.mtd.clone(), textures }
        })
        .collect();
    sheet.meshes = f.meshes.iter().enumerate().map(|(i, m)| mesh(&f, data, &world, i, m)).collect();
    sheet
}

fn mesh(f: &flver::Flver, data: &[u8], world: &[flver::Xform], index: usize, m: &flver::Mesh) -> MeshRow {
    let members = m
        .vertex_buffers
        .iter()
        .flat_map(|&b| f.layouts[f.vertex_buffers[b].layout].members.iter())
        .map(|x| format!("{:#04x}/{}", x.kind, x.semantic))
        .collect();
    let mut row = MeshRow {
        index,
        material: m.material,
        dynamic: m.dynamic,
        vertices: m.vertex_buffers.first().map_or(0, |&b| f.vertex_buffers[b].vertex_count.max(0) as usize),
        members,
        vertex_error: None,
        bounds: None,
        unboned: 0,
        unreferenced: 0,
        clockwise: 0,
        counter_clockwise: 0,
        face_sets: Vec::new(),
    };
    let mut vertices = f.vertices(data, m).map_err(|e| row.vertex_error = Some(format!("{e:#}"))).ok();
    if let Some(v) = vertices.as_mut() {
        let bones = f.vertex_bones(m, v);
        row.unboned = bones.iter().filter(|b| b.is_none()).count();
        f.to_model_space(m, v, &bones, world);
        row.bounds = v.positions.iter().fold(None, |acc: Option<([f32; 3], [f32; 3])>, p| {
            let (mut lo, mut hi) = acc.unwrap_or((*p, *p));
            (0..3).for_each(|k| (lo[k], hi[k]) = (lo[k].min(p[k]), hi[k].max(p[k])));
            Some((lo, hi))
        });
    }
    let mut used = vec![false; row.vertices];
    let main = f.main_face_set(m);
    for &fi in &m.face_sets {
        let fs = &f.face_sets[fi];
        let mut fr = FaceSetRow {
            index: fi,
            flags: fs.flags,
            strip: fs.strip,
            index_size: f.index_size(fs),
            indices: 0,
            triangles: 0,
            degenerate: 0,
            max_index: None,
            error: None,
        };
        match f.indices(data, fs).and_then(|idx| Ok((idx.len(), f.triangles(data, fs)?))) {
            Ok((n, tris)) => {
                fr.indices = n;
                fr.triangles = tris.len();
                fr.degenerate = tris.iter().filter(|t| t[0] == t[1] || t[1] == t[2] || t[0] == t[2]).count();
                fr.max_index = tris.iter().flatten().copied().max();
                for &i in tris.iter().flatten() {
                    if let Some(u) = used.get_mut(i as usize) {
                        *u = true;
                    }
                }
                if main.is_some_and(|x| std::ptr::eq(x, fs)) {
                    if let Some(v) = vertices.as_ref().filter(|v| v.normals.len() == row.vertices && v.positions.len() == row.vertices) {
                        for t in &tris {
                            match winding(v, t) {
                                Some(true) => row.clockwise += 1,
                                Some(false) => row.counter_clockwise += 1,
                                None => {}
                            }
                        }
                    }
                }
            }
            Err(e) => fr.error = Some(format!("{e:#}")),
        }
        row.face_sets.push(fr);
    }
    row.unreferenced = used.iter().filter(|&&u| !u).count();
    row
}

/// Whether the triangle winds clockwise around its vertices' summed normal (right-handed cross
/// product points against it). `None` when the triangle or the normals give no direction.
fn winding(v: &flver::Vertices, t: &[u32; 3]) -> Option<bool> {
    let [a, b, c] = t.map(|i| v.positions.get(i as usize).copied());
    let (a, b, c) = (a?, b?, c?);
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let face = [u[1] * w[2] - u[2] * w[1], u[2] * w[0] - u[0] * w[2], u[0] * w[1] - u[1] * w[0]];
    let mut n = [0f32; 3];
    for &i in t {
        let x = v.normals.get(i as usize)?;
        (0..3).for_each(|k| n[k] += x[k]);
    }
    let d: f32 = (0..3).map(|k| face[k] * n[k]).sum();
    if d == 0.0 || !d.is_finite() {
        None
    } else {
        Some(d < 0.0)
    }
}
