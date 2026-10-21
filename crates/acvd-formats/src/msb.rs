//! MSB map layout (`model/map/**/*.msb`). The file is a chain of sections (`MODEL_PARAM_ST`,
//! `EVENT_PARAM_ST`, `POINT_PARAM_ST`, `ROUTE_PARAM_ST`, `LAYER_PARAM_ST`, `PARTS_PARAM_ST`,
//! `MAPSTUDIO_TREE_ST`). Each section starts with `u32 0x00989A6A, u32 type-name offset, u32 n`
//! and `n` absolute offsets: `n - 1` records, then the next section's header (0 after the last).
//! Record string offsets are relative to the record.
//!
//! `PARTS_PARAM_ST` record: `u32 name, u32 kind, u32 index, u32 model name, u32 sib path,
//! f32x3 translation, f32x3 rotation (degrees), f32x3 scale`. Kind 0 is a map piece, 1 an object
//! (`o####`), 2 an enemy or AC (`e####` / `a####`, `m4000_actest.msb`). Rotations match the FLVER
//! local order once converted to radians (`flver::Xform::local`).
//!
//! `POINT_PARAM_ST` record: `u32 name, u32 index, u32 kind, u32 kind index, u32 shape,
//! f32x3 translation, f32x3 rotation (degrees)`. Kinds seen on m4000: 50 water return
//! (水没復帰), 100 / 101 / 102 operation / warning / caution area (作戦 / 警告 / 注意領域),
//! 200 start position (AC初期位置 index 0, vs UNAC用のUNAC初期位置 index 1), 1000 ambient sound,
//! 3000 navigation debug point.

use anyhow::{ensure, Context, Result};
use serde::Serialize;

use crate::reader::Be;

pub const MAGIC: u32 = 0x0098_9A6A;
/// Section version of the `ch_env/*_env.msb` files (`crate::env`).
pub const MAGIC_ENV: u32 = 0x0098_9A6C;

/// `POINT_PARAM_ST` kind of a start position.
pub const POINT_START: u32 = 200;

