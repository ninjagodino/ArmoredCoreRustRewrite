//! BND3 binder: a flat list of named files.
//!
//! Header (0x20 bytes): `"BND3", fixstr[8] version, u8 format, u8 big_endian, u8 bit_big_endian,
//! u8 0, u32 file_count, u32 headers_end, u32 unk18, u32 unk1c`, then `file_count` entries of
//! `u8 flags, u8[3] 0, u32 stored_size, u32 offset, [u32 id], [u32 name_offset], [u32 size]`,
//! where the bracketed fields exist when the format has IDs, names, and compression respectively.
//! Names are NUL-terminated Shift-JIS. Entries with flag bit 0 hold a zlib stream that expands to
//! `size` bytes; all other entries are stored as-is.

use std::borrow::Cow;

use anyhow::{bail, ensure, Context, Result};
use serde::Serialize;

use crate::reader::Be;

pub const MAGIC: &[u8; 4] = b"BND3";

pub const BIG_ENDIAN: u8 = 0x01;
pub const IDS: u8 = 0x02;
pub const NAMES1: u8 = 0x04;
pub const NAMES2: u8 = 0x08;
pub const LONG_OFFSETS: u8 = 0x10;
pub const COMPRESSION: u8 = 0x20;

pub const FLAG_ZLIB: u8 = 0x01;

#[derive(Debug, Clone, Serialize)]
pub struct Bnd3 {
    pub version: String,
    pub version_raw: [u8; 8],
    /// Format byte as stored.
    pub raw_format: u8,
    /// Format byte in canonical bit order (see [`canonical_format`]).
    pub format: u8,
    pub big_endian: u8,
    pub bit_big_endian: u8,
    pub headers_end: u32,
    pub unk18: u32,
    pub unk1c: u32,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub flags: u8,
    pub stored_size: u32,
    pub offset: u32,
    pub id: Option<i32>,
    pub name_offset: Option<u32>,
    pub name: Option<String>,
    pub size: Option<u32>,
}

pub fn is_bnd3(data: &[u8]) -> bool {
    data.starts_with(MAGIC)
}

/// Format bytes are stored bit-reversed unless `bit_big_endian` is set or the raw byte already
/// reads as canonical (bit 0 set, bit 7 clear). Matches every binder on the disc: raw 0x54 with
/// `bit_big_endian == 0` is 0x2A (IDs | Names2 | Compression).
pub fn canonical_format(raw: u8, bit_big_endian: bool) -> u8 {
    if bit_big_endian || (raw & 0x01 != 0 && raw & 0x80 == 0) {
        raw
    } else {
        raw.reverse_bits()
    }
}

fn has(format: u8, bit: u8) -> bool {
    format & bit != 0
}

pub fn entry_size(format: u8) -> usize {
    0x0C + 4 * [IDS, NAMES1 | NAMES2, COMPRESSION].iter().filter(|&&b| has(format, b)).count()
}

pub fn read(data: &[u8]) -> Result<Bnd3> {
    let r = Be(data);
    ensure!(r.bytes(0, 4)? == MAGIC, "not a BND3");
    let raw_format = r.u8(0x0C)?;
    let big_endian = r.u8(0x0D)?;
    let bit_big_endian = r.u8(0x0E)?;
    ensure!(r.u8(0x0F)? == 0, "BND3 byte 0x0F is {:#x}, expected 0", r.u8(0x0F)?);
    let format = canonical_format(raw_format, bit_big_endian != 0);
    ensure!(big_endian != 0 || has(format, BIG_ENDIAN), "little-endian BND3 (format {raw_format:#04x}) is not on the ACVD disc");
    ensure!(!has(format, LONG_OFFSETS), "BND3 format {raw_format:#04x} has 64-bit offsets, not on the ACVD disc");

    let count = r.u32(0x10)? as usize;
    let stride = entry_size(format);
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let mut at = 0x20 + i * stride;
        let flags = r.u8(at)?;
        ensure!(r.bytes(at + 1, 3)? == [0, 0, 0], "entry {i} has nonzero padding after flags");
        let stored_size = r.u32(at + 4)?;
        let offset = r.u32(at + 8)?;
        at += 0x0C;
        let mut next = |present: bool| -> Result<Option<u32>> {
            if !present {
                return Ok(None);
            }
            let v = r.u32(at)?;
            at += 4;
            Ok(Some(v))
        };
        let id = next(has(format, IDS))?.map(|v| v as i32);
        let name_offset = next(has(format, NAMES1 | NAMES2))?;
        let size = next(has(format, COMPRESSION))?;
        let name = name_offset.map(|o| r.cstr_sjis(o as usize)).transpose().with_context(|| format!("entry {i} name"))?;
        r.bytes(offset as usize, stored_size as usize).with_context(|| format!("entry {i} data"))?;
        entries.push(Entry { flags, stored_size, offset, id, name_offset, name, size });
    }

    Ok(Bnd3 {
        version: r.fixstr(0x04, 8)?,
        version_raw: r.bytes(0x04, 8)?.try_into().expect("8 bytes"),
        raw_format,
        format,
        big_endian,
        bit_big_endian,
        headers_end: r.u32(0x14)?,
        unk18: r.u32(0x18)?,
        unk1c: r.u32(0x1C)?,
        entries,
    })
}

impl Entry {
    pub fn is_zlib(&self) -> bool {
        self.flags & FLAG_ZLIB != 0
    }

