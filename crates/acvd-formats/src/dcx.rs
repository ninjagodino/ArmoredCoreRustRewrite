//! DCX: EDGE on the PS3 disc (all 9426 files), DFLT on the 360 disc.
//!
//! DFLT (big-endian): `0x00 "DCX\0", u32 0x10000, u32 0x18, u32 0x24, u32 0x24, u32 0x2C,
//! 0x18 "DCS\0", u32 uncompressed_size, u32 compressed_size, 0x24 "DCP\0", "DFLT", u32 0x20,
//! u32 0x09000000, u32 0, u32 0, u32 0, u32 0x00010100, 0x44 "DCA\0", u32 8`, then one zlib stream
//! of `compressed_size` bytes at 0x4C.
//!
//! EDGE layout (big-endian):
//! `0x00 "DCX\0", u32 0x10000, u32 0x18, u32 0x24, u32 0x24, u32 0x2C + egdt_size,
//!  0x18 "DCS\0", u32 uncompressed_size, u32 compressed_size,
//!  0x24 "DCP\0", "EDGE", u32 0x20, u32 0x09000000, u32 0x10000, u32 0, u32 0, u32 0x00100100,
//!  0x44 "DCA\0", u32 8 + egdt_size,
//!  0x4C "EgdT", u32 0x00010100, u32 0x24, u32 0x10, u32 0x10000, u32 last_chunk_size,
//!       u32 egdt_size (0x24 + 0x10 * chunk_count), u32 chunk_count, u32 0x100000,
//!  0x70 chunk_count x { u32 0, u32 offset, u32 size, u32 deflated }`,
//! then chunk data at `0x44 + dca_size + offset`. Each chunk is raw deflate (or stored when
//! `deflated == 0`) and expands to 0x10000 bytes, except the last, which expands to `last_chunk_size`.
//! Some files carry nonzero bytes after the last chunk; the header does not cover them and they
//! are reported as `trailing_bytes`.

use anyhow::{bail, ensure, Context, Result};
use serde::Serialize;

use crate::reader::Be;

pub const MAGIC: &[u8; 4] = b"DCX\0";
pub const CHUNK_SIZE: u32 = 0x10000;
const DCA_AT: usize = 0x44;
const CHUNKS_AT: usize = 0x70;
const DFLT_DATA_AT: usize = 0x4C;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Variant {
    Edge,
    Dflt,
}

