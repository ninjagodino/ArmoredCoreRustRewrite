//! BHD5: the hashed index of a `.bdt` archive (360 `bind/dvdbnd5_layer{0,1}.bhd`).
//!
//! Big-endian. Header (0x18 bytes): `"BHD5", u32 0, u32 1, u32 file_size, u32 bucket_count,
//! u32 buckets_offset`; buckets are `u32 count, u32 entries_offset`; entries are 16 bytes
//! `u32 path_hash, u32 size, u64 offset` into the `.bdt`. The archive stores no names: a path is
//! found by its [`path_hash`]. Entries are stored as-is (DCX files stay DCX).

use anyhow::{ensure, Context, Result};

use crate::reader::Be;

pub const MAGIC: &[u8; 4] = b"BHD5";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub hash: u32,
    pub size: u32,
    pub offset: u64,
}

pub fn is_bhd5(data: &[u8]) -> bool {
    data.starts_with(MAGIC)
}

/// Hash of a disc path: lowercase, `/` separators, a leading `/` (added when missing), then
/// `h = h * 37 + byte` in 32 bits.
pub fn path_hash(path: &str) -> u32 {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    let rooted = if path.starts_with('/') { path } else { format!("/{path}") };
    rooted.bytes().fold(0u32, |h, c| h.wrapping_mul(37).wrapping_add(c as u32))
}

pub fn read(data: &[u8]) -> Result<Vec<Entry>> {
    let r = Be(data);
    ensure!(r.bytes(0, 4)? == MAGIC, "not a BHD5");
    ensure!(r.u32(4)? == 0, "BHD5 word 0x04 is {:#x}, expected 0 (big-endian)", r.u32(4)?);
    ensure!(r.u32(0x0C)? as usize == data.len(), "BHD5 declares {:#x} bytes, file has {:#x}", r.u32(0x0C)?, data.len());
    let (buckets, at) = (r.u32(0x10)? as usize, r.u32(0x14)? as usize);
    let mut out = Vec::new();
    for b in 0..buckets {
        let (count, entries) = (r.u32(at + 8 * b)? as usize, r.u32(at + 8 * b + 4)? as usize);
        for i in 0..count {
            let e = entries + 16 * i;
            let read = || -> Result<Entry> { Ok(Entry { hash: r.u32(e)?, size: r.u32(e + 4)?, offset: (r.u32(e + 8)? as u64) << 32 | r.u32(e + 12)? as u64 }) };
            out.push(read().with_context(|| format!("bucket {b} entry {i}"))?);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_matches_disc_entries() {
        // Sizes checked against the 360 archive: color5001.bin 856 bytes, coloringset.bin 2121.
        assert_eq!(path_hash("/param/accolor/color5001.bin"), path_hash("PARAM\\ACColor\\color5001.bin"));
        assert_eq!(path_hash("param/thumbnail/arm.bin"), 0x0000a741);
        assert_eq!(path_hash("/model/ac/parts/hand/hl3423/hl3423_m.bnd.dcx"), 0x000945ab);
    }

    #[test]
    fn reads_buckets() {
        let mut d = Vec::new();
        d.extend(MAGIC);
        for v in [0u32, 1, 0, 1, 0x18] {
            d.extend(v.to_be_bytes());
        }
        d.extend(1u32.to_be_bytes());
        d.extend(0x20u32.to_be_bytes());
        for v in [0xabcdu32, 7, 1, 0x10] {
            d.extend(v.to_be_bytes());
        }
        let len = d.len() as u32;
        d[0x0C..0x10].copy_from_slice(&len.to_be_bytes());
        assert_eq!(read(&d).unwrap(), vec![Entry { hash: 0xabcd, size: 7, offset: 0x1_0000_0010 }]);
    }
}