    pub fn stored<'a>(&self, binder: &'a [u8]) -> Result<&'a [u8]> {
        Be(binder).bytes(self.offset as usize, self.stored_size as usize)
    }

    /// Entry contents, inflated when the entry is zlib-compressed. A compressed entry must expand
    /// to exactly its declared size.
    pub fn contents<'a>(&self, binder: &'a [u8]) -> Result<Cow<'a, [u8]>> {
        let raw = self.stored(binder)?;
        if !self.is_zlib() {
            return Ok(Cow::Borrowed(raw));
        }
        let want = self.size.context("zlib entry in a binder without a size field")? as usize;
        let out = match miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(raw, want) {
            Ok(v) => v,
            Err(e) => bail!("zlib inflate failed: {e:?}"),
        };
        ensure!(out.len() == want, "zlib entry expanded to {:#x} bytes, expected {want:#x}", out.len());
        Ok(Cow::Owned(out))
    }
}

impl Bnd3 {
    /// Rebuilds the binder from the parsed header, entry table and names, copying each entry's
    /// stored bytes into place, and returns the first offset where the rebuild differs from
    /// `binder`. `None` means every byte outside entry data is explained (gaps are zero).
    pub fn first_unexplained_byte(&self, binder: &[u8]) -> Result<Option<usize>> {
        let mut out = vec![0u8; binder.len()];
        let put = |out: &mut Vec<u8>, at: usize, b: &[u8]| -> Result<()> {
            let dst = out.get_mut(at..at + b.len()).with_context(|| format!("rebuild write at {at:#x} past end"))?;
            dst.copy_from_slice(b);
            Ok(())
        };
        put(&mut out, 0, MAGIC)?;
        put(&mut out, 4, &self.version_raw)?;
        put(&mut out, 0x0C, &[self.raw_format, self.big_endian, self.bit_big_endian, 0])?;
        for (at, v) in [(0x10, self.entries.len() as u32), (0x14, self.headers_end), (0x18, self.unk18), (0x1C, self.unk1c)] {
            put(&mut out, at, &v.to_be_bytes())?;
        }
        let stride = entry_size(self.format);
        for (i, e) in self.entries.iter().enumerate() {
            let mut rec = vec![e.flags, 0, 0, 0];
            rec.extend(e.stored_size.to_be_bytes());
            rec.extend(e.offset.to_be_bytes());
            for v in [e.id.map(|v| v as u32), e.name_offset, e.size].into_iter().flatten() {
                rec.extend(v.to_be_bytes());
            }
            ensure!(rec.len() == stride, "entry {i} rebuilt to {} bytes, stride {stride}", rec.len());
            put(&mut out, 0x20 + i * stride, &rec)?;
            if let (Some(at), Some(name)) = (e.name_offset, &e.name) {
                let (enc, _, lossy) = encoding_rs::SHIFT_JIS.encode(name);
                ensure!(!lossy, "entry {i} name does not re-encode to Shift-JIS");
                put(&mut out, at as usize, &enc)?;
                put(&mut out, at as usize + enc.len(), &[0])?;
            }
            put(&mut out, e.offset as usize, e.stored(binder)?)?;
        }
        Ok(out.iter().zip(binder).position(|(a, b)| a != b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two-entry binder in the disc's common layout (raw format 0x26, 0x18-byte entries):
    /// entry 0 stored, entry 1 zlib-compressed.
    fn sample() -> (Vec<u8>, Vec<u8>) {
        let plain = b"hello hello hello hello".to_vec();
        let packed = miniz_oxide::deflate::compress_to_vec_zlib(&plain, 6);
        let names = [b"a.bin\0".as_slice(), b"b.bin\0"];
        let table_end = 0x20 + 2 * 0x18;
        let names_end = table_end + names.iter().map(|n| n.len()).sum::<usize>();
        let data0 = names_end.next_multiple_of(0x10);
        let data1 = data0 + 4;
        let mut out = Vec::new();
        out.extend(MAGIC);
        out.extend(b"JP100\0\0\0");
        out.extend([0x26, 1, 1, 0]);
        for v in [2u32, names_end as u32, 0, 0] {
            out.extend(v.to_be_bytes());
        }
        let entries = [(0x02u8, 4u32, data0, 1u32, table_end, 4u32), (0x03, packed.len() as u32, data1, 2, table_end + 6, plain.len() as u32)];
        for (flags, stored, offset, id, name, size) in entries {
            out.extend([flags, 0, 0, 0]);
            for v in [stored, offset as u32, id, name as u32, size] {
                out.extend(v.to_be_bytes());
            }
        }
        names.iter().for_each(|n| out.extend(*n));
        out.resize(data0, 0);
        out.extend(b"RAW!");
        out.extend(&packed);
        (out, plain)
    }

    #[test]
    fn reads_entries_and_inflates_zlib() {
        let (file, plain) = sample();
        let b = read(&file).unwrap();
        assert_eq!(b.version, "JP100");
        assert_eq!(entry_size(b.format), 0x18);
        assert_eq!(b.entries[0].name.as_deref(), Some("a.bin"));
        assert_eq!(&*b.entries[0].contents(&file).unwrap(), b"RAW!");
        assert!(b.entries[1].is_zlib());
        assert_eq!(&*b.entries[1].contents(&file).unwrap(), plain.as_slice());
        assert_eq!(b.first_unexplained_byte(&file).unwrap(), None);
    }

    #[test]
    fn stray_bytes_are_unexplained() {
        let (mut file, _) = sample();
        let gap = 0x20 + 2 * 0x18 + 12;
        file[gap] = 0x55;
        assert_eq!(read(&file).unwrap().first_unexplained_byte(&file).unwrap(), Some(gap));
    }

    #[test]
    fn format_bit_order_matches_disc() {
        assert_eq!(canonical_format(0x26, true), 0x26);
        assert_eq!(canonical_format(0x67, false), 0x67);
        assert_eq!(canonical_format(0x54, false), 0x2A);
        assert_eq!(entry_size(0x06), 0x14);
    }
}