#[derive(Debug, Clone, Serialize)]
pub struct Dcx {
    pub variant: Variant,
    pub uncompressed_size: u32,
    pub compressed_size: u32,
    pub last_chunk_size: u32,
    pub trailing_bytes: usize,
    pub chunks: Vec<Chunk>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Chunk {
    pub offset: u32,
    pub size: u32,
    pub deflated: bool,
}

pub fn is_dcx(data: &[u8]) -> bool {
    data.starts_with(MAGIC)
}

fn expect_u32(r: &Be, at: usize, want: u32) -> Result<()> {
    let got = r.u32(at)?;
    ensure!(got == want, "DCX field at {at:#x} is {got:#x}, expected {want:#x}");
    Ok(())
}

fn expect_tag(r: &Be, at: usize, want: &[u8; 4]) -> Result<()> {
    let got = r.bytes(at, 4)?;
    ensure!(got == want, "DCX tag at {at:#x} is {got:02x?}, expected {:?}", String::from_utf8_lossy(want));
    Ok(())
}

pub fn read(data: &[u8]) -> Result<Dcx> {
    let r = Be(data);
    expect_tag(&r, 0x00, MAGIC)?;
    for (at, want) in [(0x04, 0x10000), (0x08, 0x18), (0x0C, 0x24), (0x10, 0x24)] {
        expect_u32(&r, at, want)?;
    }
    expect_tag(&r, 0x18, b"DCS\0")?;
    expect_tag(&r, 0x24, b"DCP\0")?;
    if r.bytes(0x28, 4)? == b"DFLT" {
        return read_dflt(&r);
    }
    expect_tag(&r, 0x28, b"EDGE")?;
    for (at, want) in [(0x2C, 0x20), (0x30, 0x0900_0000), (0x34, 0x10000), (0x38, 0), (0x3C, 0), (0x40, 0x0010_0100)] {
        expect_u32(&r, at, want)?;
    }
    expect_tag(&r, DCA_AT, b"DCA\0")?;
    expect_tag(&r, 0x4C, b"EgdT")?;
    for (at, want) in [(0x50, 0x0001_0100), (0x54, 0x24), (0x58, 0x10), (0x5C, CHUNK_SIZE), (0x6C, 0x10_0000)] {
        expect_u32(&r, at, want)?;
    }

    let uncompressed_size = r.u32(0x1C)?;
    let compressed_size = r.u32(0x20)?;
    let last_chunk_size = r.u32(0x60)?;
    let egdt_size = r.u32(0x64)?;
    let count = r.u32(0x68)?;
    ensure!(egdt_size == 0x24 + 0x10 * count, "EgdT size {egdt_size:#x} does not fit {count} chunks");
    expect_u32(&r, 0x14, 0x2C + egdt_size)?;
    expect_u32(&r, 0x48, 8 + egdt_size)?;
    let want_count = uncompressed_size.div_ceil(CHUNK_SIZE);
    ensure!(count == want_count, "{count} chunks for {uncompressed_size:#x} bytes, expected {want_count}");
    let want_last = uncompressed_size - (count.max(1) - 1) * CHUNK_SIZE;
    ensure!(last_chunk_size == want_last, "last chunk size {last_chunk_size:#x}, expected {want_last:#x}");
    let end = DCA_AT + 8 + egdt_size as usize + compressed_size as usize;
    ensure!(data.len() >= end, "file is {:#x} bytes, header implies {end:#x}", data.len());

    let mut chunks = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        let at = CHUNKS_AT + 0x10 * i;
        expect_u32(&r, at, 0)?;
        let flag = r.u32(at + 12)?;
        ensure!(flag <= 1, "chunk {i} compression flag {flag}");
        chunks.push(Chunk { offset: r.u32(at + 4)?, size: r.u32(at + 8)?, deflated: flag == 1 });
    }
    Ok(Dcx { variant: Variant::Edge, uncompressed_size, compressed_size, last_chunk_size, trailing_bytes: data.len() - end, chunks })
}

fn read_dflt(r: &Be) -> Result<Dcx> {
    for (at, want) in [(0x14, 0x2C), (0x2C, 0x20), (0x30, 0x0900_0000), (0x34, 0), (0x38, 0), (0x3C, 0), (0x40, 0x0001_0100), (0x48, 8)] {
        expect_u32(r, at, want)?;
    }
    expect_tag(r, DCA_AT, b"DCA\0")?;
    let uncompressed_size = r.u32(0x1C)?;
    let compressed_size = r.u32(0x20)?;
    let end = DFLT_DATA_AT + compressed_size as usize;
    ensure!(r.len() >= end, "file is {:#x} bytes, header implies {end:#x}", r.len());
    Ok(Dcx { variant: Variant::Dflt, uncompressed_size, compressed_size, last_chunk_size: 0, trailing_bytes: r.len() - end, chunks: Vec::new() })
}

impl Dcx {
    fn data_at(&self) -> usize {
        CHUNKS_AT + 0x10 * self.chunks.len()
    }

    /// Expands every chunk and checks each one produces exactly its declared size.
    pub fn decompress(&self, data: &[u8]) -> Result<Vec<u8>> {
        let r = Be(data);
        if self.variant == Variant::Dflt {
            let want = self.uncompressed_size as usize;
            let out = match miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(r.bytes(DFLT_DATA_AT, self.compressed_size as usize)?, want) {
                Ok(v) => v,
                Err(e) => bail!("inflate failed: {e:?}"),
            };
            ensure!(out.len() == want, "expanded to {:#x} bytes, expected {want:#x}", out.len());
            return Ok(out);
        }
        let mut out = Vec::with_capacity(self.uncompressed_size as usize);
        for (i, c) in self.chunks.iter().enumerate() {
            let want = if i + 1 == self.chunks.len() { self.last_chunk_size } else { CHUNK_SIZE } as usize;
            let raw = r.bytes(self.data_at() + c.offset as usize, c.size as usize).with_context(|| format!("chunk {i}"))?;
            let before = out.len();
            if c.deflated {
                match miniz_oxide::inflate::decompress_to_vec_with_limit(raw, want) {
                    Ok(v) => out.extend_from_slice(&v),
                    Err(e) => bail!("chunk {i}: inflate failed: {e:?}"),
                }
            } else {
                out.extend_from_slice(raw);
            }
            ensure!(out.len() - before == want, "chunk {i} expanded to {:#x} bytes, expected {want:#x}", out.len() - before);
        }
        Ok(out)
    }
}

