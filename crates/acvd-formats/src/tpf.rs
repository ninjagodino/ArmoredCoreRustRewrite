//! TPF texture pack, PS3 (platform 2) or Xbox 360 (platform 1).
//!
//! Header (0x10 bytes): `"TPF\0", u32 data_size, u32 texture_count, u8 platform, u8 flag2,
//! u8 encoding, u8 0`, then per texture:
//! `u32 data_offset, u32 data_size, u8 format, u8 kind (0 = 2D, 1 = cube), u8 mipmaps,
//!  u8 flags1, u16 width, u16 height, u32 unk1, [u32 unk2 when PS3 and flag2 != 0],
//!  u32 name_offset, u32 has_floats, [u32 floats_unk, u32 floats_len, f32 x floats_len/4 when
//!  has_floats]`. The 360 entry has no `unk2` (every 360 pack has flag2 3 and 28-byte entries).
//! Names are Shift-JIS (encoding 0 or 2) or UTF-16 (encoding 1). PS3 texture data is the raw RSX
//! image: for the block-compressed formats on the disc, every mip level of every face in order,
//! with `mipmaps == 0` meaning the full chain down to 1x1. Cube faces after the first start on a
//! 128-byte boundary (65 of the 66 cube maps on the disc: 6 x 10936-byte faces are stored in
//! 65976 bytes); the remaining one stores its faces back to back. 360 texture data is the Xenos
//! tiled image ([`xenos`]); [`Texture::linear`] turns it into the PS3 layout.

use std::borrow::Cow;

use anyhow::{bail, ensure, Result};
use serde::Serialize;

use crate::reader::{utf16be, Be};

pub const MAGIC: &[u8; 4] = b"TPF\0";
pub const PLATFORM_X360: u8 = 1;
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
    ensure!(platform == PLATFORM_PS3 || platform == PLATFORM_X360, "TPF platform {platform} is neither PS3 nor 360");
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
        let unk2 = if flag2 != 0 && platform == PLATFORM_PS3 {
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
        textures.push(Texture { name, offset, size, format, kind, mipmaps, flags1, width, height, unk1, unk2, floats_unk, floats });
    }
    Ok(Tpf { data_size: r.u32(0x04)?, platform, flag2, encoding, textures })
}

impl Texture {
    /// The stored bytes. Fails for a texture that runs past the end of the pack (the last one in
    /// the 360 `model/ene/e4050/e4050.tpf.dcx`), which [`read`] still lists.
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

    /// The image in the PS3 layout (every level of every face, linear little-endian blocks): the
    /// stored bytes for PS3 packs, the untiled Xenos image for 360 ones, with faces back to back.
    pub fn linear<'a>(&self, platform: u8, tpf: &'a [u8], block_bytes: u32) -> Result<Cow<'a, [u8]>> {
        let data = self.data(tpf)?;
        if platform != PLATFORM_X360 {
            return Ok(Cow::Borrowed(data));
        }
        Ok(Cow::Owned(xenos::untile(data, self.width as u32, self.height as u32, self.levels(), self.faces(), block_bytes)?))
    }
}

/// Bytes a block-compressed image takes on `platform` (see [`block_chain_size`], [`xenos::size`]).
pub fn stored_size(platform: u8, width: u16, height: u16, levels: u32, faces: u32, block_bytes: u32) -> u64 {
    if platform == PLATFORM_X360 {
        xenos::size(width as u32, height as u32, levels, faces, block_bytes)
    } else {
        block_chain_size(width, height, levels, faces, block_bytes)
    }
}

/// Xenos (360 GPU) tiled layout of 4x4 block-compressed textures, as Xenia's `texture_util` and
/// `texture_address` describe it. Per level, every face's slice in turn; a slice is the level
/// padded to 32x32 blocks (row pitch and rows rounded to powers of two of the base size past
/// level 0) and to 4 KB. Levels from the first whose short side is 16 texels or less share
/// one packed tail slice. Blocks are 32x32-block macro tiles of 8x16 micro tiles with pipe and
/// bank swizzles ([`tiled_offset`]), and their 16-bit words are big-endian.
pub mod xenos {
    use anyhow::{ensure, Result};

