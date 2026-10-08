//! FMG message bank: localized strings keyed by message id (`lang/<lang>/text/**/*.fmg`).
//!
//! Header (0x1C bytes): `u8 0, u8 big_endian (1), u8 version (0), u8 0, u32 file_size,
//! u8 encoding (1 = UTF-16BE, 0 = Shift-JIS; the 15 `partsname_*.fmg` banks are 0),
//! u8 0xFF, u16 0, u32 group_count, u32 string_count,
//! u32 string_offsets_offset (= 0x1C + 12 * group_count), u32 0`, then groups of
//! `u32 first_string, i32 first_id, i32 last_id` (ids `first_id..=last_id` take consecutive
//! strings), then one `u32` per string: offset of a NUL-terminated string, 0 for none.
//! Every bank on the disc matches this layout, with groups covering the strings in order.

use anyhow::{ensure, Result};
use serde::Serialize;

use crate::reader::{sjis, until_nul, utf16be, Be};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Encoding {
    Utf16Be,
    ShiftJis,
}

#[derive(Debug, Clone, Serialize)]
pub struct Fmg {
    pub encoding: Encoding,
    /// `(id, text)` in file order; `None` for ids whose offset is 0.
    pub entries: Vec<(i32, Option<String>)>,
}

pub fn read(data: &[u8]) -> Result<Fmg> {
    let r = Be(data);
    ensure!(
        r.u8(0)? == 0 && r.u8(1)? == 1 && r.u8(2)? == 0,
        "not a big-endian version-0 FMG"
    );
    ensure!(
        r.u32(4)? as usize == data.len(),
        "FMG size field {:#x} != file size {:#x}",
        r.u32(4)?,
        data.len()
    );
    let encoding = match r.u8(8)? {
        0 => Encoding::ShiftJis,
        1 => Encoding::Utf16Be,
        e => anyhow::bail!("FMG encoding {e}"),
    };
    let groups = r.u32(0x0C)? as usize;
    let strings = r.u32(0x10)? as usize;
    let offsets = r.u32(0x14)? as usize;
    ensure!(
        offsets == 0x1C + 12 * groups,
        "string offsets at {offsets:#x}, expected after {groups} groups"
    );
    let mut entries = Vec::with_capacity(strings);
    for g in 0..groups {
        let at = 0x1C + 12 * g;
        let (first, lo, hi) = (r.u32(at)? as usize, r.i32(at + 4)?, r.i32(at + 8)?);
        ensure!(
            first == entries.len() && lo <= hi,
            "group {g} ({first}, {lo}..={hi}) out of order"
        );
        for (k, id) in (lo..=hi).enumerate() {
            let i = first + k;
            ensure!(i < strings, "group {g} runs past {strings} strings");
            let o = r.u32(offsets + 4 * i)? as usize;
            let text = if o == 0 {
                None
            } else {
                let tail = r.bytes(o, data.len() - o)?;
                Some(match encoding {
                    Encoding::Utf16Be => utf16be(tail),
                    Encoding::ShiftJis => sjis(until_nul(tail)),
                })
            };
            entries.push((id, text));
        }
    }
    ensure!(
        entries.len() == strings,
        "groups cover {} of {strings} strings",
        entries.len()
    );
    Ok(Fmg { encoding, entries })
}

impl Fmg {
    pub fn get(&self, id: i32) -> Option<&str> {
        self.entries
            .iter()
            .find(|(i, _)| *i == id)
            .and_then(|(_, t)| t.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16()
            .chain([0])
            .flat_map(u16::to_be_bytes)
            .collect()
    }

    #[test]
    fn reads_groups_and_null_entries() {
        let groups: [(u32, i32, i32); 2] = [(0, 10, 11), (2, 2593, 2593)];
        let texts = [Some("Add-on"), None, Some("Time Left")];
        let offsets_at = 0x1C + 12 * groups.len();
        let mut body = Vec::new();
        let mut offsets = Vec::new();
        let mut at = offsets_at + 4 * texts.len();
        for t in texts {
            match t {
                Some(t) => {
                    offsets.push(at as u32);
                    let b = utf16(t);
                    at += b.len();
                    body.extend(b);
                }
                None => offsets.push(0),
            }
        }
        let mut out = vec![0, 1, 0, 0];
        out.extend(((at) as u32).to_be_bytes());
        out.extend([1, 0xFF, 0, 0]);
        for v in [
            groups.len() as u32,
            texts.len() as u32,
            offsets_at as u32,
            0,
        ] {
            out.extend(v.to_be_bytes());
        }
        for (f, lo, hi) in groups {
            out.extend(f.to_be_bytes());
            out.extend(lo.to_be_bytes());
            out.extend(hi.to_be_bytes());
        }
        offsets.iter().for_each(|o| out.extend(o.to_be_bytes()));
        out.extend(body);
        let f = read(&out).unwrap();
        assert_eq!(f.entries.len(), 3);
        assert_eq!(f.get(10), Some("Add-on"));
        assert_eq!(f.get(11), None);
        assert_eq!(f.get(2593), Some("Time Left"));
        assert_eq!(f.encoding, Encoding::Utf16Be);
    }

    #[test]
    fn reads_shift_jis_partsname_style() {
        let text = b"HA-202\0";
        let offsets_at = 0x1C + 12;
        let str_at = offsets_at + 4;
        let mut out = vec![0, 1, 0, 0];
        out.extend(((str_at + text.len()) as u32).to_be_bytes());
        out.extend([0, 0xFF, 0, 0]);
        for v in [1u32, 1, offsets_at as u32, 0] {
            out.extend(v.to_be_bytes());
        }
        for v in [0u32, 110, 110] {
            out.extend(v.to_be_bytes());
        }
        out.extend((str_at as u32).to_be_bytes());
        out.extend(text);
        let f = read(&out).unwrap();
        assert_eq!(f.encoding, Encoding::ShiftJis);
        assert_eq!(f.get(110), Some("HA-202"));
    }

    #[test]
    fn disc_banks() {
        let Some(disc) = crate::vfs::test_disc() else {
            return;
        };
        let mut n = 0;
        for file in disc.files() {
            let lower = file.to_ascii_lowercase();
            if !(lower.starts_with("lang/en/text/") || lower.starts_with("lang/jp/text/"))
                || !lower.ends_with(".fmg")
            {
                continue;
            }
            let f =
                read(&disc.asset(&file).unwrap()).unwrap_or_else(|err| panic!("{file}: {err:#}"));
            n += 1;
            if lower
                .rsplit('/')
                .next()
                .is_some_and(|n| n.starts_with("partsname"))
            {
                assert_eq!(f.encoding, Encoding::ShiftJis, "{file}");
            }
        }
        assert!(n > 100, "{n} FMGs");
        let names = read(&disc.asset("lang/en/text/partsname_en.fmg").unwrap()).unwrap();
        assert_eq!(names.get(110), Some("HA-202"));
        let menu = read(&disc.asset("lang/en/text/menu/menu.fmg").unwrap()).unwrap();
        assert_eq!(menu.get(204), Some("PRESS START"));
    }
}
