//! `dbmenu/*.dbp`: debug-menu layouts for the headerless tuning binaries `system/paramlist.xml`
//! pairs them with (`<Bin>` and `<Dbp>`). A big-endian u32 field count, zero padding, then one
//! record per field: u32 type, 8 zero bytes, then value, step, min and max at the type's width.
//! A string table follows: one (label, printf format) pair of NUL-terminated Shift-JIS strings
//! per field, in field order. The paired `.bin` holds each field's value at the type's width,
//! naturally aligned, in field order, and nothing else.

use anyhow::{bail, ensure, Result};

use crate::reader::{sjis, Be};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    S8,
    U8,
    S16,
    U16,
    S32,
    U32,
    F32,
}

impl Kind {
    pub fn from_code(code: u32) -> Option<Self> {
        Some(match code {
            0 => Self::S8,
            1 => Self::U8,
            2 => Self::S16,
            3 => Self::U16,
            4 => Self::S32,
            5 => Self::U32,
            6 => Self::F32,
            _ => return None,
        })
    }

    pub fn width(self) -> usize {
        match self {
            Self::S8 | Self::U8 => 1,
            Self::S16 | Self::U16 => 2,
            Self::S32 | Self::U32 | Self::F32 => 4,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::S8 => "s8",
            Self::U8 => "u8",
            Self::S16 => "s16",
            Self::U16 => "u16",
            Self::S32 => "s32",
            Self::U32 => "u32",
            Self::F32 => "f32",
        }
    }

    fn read(self, r: Be, at: usize) -> Result<f64> {
        Ok(match self {
            Self::S8 => r.i8(at)? as f64,
            Self::U8 => r.u8(at)? as f64,
            Self::S16 => r.i16(at)? as f64,
            Self::U16 => r.u16(at)? as f64,
            Self::S32 => r.i32(at)? as f64,
            Self::U32 => r.u32(at)? as f64,
            Self::F32 => r.f32(at)? as f64,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub kind: Kind,
    /// The value the menu was saved with; the paired `.bin` holds the shipped one.
    pub value: f64,
    pub step: f64,
    pub min: f64,
    pub max: f64,
    pub label: String,
    pub format: String,
}

#[derive(Debug, Clone)]
pub struct Dbp {
    pub fields: Vec<Field>,
    /// Offset of the first record.
    pub records: usize,
    /// Offset of the string table.
    pub labels: usize,
    /// Strings past the last (label, format) pair.
    pub extra_strings: usize,
}

const RECORD_HEAD: usize = 12;

/// A record's type and its value, step, min and max.
type Record = (Kind, [f64; 4]);

/// `count` records from `at`, and the offset past them.
fn records(r: Be, at: usize, count: usize) -> Option<(Vec<Record>, usize)> {
    let (mut pos, mut out) = (at, Vec::with_capacity(count));
    for _ in 0..count {
        let kind = Kind::from_code(r.u32(pos).ok()?)?;
        if r.bytes(pos + 4, 8).ok()? != [0; 8] {
            return None;
        }
        let w = kind.width();
        let mut v = [0.0; 4];
        for (i, x) in v.iter_mut().enumerate() {
            *x = kind.read(r, pos + RECORD_HEAD + i * w).ok()?;
        }
        out.push((kind, v));
        pos += RECORD_HEAD + 4 * w;
    }
    Some((out, pos))
}

/// The records start after an unexplained gap; it is the first 4-aligned offset from which
/// `count` well-formed records end where a (label, format) pair with a `%` format begins.
pub fn read(data: &[u8]) -> Result<Dbp> {
    let r = Be(data);
    let count = r.u32(0)? as usize;
    ensure!(count > 0 && count * RECORD_HEAD < data.len(), "field count {count} does not fit a {}-byte file", data.len());
    for at in (4..data.len()).step_by(4) {
        let Some((recs, end)) = records(r, at, count) else { continue };
        let strings: Vec<&[u8]> = data[end..].split(|&b| b == 0).collect();
        if !strings.get(1).is_some_and(|f| f.starts_with(b"%")) || strings.len() < 2 * count {
            continue;
        }
        let used = 2 * count;
        let extra_strings = strings[used..].iter().filter(|s| !s.is_empty()).count();
        let fields = recs
            .into_iter()
            .enumerate()
            .map(|(i, (kind, [value, step, min, max]))| Field {
                kind,
                value,
                step,
                min,
                max,
                label: sjis(strings[2 * i]),
                format: sjis(strings[2 * i + 1]),
            })
            .collect();
        return Ok(Dbp { fields, records: at, labels: end, extra_strings });
    }
    bail!("no offset holds {count} records followed by a label table")
}

impl Dbp {
    /// Bytes the paired `.bin` must hold.
    pub fn bin_size(&self) -> usize {
        self.fields.iter().fold(0, |at, f| at.next_multiple_of(f.kind.width()) + f.kind.width())
    }

    /// Every field's value in the paired `.bin`, in field order. Some retail binaries run past
    /// their layout (fields added after the menu was saved, or padding); the caller sees that as
    /// `bin.len() - bin_size()`.
    pub fn values(&self, bin: &[u8]) -> Result<Vec<f64>> {
        ensure!(bin.len() >= self.bin_size(), "{} bytes, the layout packs {}", bin.len(), self.bin_size());
        let r = Be(bin);
        let mut at = 0usize;
        self.fields
            .iter()
            .map(|f| {
                at = at.next_multiple_of(f.kind.width());
                let v = f.kind.read(r, at);
                at += f.kind.width();
                v
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(code: u32, values: &[u8]) -> Vec<u8> {
        let mut d = code.to_be_bytes().to_vec();
        d.extend_from_slice(&[0; 8]);
        d.extend_from_slice(values);
        d
    }

    #[test]
    fn finds_records_and_pairs_labels() {
        let mut d = 2u32.to_be_bytes().to_vec();
        d.extend_from_slice(&[0; 4]);
        d.extend(record(1, &[7, 1, 0, 9]));
        let floats: Vec<u8> = [1.5f32, 0.5, 0.0, 10.0].iter().flat_map(|v| v.to_be_bytes()).collect();
        d.extend(record(6, &floats));
        d.extend_from_slice(b"speed\0%d\0gravity\0%.1f\0");
        let dbp = read(&d).unwrap();
        assert_eq!(dbp.records, 8);
        assert_eq!(dbp.fields.len(), 2);
        assert_eq!((dbp.fields[0].kind, dbp.fields[0].value, dbp.fields[0].max), (Kind::U8, 7.0, 9.0));
        assert_eq!((dbp.fields[1].label.as_str(), dbp.fields[1].format.as_str(), dbp.fields[1].value), ("gravity", "%.1f", 1.5));
        assert_eq!(dbp.bin_size(), 8);
        let mut bin = vec![3, 0, 0, 0];
        bin.extend_from_slice(&2.5f32.to_be_bytes());
        assert_eq!(dbp.values(&bin).unwrap(), [3.0, 2.5]);
        assert!(dbp.values(&bin[..7]).is_err());
        bin.extend_from_slice(&[0; 4]);
        assert_eq!(dbp.values(&bin).unwrap(), [3.0, 2.5]);
    }
}