    const TILE: u32 = 32;
    const SLICE_ALIGN: u64 = 4096;

    fn log2_ceil(v: u32) -> u32 {
        32 - v.max(1).saturating_sub(1).leading_zeros()
    }

    /// First level stored in the packed mip tail.
    pub fn packed_level(width: u32, height: u32) -> u32 {
        log2_ceil(width.min(height)).saturating_sub(4)
    }

    /// Row pitch and row count in blocks, and the slice bytes, of stored level `level`.
    fn slice(width: u32, height: u32, level: u32, block_bytes: u32) -> (u32, u32, u64) {
        let (w, h) = if level == 0 {
            let packed = packed_level(width, height) == 0;
            (width, if packed { height.next_power_of_two() } else { height })
        } else {
            ((width.next_power_of_two() >> level).max(1), (height.next_power_of_two() >> level).max(1))
        };
        let pitch = w.div_ceil(4).next_multiple_of(TILE);
        let rows = h.div_ceil(4).next_multiple_of(TILE);
        (pitch, rows, (pitch as u64 * rows as u64 * block_bytes as u64).next_multiple_of(SLICE_ALIGN))
    }

    fn stored_levels(width: u32, height: u32, levels: u32) -> u32 {
        levels.min(packed_level(width, height) + 1)
    }

    /// Bytes of a tiled image of `levels` levels and `faces` faces.
    pub fn size(width: u32, height: u32, levels: u32, faces: u32, block_bytes: u32) -> u64 {
        (0..stored_levels(width, height, levels)).map(|l| slice(width, height, l, block_bytes).2 * faces.max(1) as u64).sum()
    }

    /// Byte offset of block (`x`, `y`) in a tiled slice `pitch` blocks wide (Xenia `Tiled2D`).
    pub fn tiled_offset(x: u32, y: u32, pitch: u32, block_bytes: u32) -> u64 {
        let log2 = block_bytes.trailing_zeros();
        let outer = ((y >> 5) * (pitch >> 5) + (x >> 5)) << 6;
        let inner = (((y >> 1) & 7) << 3) | (x & 7);
        let bytes = ((outer | inner) as u64) << log2;
        let bank = ((y >> 4) & 1) as u64;
        let pipe = (((x >> 3) & 3) ^ (((y >> 3) & 1) << 1)) as u64;
        ((y as u64 & 1) << 4) | (pipe << 6) | (bank << 11) | (bytes & 0xF) | (((bytes >> 4) & 1) << 5) | (((bytes >> 5) & 7) << 8) | (bytes >> 8 << 12)
    }

    /// Block offset of `level` inside the packed tail slice (Xenia `GetPackedMipOffset`, 2D).
    pub fn packed_offset(width: u32, height: u32, level: u32) -> (u32, u32) {
        let (lw, lh) = (log2_ceil(width), log2_ceil(height));
        let base = lw.min(lh).saturating_sub(4);
        let m = level - base;
        let (x, y) = if m < 3 {
            if lw > lh { (0, 16 >> m) } else { (16 >> m, 0) }
        } else if lw > lh {
            ((1 << (lw - base)) >> (m - 2), 0)
        } else {
            (0, (1 << (lh - base)) >> (m - 2))
        };
        (x >> 2, y >> 2)
    }

