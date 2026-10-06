//! The unpacked Xbox 360 image `ACV2.pe`: a memory image (file offset = address - base), so
//! section virtual ranges are used directly and anything past the file end reads as zero.

use std::path::Path;

use anyhow::{bail, Context, Result};

pub const BASE: u32 = 0x8200_0000;
pub const PDATA: u32 = 0x8225_c200;
pub const PDATA_SIZE: u32 = 0x99a38;

pub struct Section {
    pub name: String,
    pub lo: u32,
    pub hi: u32,
    pub code: bool,
}

pub struct Image {
    pub bytes: Vec<u8>,
    pub sections: Vec<Section>,
    /// `.pdata` functions, sorted by start: (start, end).
    pub pdata: Vec<(u32, u32)>,
}

impl Image {
    pub fn open(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        if bytes.get(..2) != Some(b"MZ") {
            bail!("{} is not a PE image", path.display());
        }
        let le32 = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
        let le16 = |o: usize| u16::from_le_bytes(bytes[o..o + 2].try_into().unwrap());
        let pe = le32(0x3c) as usize;
        let count = le16(pe + 6) as usize;
        let mut o = pe + 24 + le16(pe + 20) as usize;
        let mut sections = Vec::new();
        for _ in 0..count {
            let name = String::from_utf8_lossy(&bytes[o..o + 8]).trim_end_matches('\0').to_string();
            let (vsize, va, flags) = (le32(o + 8), le32(o + 12), le32(o + 36));
            sections.push(Section { name, lo: BASE + va, hi: BASE + va + vsize, code: flags & 0x20 != 0 });
            o += 40;
        }
        let mut img = Image { bytes, sections, pdata: Vec::new() };
        for a in (PDATA..PDATA + PDATA_SIZE).step_by(8) {
            let (begin, packed) = (img.word(a), img.word(a + 4));
            let words = (packed >> 8) & 0x3f_ffff;
            if begin != 0 && words != 0 {
                img.pdata.push((begin, begin + words * 4));
            }
        }
        img.pdata.sort_unstable();
        Ok(img)
    }

    pub fn word(&self, a: u32) -> u32 {
        let o = a.wrapping_sub(BASE) as usize;
        self.bytes.get(o..o + 4).map_or(0, |b| u32::from_be_bytes(b.try_into().unwrap()))
    }

    pub fn section(&self, a: u32) -> Option<&Section> {
        self.sections.iter().find(|s| s.lo <= a && a < s.hi)
    }

    pub fn is_code(&self, a: u32) -> bool {
        self.section(a).is_some_and(|s| s.code)
    }

    /// Data sections that can hold pointer tables (`.rdata`, `.data`).
    pub fn data_ranges(&self) -> Vec<(u32, u32)> {
        self.sections
            .iter()
            .filter(|s| s.name == ".rdata" || s.name == ".data")
            .map(|s| (s.lo, s.hi.min(BASE + self.bytes.len() as u32)))
            .collect()
    }

    /// A NUL-terminated printable ASCII string of at least 2 characters at `a`.
    pub fn cstr(&self, a: u32) -> Option<String> {
        let o = a.checked_sub(BASE)? as usize;
        let tail = self.bytes.get(o..)?;
        let end = tail.iter().take(256).position(|&b| b == 0)?;
        let s = &tail[..end];
        (s.len() >= 2 && s.iter().all(|&b| (0x20..0x7f).contains(&b) || b == b'\t' || b == b'\n'))
            .then(|| String::from_utf8_lossy(s).into_owned())
    }
}

/// Index into a sorted (start, end) list of the range containing `a`.
pub fn containing(funcs: &[(u32, u32)], a: u32) -> Option<usize> {
    let i = funcs.partition_point(|f| f.0 <= a).checked_sub(1)?;
    (a < funcs[i].1).then_some(i)
}