/// Reads and expands a DCX file in one step.
pub fn decompress(data: &[u8]) -> Result<Vec<u8>> {
    read(data)?.decompress(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an EDGE DCX with one stored chunk per 0x10000 bytes, following the module layout.
    fn stored_dcx(plain: &[u8], trailing: &[u8]) -> Vec<u8> {
        let chunks: Vec<&[u8]> = plain.chunks(CHUNK_SIZE as usize).collect();
        let egdt = 0x24 + 0x10 * chunks.len() as u32;
        let mut out = Vec::new();
        let u32s = |out: &mut Vec<u8>, vals: &[u32]| vals.iter().for_each(|v| out.extend(v.to_be_bytes()));
        out.extend(MAGIC);
        u32s(&mut out, &[0x10000, 0x18, 0x24, 0x24, 0x2C + egdt]);
        out.extend(b"DCS\0");
        u32s(&mut out, &[plain.len() as u32, plain.len() as u32]);
        out.extend(b"DCP\0EDGE");
        u32s(&mut out, &[0x20, 0x0900_0000, 0x10000, 0, 0, 0x0010_0100]);
        out.extend(b"DCA\0");
        u32s(&mut out, &[8 + egdt]);
        out.extend(b"EgdT");
        let last = chunks.last().map_or(0, |c| c.len() as u32);
        u32s(&mut out, &[0x0001_0100, 0x24, 0x10, CHUNK_SIZE, last, egdt, chunks.len() as u32, 0x10_0000]);
        let mut offset = 0;
        for c in &chunks {
            u32s(&mut out, &[0, offset, c.len() as u32, 0]);
            offset += c.len() as u32;
        }
        out.extend(plain);
        out.extend(trailing);
        out
    }

    #[test]
    fn stored_chunks_expand_and_trailing_bytes_are_counted() {
        let plain: Vec<u8> = (0..0x18000u32).map(|i| (i * 7) as u8).collect();
        let file = stored_dcx(&plain, &[0xAB; 5]);
        let d = read(&file).unwrap();
        assert_eq!(d.chunks.len(), 2);
        assert_eq!(d.last_chunk_size, 0x8000);
        assert_eq!(d.trailing_bytes, 5);
        assert_eq!(d.decompress(&file).unwrap(), plain);
    }

    #[test]
    fn dflt_expands_one_zlib_stream() {
        let plain: Vec<u8> = (0..0x18000u32).map(|i| (i % 251) as u8).collect();
        let packed = miniz_oxide::deflate::compress_to_vec_zlib(&plain, 6);
        let mut file = Vec::new();
        let u32s = |out: &mut Vec<u8>, vals: &[u32]| vals.iter().for_each(|v| out.extend(v.to_be_bytes()));
        file.extend(MAGIC);
        u32s(&mut file, &[0x10000, 0x18, 0x24, 0x24, 0x2C]);
        file.extend(b"DCS\0");
        u32s(&mut file, &[plain.len() as u32, packed.len() as u32]);
        file.extend(b"DCP\0DFLT");
        u32s(&mut file, &[0x20, 0x0900_0000, 0, 0, 0, 0x0001_0100]);
        file.extend(b"DCA\0");
        u32s(&mut file, &[8]);
        file.extend(&packed);
        let d = read(&file).unwrap();
        assert_eq!((d.variant, d.trailing_bytes), (Variant::Dflt, 0));
        assert_eq!(d.decompress(&file).unwrap(), plain);
    }

    #[test]
    fn truncated_file_is_rejected() {
        let file = stored_dcx(&[1, 2, 3, 4], &[]);
        assert!(read(&file[..file.len() - 1]).is_err());
    }
}
