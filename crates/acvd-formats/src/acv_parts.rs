//! `param/acvparts.bin`: the AC parts catalogue. A zero u32, then one big-endian u16 record
//! count per category; records follow back to back, category by category, each opening with
//! its part id and the model id it draws. Record sizes are not stored: the caller supplies
//! them (from `sheets/ac_part_categories.csv`). The retail file carries more, unindexed data
//! past the last counted record; `Parts::tail` reports where it starts.

use anyhow::{bail, ensure, Result};

use crate::reader::Be;

pub const CATEGORIES: usize = 24;
pub const HEADER_SIZE: usize = 4 + CATEGORIES * 2;

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
        let Some(size) = record_size(category) else { bail!("category {category} has {count} records but no record size") };
        ensure!(size >= 4, "category {category} record size {size} is shorter than its ids");
        for index in 0..count as usize {
            let rec = r.bytes(at, size)?;
            records.push(Record { category, index, offset: at, id: r.u16(at)?, model_id: r.u16(at + 2)?, data: rec });
            at += size;
        }
    }
    let tail = data[at..].iter().position(|&b| b != 0).map(|i| at + i);
    Ok(Parts { counts, records, end: at, tail })
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
        let d = file(&[(0, 1), (2, 2)], &[&[0, 1, 0, 1, 9, 9], &[0, 5, 0, 6], &[0, 7, 0, 7], &[0, 0]]);
        let p = read(&d, |c| [Some(6), None, Some(4)][c]).unwrap();
        let ids: Vec<_> = p.records.iter().map(|r| (r.category, r.id, r.model_id)).collect();
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
}
