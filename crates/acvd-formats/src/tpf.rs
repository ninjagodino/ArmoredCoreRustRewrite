//! TPF texture pack, PS3 platform (the only platform byte on the disc).
//!
//! Header (0x10 bytes): `"TPF\0", u32 data_size, u32 texture_count, u8 platform (2 = PS3),
//! u8 flag2, u8 encoding, u8 0`, then per texture:
//! `u32 data_offset, u32 data_size, u8 format, u8 kind (0 = 2D, 1 = cube), u8 mipmaps,
//!  u8 flags1, u16 width, u16 height, u32 unk1, [u32 unk2 when flag2 != 0], u32 name_offset,
//!  u32 has_floats, [u32 floats_unk, u32 floats_len, f32 x floats_len/4 when has_floats]`.
//! Names are Shift-JIS (encoding 0 or 2) or UTF-16 (encoding 1). Texture data is the raw RSX
//! image: for the block-compressed formats on the disc, every mip level of every face in order,
//! with `mipmaps == 0` meaning the full chain down to 1x1. Cube faces after the first start on a
//! 128-byte boundary (65 of the 66 cube maps on the disc: 6 x 10936-byte faces are stored in
//! 65976 bytes); the remaining one stores its faces back to back.

use anyhow::{bail, ensure, Result};
use serde::Serialize;

use crate::reader::{utf16be, Be};

pub const MAGIC: &[u8; 4] = b"TPF\0";
pub const PLATFORM_PS3: u8 = 2;

#[derive(Debug, Clone, Serialize)]
pub struct Tpf {
    pub data_size: u32,
    pub platform: u8,
    pub flag2: u8,
    pub encoding: u8,
    pub textures: Vec<Texture>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Texture {
    pub name: String,
    pub offset: u32,
    pub size: u32,
    pub format: u8,
    pub kind: u8,
    pub mipmaps: u8,
    pub flags1: u8,
    pub width: u16,
    pub height: u16,
    pub unk1: u32,
    pub unk2: Option<u32>,
    pub floats_unk: Option<u32>,
    pub floats: Vec<f32>,
}

pub fn is_tpf(data: &[u8]) -> bool {
    data.starts_with(MAGIC)
}

pub fn read(data: &[u8]) -> Result<Tpf> {
    let r = Be(data);
    ensure!(r.bytes(0, 4)? == MAGIC, "not a TPF");
    let platform = r.u8(0x0C)?;
    ensure!(platform == PLATFORM_PS3, "TPF platform {platform} is not PS3");
    let flag2 = r.u8(0x0D)?;
    let encoding = r.u8(0x0E)?;
    ensure!(r.u8(0x0F)? == 0, "TPF byte 0x0F is nonzero");
    let count = r.u32(0x08)? as usize;
    let mut at = 0x10;
    let mut textures = Vec::with_capacity(count);
    for i in 0..count {
        let offset = r.u32(at)?;
        let size = r.u32(at + 4)?;
        let (format, kind, mipmaps, flags1) = (r.u8(at + 8)?, r.u8(at + 9)?, r.u8(at + 10)?, r.u8(at + 11)?);
        let (width, height) = (r.u16(at + 12)?, r.u16(at + 14)?);
        let unk1 = r.u32(at + 16)?;
        at += 20;
        let unk2 = if flag2 != 0 {
            at += 4;
            Some(r.u32(at - 4)?)
        } else {
            None
        };
        let name_offset = r.u32(at)? as usize;
        let has_floats = r.u32(at + 4)?;
        at += 8;
        ensure!(has_floats <= 1, "texture {i} float flag {has_floats}");
        let (mut floats_unk, mut floats) = (None, Vec::new());
        if has_floats == 1 {
            floats_unk = Some(r.u32(at)?);
            let len = r.u32(at + 4)? as usize;
            ensure!(len.is_multiple_of(4), "texture {i} float block length {len}");
            floats = (0..len / 4).map(|k| r.f32(at + 8 + 4 * k)).collect::<Result<_>>()?;
            at += 8 + len;
        }
        let name = match encoding {
            0 | 2 => r.cstr_sjis(name_offset)?,
            1 => utf16be(r.bytes(name_offset, data.len().saturating_sub(name_offset))?),
            e => bail!("TPF name encoding {e}"),
        };
        r.bytes(offset as usize, size as usize)?;
        textures.push(Texture { name, offset, size, format, kind, mipmaps, flags1, width, height, unk1, unk2, floats_unk, floats });
    }
    Ok(Tpf { data_size: r.u32(0x04)?, platform, flag2, encoding, textures })
}

impl Texture {
    pub fn data<'a>(&self, tpf: &'a [u8]) -> Result<&'a [u8]> {
        Be(tpf).bytes(self.offset as usize, self.size as usize)
    }

    pub fn faces(&self) -> u32 {
        if self.kind == 1 {
            6
        } else {
            1
        }
    }

    /// Mip levels stored: `mipmaps`, or the full chain when it is 0.
    pub fn levels(&self) -> u32 {
        if self.mipmaps > 0 {
            self.mipmaps as u32
        } else {
            32 - (self.width.max(self.height).max(1) as u32).leading_zeros()
        }
    }

    /// Byte size of the stored image for a 4x4 block format of `block_bytes` bytes per block.
    pub fn block_size(&self, block_bytes: u32) -> u64 {
        block_chain_size(self.width, self.height, self.levels(), self.faces(), block_bytes)
    }
}

pub const FACE_ALIGN: u64 = 128;

fn chain(width: u16, height: u16, levels: u32, block_bytes: u32) -> u64 {
    let level = |m: u32| {
        let w = (width as u32 >> m).max(1).div_ceil(4);
        let h = (height as u32 >> m).max(1).div_ceil(4);
        (w * h * block_bytes) as u64
    };
    (0..levels).map(level).sum()
}

/// Bytes in `faces` mip chains of `levels` levels of a 4x4 block-compressed image, with every
/// face but the last padded to [`FACE_ALIGN`].
pub fn block_chain_size(width: u16, height: u16, levels: u32, faces: u32, block_bytes: u32) -> u64 {
    let face = chain(width, height, levels, block_bytes);
    face.next_multiple_of(FACE_ALIGN) * (faces.max(1) as u64 - 1) + face
}

/// Byte offset of mip `level` of `face` inside a stored image of `stored` bytes, and that level's
/// size. Faces are packed back to back when `stored` is exactly `faces` unpadded chains.
pub fn block_level_span(width: u16, height: u16, levels: u32, faces: u32, stored: u64, face: u32, level: u32, block_bytes: u32) -> (u64, u64) {
    let unpadded = chain(width, height, levels, block_bytes);
    let stride = if stored == unpadded * faces as u64 { unpadded } else { unpadded.next_multiple_of(FACE_ALIGN) };
    let before = chain(width, height, level, block_bytes);
    let this = chain(width, height, level + 1, block_bytes) - before;
    (face as u64 * stride + before, this)
}
