//! CCM / CCF font glyph map (`font/<name>/<name>.ccm`; fonts 12 and 14 use the `.ccf` twin).
//! The glyphs live in `font/<name>/<name>_t.bnd|<name>.tpf`, one BC3 sheet per texture index.
//!
//! Header (0x20 bytes): `u32 version (0x10000, 0x10001, 0x10002), u32 size, i16 line_height,
//! i16 tex_width, i16 tex_height (0 in some 0x10000 files: take the TPF's size), i16 unk0e
//! (0 or 32), i16 group_count, i16 glyph_count, u32 0x20 (groups offset), u32 glyph_offset,
//! u8 unk1c (1), u8 unk1d (1), u8 texture_count, u8 0`, then groups of `i32 first_code,
//! i32 last_code, i32 first_glyph` (UTF-16 code units, ascending), then glyphs of
//! `f32 u0, f32 v0, f32 u1, f32 v1, i16 pre_space, i16 width, i16 advance, i16 texture`
//! plus `i16 unk[2]` in version 0x10002 (pairs (0,0), (1,2), (2,1), (3,3), (5,5)).
//! UVs are fractions of the sheet; the other fields are pixels. `.ccf` files pad `size` to 0x20.

use anyhow::{ensure, Result};
use serde::Serialize;

use crate::reader::Be;

#[derive(Debug, Clone, Serialize)]
pub struct Ccm {
    pub version: u32,
    pub line_height: i16,
    pub tex_width: i16,
    pub tex_height: i16,
    pub unk0e: i16,
    pub texture_count: u8,
    pub groups: Vec<CodeGroup>,
    pub glyphs: Vec<Glyph>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct CodeGroup {
    pub first: i32,
    pub last: i32,
    pub glyph: i32,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct Glyph {
    pub uv0: [f32; 2],
    pub uv1: [f32; 2],
    pub pre_space: i16,
    pub width: i16,
    pub advance: i16,
    pub texture: i16,
    pub unk: Option<[i16; 2]>,
}

pub fn read(data: &[u8]) -> Result<Ccm> {
    let r = Be(data);
    let version = r.u32(0)?;
    ensure!(matches!(version, 0x10000..=0x10002), "CCM version {version:#x}");
    let size = r.u32(4)? as usize;
    ensure!(size <= data.len(), "CCM size {size:#x} past file end {:#x}", data.len());
    let group_count = r.i16(0x10)?.max(0) as usize;
    let glyph_count = r.i16(0x12)?.max(0) as usize;
    ensure!(r.u32(0x14)? == 0x20, "CCM groups not at 0x20");
    let glyph_at = r.u32(0x18)? as usize;
    ensure!(glyph_at == 0x20 + 12 * group_count, "CCM glyphs at {glyph_at:#x}, expected after {group_count} groups");
    let texture_count = r.u8(0x1E)?;
    let groups: Vec<CodeGroup> = (0..group_count)
        .map(|i| Ok(CodeGroup { first: r.i32(0x20 + 12 * i)?, last: r.i32(0x24 + 12 * i)?, glyph: r.i32(0x28 + 12 * i)? }))
        .collect::<Result<_>>()?;
    let stride = if version == 0x10002 { 28 } else { 24 };
    ensure!(glyph_at + stride * glyph_count == size, "CCM glyph table ends at {:#x}, size {size:#x}", glyph_at + stride * glyph_count);
    let glyphs: Vec<Glyph> = (0..glyph_count)
        .map(|i| {
            let at = glyph_at + stride * i;
            let texture = r.i16(at + 22)?;
            ensure!((0..texture_count as i16).contains(&texture), "glyph {i} texture {texture} of {texture_count}");
            Ok(Glyph {
                uv0: [r.f32(at)?, r.f32(at + 4)?],
                uv1: [r.f32(at + 8)?, r.f32(at + 12)?],
                pre_space: r.i16(at + 16)?,
                width: r.i16(at + 18)?,
                advance: r.i16(at + 20)?,
                texture,
                unk: (stride == 28).then(|| Ok::<_, anyhow::Error>([r.i16(at + 24)?, r.i16(at + 26)?])).transpose()?,
            })
        })
        .collect::<Result<_>>()?;
    let mut next = 0;
    for (i, g) in groups.iter().enumerate() {
        ensure!(g.glyph == next && g.first <= g.last, "code group {i} out of order");
        next += g.last - g.first + 1;
    }
    ensure!(next as usize == glyph_count, "code groups cover {next} of {glyph_count} glyphs");
    Ok(Ccm { version, line_height: r.i16(8)?, tex_width: r.i16(0x0A)?, tex_height: r.i16(0x0C)?, unk0e: r.i16(0x0E)?, texture_count, groups, glyphs })
}

impl Ccm {
    /// Glyph for UTF-16 code unit `code`.
    pub fn glyph(&self, code: u16) -> Option<&Glyph> {
        let c = code as i32;
        let i = self.groups.partition_point(|g| g.last < c);
        let g = self.groups.get(i).filter(|g| g.first <= c)?;
        self.glyphs.get((g.glyph + c - g.first) as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_up_codes_through_groups() {
        let groups = [(0x20, 0x20, 0), (0x30, 0x31, 1)];
        let mut out = Vec::new();
        let size = 0x20 + 12 * groups.len() + 24 * 3;
        for v in [0x10000u32, size as u32] {
            out.extend(v.to_be_bytes());
        }
        for v in [18i16, 64, 64, 0, groups.len() as i16, 3] {
            out.extend(v.to_be_bytes());
        }
        out.extend(0x20u32.to_be_bytes());
        out.extend(((0x20 + 12 * groups.len()) as u32).to_be_bytes());
        out.extend([1, 1, 1, 0]);
        for (a, b, g) in groups {
            for v in [a, b, g] {
                out.extend((v as i32).to_be_bytes());
            }
        }
        for k in 0..3 {
            for f in [k as f32 * 0.125, 0.0, k as f32 * 0.125 + 0.1, 0.28] {
                out.extend(f.to_be_bytes());
            }
            for v in [1i16, 8, 10 + k, 0] {
                out.extend(v.to_be_bytes());
            }
        }
        let c = read(&out).unwrap();
        assert_eq!(c.glyph(b' ' as u16).unwrap().advance, 10);
        assert_eq!(c.glyph(b'1' as u16).unwrap().advance, 12);
        assert!(c.glyph(b'2' as u16).is_none());
        assert!(c.glyph(b'!' as u16).is_none());
    }

    #[test]
    fn disc_fonts() {
        let usrdir = crate::vfs::usrdir(&std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join("ACVD Unbound"));
        let font = usrdir.join("font");
        if !font.is_dir() {
            return;
        }
        let mut n = 0;
        for dir in std::fs::read_dir(&font).unwrap().flatten() {
            let p = dir.path();
            if !p.is_dir() {
                continue;
            }
            for ext in ["ccm", "ccf"] {
                let file = p.join(format!("{}.{ext}", dir.file_name().to_string_lossy()));
                if !file.is_file() {
                    continue;
                }
                let c = read(&std::fs::read(&file).unwrap()).unwrap_or_else(|e| panic!("{}: {e:#}", file.display()));
                n += 1;
                if file.file_name().unwrap() == "e1_ext.ccm" {
                    assert_eq!(c.line_height, 20);
                    assert!(c.glyph(b'A' as u16).is_some());
                }
            }
        }
        assert!(n >= 26, "{n} CCM/CCF");
    }
}