    /// The linear little-endian block image (every level of face 0, then face 1, ...) of a
    /// tiled one.
    pub fn untile(data: &[u8], width: u32, height: u32, levels: u32, faces: u32, block_bytes: u32) -> Result<Vec<u8>> {
        let faces = faces.max(1);
        let want = size(width, height, levels, faces, block_bytes);
        ensure!(data.len() as u64 >= want, "tiled {width}x{height} image is {} bytes, needs {want}", data.len());
        let packed = packed_level(width, height);
        let bb = block_bytes as usize;
        let mut starts = Vec::new();
        let mut at = 0u64;
        for l in 0..stored_levels(width, height, levels) {
            let (pitch, _, bytes) = slice(width, height, l, block_bytes);
            starts.push((at, pitch, bytes));
            at += bytes * faces as u64;
        }
        let mut out = Vec::new();
        for face in 0..faces as u64 {
            for level in 0..levels {
                let stored = level.min(packed) as usize;
                let (start, pitch, bytes) = starts[stored];
                let (ox, oy) = if level >= packed { packed_offset(width, height, level) } else { (0, 0) };
                let base = (start + face * bytes) as usize;
                let (bw, bh) = ((width >> level).max(1).div_ceil(4), (height >> level).max(1).div_ceil(4));
                for y in 0..bh {
                    for x in 0..bw {
                        let src = base + tiled_offset(x + ox, y + oy, pitch, block_bytes) as usize;
                        let block = &data[src..src + bb];
                        for pair in block.chunks_exact(2) {
                            out.extend_from_slice(&[pair[1], pair[0]]);
                        }
                    }
                }
            }
        }
        Ok(out)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs;

    #[test]
    fn xenos_sizes_match_the_360_disc() {
        // image/authenticate/authenticate00.tpf: 1024x256 BC1, 4 levels, no packed tail.
        assert_eq!(xenos::size(1024, 256, 4, 1, 8), 188_416);
        // Global_env+c: 128x128 BC1 cube, 8 levels; levels 3..7 share the tail slice.
        assert_eq!(xenos::size(128, 128, 8, 6, 8), 196_608);
        // e7201_n: 2048x2048 8-byte blocks, 5 levels.
        assert_eq!(xenos::size(2048, 2048, 5, 1, 8), 2_793_472);
        assert_eq!(xenos::packed_level(128, 128), 3);
        assert_eq!(xenos::packed_level(16, 16), 0);
        assert_eq!(xenos::tiled_offset(0, 0, 32, 8), 0);
        assert_eq!(xenos::tiled_offset(1, 0, 32, 8), 8);
        assert_eq!(xenos::tiled_offset(0, 1, 32, 8), 16);
    }

    #[test]
    fn x360_textures_untile_to_the_ps3_blocks() {
        let root = vfs::repo_root();
        let (iso, dump) = (root.join(vfs::X360_ISO), root.join(vfs::PS3_DUMP));
        if !iso.is_file() || !dump.is_dir() {
            return;
        }
        // am9000's BC1 and BC3 textures were compressed once for both discs, so the untiled 360
        // blocks equal the PS3 ones byte for byte, every level included.
        let pack = "model/ac/parts/arm/am9000/am9000.tpf.dcx";
        let (x, p) = (vfs::Disc::open(&iso).unwrap().asset(pack).unwrap(), vfs::Disc::open(&dump).unwrap().asset(pack).unwrap());
        let (xt, pt) = (read(&x).unwrap(), read(&p).unwrap());
        assert_eq!(xt.platform, PLATFORM_X360);
        let compared = xt.textures.iter().filter(|a| a.format == 0 || a.format == 5).count();
        assert_eq!(compared, 8);
        for (a, b) in xt.textures.iter().zip(&pt.textures).filter(|(a, _)| a.format == 0 || a.format == 5) {
            let bb = if a.format == 0 { 8 } else { 16 };
            assert_eq!(a.size as u64, stored_size(xt.platform, a.width, a.height, a.levels(), a.faces(), bb));
            let (la, lb) = (a.linear(xt.platform, &x, bb).unwrap(), b.linear(pt.platform, &p, bb).unwrap());
            let n = block_chain_size(a.width, a.height, a.levels().min(b.levels()), 1, bb) as usize;
            assert_eq!(la[..n], lb[..n], "{}", a.name);
        }
    }
}