/// `MODEL_PARAM_ST` record (`0x8280d4c8`) and its `+0x14` draw sub-record (`0x829a5e60`).
#[derive(Debug, Clone, Serialize)]
pub struct Model {
    pub name: String,
    pub kind: u32,
    /// Default light-set slot (`+0x14` sub-record `+0` `i8`, `0x829a5e8c`).
    pub light_set: i8,
    /// Camera-follow X/Z flag (`+0x14` sub-record `+1` bit 7, `0x829a5ec4`, applied at `0x829911f0`).
    pub follow_camera_xz: bool,
    /// Sky pass flag (`+0x14` sub-record `+2` `u8`, `0x829a5ef4`).
    pub is_sky: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Part {
    pub name: String,
    pub model: String,
    /// 0 = map piece, 1 = object, 2 = enemy / AC.
    pub kind: u32,
    pub translation: [f32; 3],
    pub rotation_deg: [f32; 3],
    pub scale: [f32; 3],
    /// Resolved light-set slot (`0x829a5e60` / `0x829a5d00`): `PARTS_PARAM_ST` `+0x4c` sub-record
    /// `+0` `i8` override when `>= 0`, else the matching `MODEL_PARAM_ST`'s `light_set`.
    pub light_set: u8,
    /// Whether the renderer overwrites the part's world matrix X/Z with camera X/Z (`0x829911f0`).
    pub follow_camera_xz: bool,
    /// Whether the model is flagged as a sky piece (`MODEL_PARAM_ST` `+0x14` sub-record `+2`).
    pub is_sky: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Point {
    pub name: String,
    pub kind: u32,
    /// Index among the points of the same kind.
    pub kind_index: u32,
    pub shape: u32,
    pub translation: [f32; 3],
    pub rotation_deg: [f32; 3],
}

pub fn is_msb(data: &[u8]) -> bool {
    data.len() >= 4 && u32::from_be_bytes(data[..4].try_into().unwrap()) == MAGIC
}

/// Record offsets of the section called `name`, or empty when the file has none.
pub(crate) fn section(data: &[u8], name: &str) -> Result<Vec<usize>> {
    let r = Be(data);
    let mut at = 0usize;
    for _ in 0..64 {
        ensure!(
            matches!(r.u32(at)?, MAGIC | MAGIC_ENV),
            "MSB section at {at:#x} has no header"
        );
        let count = r.u32(at + 8)? as usize;
        ensure!(
            count >= 1 && at + 12 + 4 * count <= data.len(),
            "MSB section at {at:#x} count {count}"
        );
        let offs: Vec<usize> = (0..count)
            .map(|i| r.u32(at + 12 + 4 * i).map(|v| v as usize))
            .collect::<Result<_>>()?;
        if r.cstr_sjis(r.u32(at + 4)? as usize)? == name {
            return Ok(offs[..count - 1].to_vec());
        }
        match offs[count - 1] {
            0 => break,
            next => {
                ensure!(
                    next > at && next < data.len(),
                    "MSB section link {next:#x} from {at:#x}"
                );
                at = next;
            }
        }
    }
    Ok(Vec::new())
}

fn vec3(r: &Be, at: usize) -> Result<[f32; 3]> {
    Ok([r.f32(at)?, r.f32(at + 4)?, r.f32(at + 8)?])
}

/// Every `MODEL_PARAM_ST` record (`0x8280d4c8`).
pub fn models(data: &[u8]) -> Result<Vec<Model>> {
    let r = Be(data);
    ensure!(r.u32(0)? == MAGIC, "not an MSB");
    section(data, "MODEL_PARAM_ST")?
        .into_iter()
        .map(|off| {
            let name_rel = r.u32(off)? as usize;
            ensure!(
                name_rel < 0x400,
                "MSB model at {off:#x} name offset {name_rel:#x}"
            );
            let sub_rel = if off + 0x18 <= data.len() {
                r.u32(off + 0x14)? as usize
            } else {
                0
            };
            let (light_set, follow_camera_xz, is_sky) =
                if sub_rel > 0 && off + sub_rel + 3 <= data.len() {
                    let sub = off + sub_rel;
                    (
                        r.u8(sub)? as i8,
                        (r.u8(sub + 1)? & 0x80) != 0,
                        r.u8(sub + 2)? != 0,
                    )
                } else {
                    (1, false, false)
                };
            Ok(Model {
                name: r.cstr_sjis(off + name_rel)?,
                kind: r.u32(off + 4)?,
                light_set,
                follow_camera_xz,
                is_sky,
            })
        })
        .collect()
}

/// Every `PARTS_PARAM_ST` record.
pub fn parts(data: &[u8]) -> Result<Vec<Part>> {
    let r = Be(data);
    ensure!(r.u32(0)? == MAGIC, "not an MSB");
    let model_defs = models(data)?;
    section(data, "PARTS_PARAM_ST")?
        .into_iter()
        .map(|off| {
            let name_rel = r.u32(off)? as usize;
            let model_rel = r.u32(off + 0x0C)? as usize;
            ensure!(
                name_rel < 0x400 && model_rel < 0x400,
                "MSB part at {off:#x} name offsets {name_rel:#x}/{model_rel:#x}"
            );
            let name = r.cstr_sjis(off + name_rel)?;
            let model = r.cstr_sjis(off + model_rel)?;
            let kind = r.u32(off + 4)?;
            let mdef = model_defs
                .iter()
                .find(|m| m.kind == kind && m.name == model);
            let mut light_set = mdef.map_or(1i8, |m| m.light_set);
            let follow_camera_xz = mdef.is_some_and(|m| m.follow_camera_xz);
            let is_sky = mdef.is_some_and(|m| m.is_sky);
            if off + 0x50 <= data.len() {
                let sub_rel = r.u32(off + 0x4C)? as usize;
                if sub_rel > 0 && off + sub_rel < data.len() {
                    let over = r.u8(off + sub_rel)? as i8;
                    if over >= 0 {
                        light_set = over;
                    }
                }
            }
            Ok(Part {
                name,
                model,
                kind,
                translation: vec3(&r, off + 0x14)?,
                rotation_deg: vec3(&r, off + 0x20)?,
                scale: vec3(&r, off + 0x2C)?,
                light_set: light_set.max(0) as u8,
                follow_camera_xz,
                is_sky,
            })
        })
        .collect()
}

/// Every `POINT_PARAM_ST` record.
pub fn points(data: &[u8]) -> Result<Vec<Point>> {
    let r = Be(data);
    ensure!(r.u32(0)? == MAGIC, "not an MSB");
    section(data, "POINT_PARAM_ST")?
        .into_iter()
        .map(|off| {
            let name_rel = r.u32(off)? as usize;
            ensure!(
                name_rel < 0x400,
                "MSB point at {off:#x} name offset {name_rel:#x}"
            );
            Ok(Point {
                name: r
                    .cstr_sjis(off + name_rel)
                    .with_context(|| format!("MSB point at {off:#x}"))?,
                kind: r.u32(off + 8)?,
                kind_index: r.u32(off + 0x0C)?,
                shape: r.u32(off + 0x10)?,
                translation: vec3(&r, off + 0x14)?,
                rotation_deg: vec3(&r, off + 0x20)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_u32(d: &mut [u8], at: usize, v: u32) {
        d[at..at + 4].copy_from_slice(&v.to_be_bytes());
    }
    fn put_f32(d: &mut [u8], at: usize, v: f32) {
        d[at..at + 4].copy_from_slice(&v.to_be_bytes());
    }

    /// One POINT section at 0 (one record at 0x40) linking a PARTS section at 0x100 (one record
    /// at 0x140).
    fn sample() -> Vec<u8> {
        let mut d = vec![0u8; 0x200];
        put_u32(&mut d, 0, MAGIC);
        put_u32(&mut d, 4, 0x18);
        put_u32(&mut d, 8, 2);
        put_u32(&mut d, 0x0C, 0x40);
        put_u32(&mut d, 0x10, 0x100);
        d[0x18..0x27].copy_from_slice(b"POINT_PARAM_ST\0");
        put_u32(&mut d, 0x40, 0x40);
        put_u32(&mut d, 0x48, POINT_START);
        put_f32(&mut d, 0x54, -324.65);
        put_f32(&mut d, 0x64, 90.0);
        d[0x80..0x84].copy_from_slice(b"pos\0");

        put_u32(&mut d, 0x100, MAGIC);
        put_u32(&mut d, 0x104, 0x118);
        put_u32(&mut d, 0x108, 2);
        put_u32(&mut d, 0x10C, 0x140);
        put_u32(&mut d, 0x110, 0);
        d[0x118..0x127].copy_from_slice(b"PARTS_PARAM_ST\0");
        put_u32(&mut d, 0x140, 0x80);
        put_u32(&mut d, 0x14C, 0x85);
        put_f32(&mut d, 0x154, 1.0);
        put_f32(&mut d, 0x158, 2.0);
        put_f32(&mut d, 0x15C, 3.0);
        put_f32(&mut d, 0x164, 90.0);
        put_f32(&mut d, 0x16C, 1.0);
        put_f32(&mut d, 0x170, 1.0);
        put_f32(&mut d, 0x174, 1.0);
        d[0x1C0..0x1C5].copy_from_slice(b"part\0");
        d[0x1C5..0x1CB].copy_from_slice(b"m0001\0");
        d
    }

    #[test]
    fn reads_parts_and_points_by_section_header() {
        let d = sample();
        let p = parts(&d).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!((p[0].name.as_str(), p[0].model.as_str()), ("part", "m0001"));
        assert_eq!(p[0].translation, [1.0, 2.0, 3.0]);
        assert_eq!(p[0].rotation_deg[1], 90.0);
        let q = points(&d).unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!((q[0].name.as_str(), q[0].kind), ("pos", POINT_START));
        assert_eq!(q[0].translation[0], -324.65);
        assert_eq!(q[0].rotation_deg[1], 90.0);
    }

    #[test]
    fn actest_layout_from_disc() {
        let Some(disc) = crate::vfs::test_disc() else {
            return;
        };
        let data = crate::vfs::open(&disc, "model/map/m4000/m4000_actest.msb").unwrap();
        let start = points(&data)
            .unwrap()
            .into_iter()
            .find(|p| p.kind == POINT_START && p.kind_index == 0)
            .unwrap();
        assert_eq!(start.translation, [-324.65, 29.0, 649.5]);
        assert_eq!(start.rotation_deg[1], 90.0);
        assert_eq!(
            parts(&data).unwrap().iter().filter(|p| p.kind == 2).count(),
            22
        );
        let map = crate::vfs::open(&disc, "model/map/m4000/m4000_map.msb").unwrap();
        let map_parts = parts(&map).unwrap();
        assert_eq!(map_parts.len(), 550);
        let by_model = |m: &str| map_parts.iter().find(|p| p.model == m).unwrap();
        assert_eq!(by_model("m0012").light_set, 1);
        assert_eq!(by_model("m9002").light_set, 2);
        assert_eq!(by_model("m9050").light_set, 1);
        assert_eq!(by_model("m9101").light_set, 3);
        assert!(by_model("m9101").is_sky);
        assert_eq!(by_model("o9705").light_set, 0);
        assert!(by_model("o9705").follow_camera_xz);
    }
}
