//! `param/acvparts.bin`: the AC parts catalogue. A zero u32, then one big-endian u16 record
//! count per category; records follow back to back, category by category, each opening with
//! its part id and the model id it draws. Record sizes are not stored: the caller supplies
//! them (from `sheets/ac_part_categories.csv`). The retail file carries more, unindexed data
//! past the last counted record; `Parts::tail` reports where it starts.

use anyhow::{bail, ensure, Result};

use crate::reader::Be;

pub const CATEGORIES: usize = 24;
pub const HEADER_SIZE: usize = 4 + CATEGORIES * 2;

/// Record size in bytes for each `acvparts.bin` / `acvenemyparts.bin` category
/// (360 loader `0x824a0430`, called for both files by `0x824a1498`).
pub fn record_size(category: usize) -> Option<usize> {
    Some(match category {
        0 | 1 | 5 => 0x144,
        2 | 4 | 7 | 8 => 0x148,
        3 => 0x174,
        6 => 0x150,
        9 => 0x16c,
        10 => 0x1a0,
        11 => 0x1ac,
        12 => 0x1c4,
        13 => 0x134,
        14..=23 => 0x13c,
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy)]
pub struct Record<'a> {
    pub category: usize,
    pub index: usize,
    pub offset: usize,
    pub id: u16,
    pub model_id: u16,
    pub data: &'a [u8],
}

#[derive(Debug, Clone)]
pub struct Parts<'a> {
    pub counts: [u16; CATEGORIES],
    pub records: Vec<Record<'a>>,
    /// Offset just past the last record.
    pub end: usize,
    /// First non-zero byte past `end`, if any.
    pub tail: Option<usize>,
}

pub fn read(data: &[u8], record_size: impl Fn(usize) -> Option<usize>) -> Result<Parts<'_>> {
    let r = Be(data);
    ensure!(r.u32(0)? == 0, "header does not open with a zero u32");
    let mut counts = [0u16; CATEGORIES];
    for (i, c) in counts.iter_mut().enumerate() {
        *c = r.u16(4 + i * 2)?;
    }
    let (mut at, mut records) = (HEADER_SIZE, Vec::new());
    for (category, &count) in counts.iter().enumerate().filter(|(_, &c)| c > 0) {
        let Some(size) = record_size(category) else {
            bail!("category {category} has {count} records but no record size")
        };
        ensure!(
            size >= 4,
            "category {category} record size {size} is shorter than its ids"
        );
        for index in 0..count as usize {
            let rec = r.bytes(at, size)?;
            records.push(Record {
                category,
                index,
                offset: at,
                id: r.u16(at)?,
                model_id: r.u16(at + 2)?,
                data: rec,
            });
            at += size;
        }
    }
    let tail = data[at..].iter().position(|&b| b != 0).map(|i| at + i);
    Ok(Parts {
        counts,
        records,
        end: at,
        tail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(counts: &[(usize, u16)], records: &[&[u8]]) -> Vec<u8> {
        let mut d = vec![0u8; HEADER_SIZE];
        for &(c, n) in counts {
            d[4 + c * 2..6 + c * 2].copy_from_slice(&n.to_be_bytes());
        }
        records.iter().for_each(|r| d.extend_from_slice(r));
        d
    }

    #[test]
    fn walks_categories_in_order() {
        let d = file(
            &[(0, 1), (2, 2)],
            &[&[0, 1, 0, 1, 9, 9], &[0, 5, 0, 6], &[0, 7, 0, 7], &[0, 0]],
        );
        let p = read(&d, |c| [Some(6), None, Some(4)][c]).unwrap();
        let ids: Vec<_> = p
            .records
            .iter()
            .map(|r| (r.category, r.id, r.model_id))
            .collect();
        assert_eq!(ids, [(0, 1, 1), (2, 5, 6), (2, 7, 7)]);
        assert_eq!((p.end, p.tail), (d.len() - 2, None));
    }

    #[test]
    fn rejects_unknown_size_and_reports_tail() {
        let mut d = file(&[(1, 1)], &[&[0, 1, 0, 1]]);
        assert!(read(&d, |_| None).is_err());
        d.extend_from_slice(&[0, 3]);
        assert_eq!(read(&d, |_| Some(4)).unwrap().tail, Some(d.len() - 1));
    }

    #[test]
    fn reads_enemy_parts_catalogue_from_disc() {
        let root = crate::vfs::repo_root();
        let Ok(disc) = crate::vfs::Disc::open(&crate::vfs::default_disc(&root)) else {
            return;
        };
        let data = disc.read("param/enemy/parts/acvenemyparts.bin").unwrap();
        let parts = read(&data, record_size).unwrap();
        assert!(parts.records.len() > 100);
        // Verify AC TEST enemy weapon IDs exist in category 10 with valid firing fields.
        // (`8990` is DEF-MT's shield bash placeholder with `bullet_id == 0`.)
        for wanted in [8000u16, 8030, 8031, 8060, 8120, 8230, 8231, 8990] {
            let rec = parts
                .records
                .iter()
                .find(|r| r.category == 10 && r.id == wanted)
                .unwrap_or_else(|| panic!("missing enemy weapon {wanted} in acvenemyparts.bin"));
            let be = Be(rec.data);
            let bullet_id = be.u32(316).unwrap();
            if wanted != 8990 {
                assert!(bullet_id > 0, "enemy weapon {wanted} missing bullet_id");
            }
        }
    }
}
