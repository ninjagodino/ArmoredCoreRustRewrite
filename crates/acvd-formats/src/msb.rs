//! MSB map layout (`model/map/**/*.msb`). The file is a chain of named PARAM-style sections
//! (`MODEL_PARAM_ST`, `EVENT_PARAM_ST`, `POINT_PARAM_ST`, `ROUTE_PARAM_ST`, `LAYER_PARAM_ST`,
//! `PARTS_PARAM_ST`, `MAPSTUDIO_TREE_ST`) sharing the header magic `0x00989A6A`.
//!
//! This reader only returns `PARTS_PARAM_ST` placements: an offset table immediately before the
//! type name, each record `u32 name offset, u32 kind, u32 index, u32 model-name offset, u32 sib
//! path offset, f32x3 translation, f32x3 rotation (degrees), f32x3 scale`, then the strings at
//! those offsets. Kind 0 is a map piece, kind 1 an object (`o####` models). Rotations match the
//! FLVER local order once converted to radians (`flver::Xform::local`).

use anyhow::{ensure, Result};
use serde::Serialize;

use crate::reader::Be;

pub const MAGIC: u32 = 0x0098_9A6A;

#[derive(Debug, Clone, Serialize)]
pub struct Part {
    pub name: String,
    pub model: String,
    /// 0 = map piece, 1 = object (360 MSB `PARTS_PARAM_ST`).
    pub kind: u32,
    pub translation: [f32; 3],
    pub rotation_deg: [f32; 3],
    pub scale: [f32; 3],
}

pub fn is_msb(data: &[u8]) -> bool {
    data.len() >= 4 && u32::from_be_bytes(data[..4].try_into().unwrap()) == MAGIC
}

/// Every `PARTS_PARAM_ST` record whose first word is a name offset inside the record.
pub fn parts(data: &[u8]) -> Result<Vec<Part>> {
    let r = Be(data);
    ensure!(r.u32(0)? == MAGIC, "not an MSB");
    let Some(name_at) = find(data, b"PARTS_PARAM_ST\0") else {
        return Ok(Vec::new());
    };
    let mut offs = Vec::new();
    let mut at = name_at;
    while at >= 4 {
        at -= 4;
        let v = r.u32(at)? as usize;
        if v > name_at + 16 && v < data.len() && offs.first().is_none_or(|&first| v < first) {
            offs.insert(0, v);
        } else {
            break;
        }
    }
    let mut out = Vec::new();
    for off in offs {
        if r.u32(off)? == MAGIC {
            continue;
        }
        let name_rel = r.u32(off)? as usize;
        let model_rel = r.u32(off + 0x0C)? as usize;
        ensure!(name_rel < 0x200 && model_rel < 0x200, "MSB part at {off:#x} name offsets {name_rel:#x}/{model_rel:#x}");
        out.push(Part {
            name: r.cstr_sjis(off + name_rel)?,
            model: r.cstr_sjis(off + model_rel)?,
            kind: r.u32(off + 4)?,
            translation: [r.f32(off + 0x14)?, r.f32(off + 0x18)?, r.f32(off + 0x1C)?],
            rotation_deg: [r.f32(off + 0x20)?, r.f32(off + 0x24)?, r.f32(off + 0x28)?],
            scale: [r.f32(off + 0x2C)?, r.f32(off + 0x30)?, r.f32(off + 0x34)?],
        });
    }
    Ok(out)
}

fn find(data: &[u8], pat: &[u8]) -> Option<usize> {
    data.windows(pat.len()).position(|w| w == pat)
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

    #[test]
    fn reads_one_part() {
        // Offset table at 0x20, name at 0x24, record at 0x50, strings at record+0x80.
        let mut d = vec![0u8; 0xE0];
        put_u32(&mut d, 0, MAGIC);
        put_u32(&mut d, 0x20, 0x50);
        d[0x24..0x33].copy_from_slice(b"PARTS_PARAM_ST\0");
        put_u32(&mut d, 0x50, 0x80);
        put_u32(&mut d, 0x5C, 0x85);
        put_f32(&mut d, 0x64, 1.0);
        put_f32(&mut d, 0x68, 2.0);
        put_f32(&mut d, 0x6C, 3.0);
        put_f32(&mut d, 0x74, 90.0);
        put_f32(&mut d, 0x7C, 1.0);
        put_f32(&mut d, 0x80, 1.0);
        put_f32(&mut d, 0x84, 1.0);
        d[0xD0..0xD5].copy_from_slice(b"part\0");
        d[0xD5..0xDB].copy_from_slice(b"m0001\0");
        let p = parts(&d).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!((p[0].name.as_str(), p[0].model.as_str()), ("part", "m0001"));
        assert_eq!(p[0].translation, [1.0, 2.0, 3.0]);
        assert_eq!(p[0].rotation_deg[1], 90.0);
    }
}
