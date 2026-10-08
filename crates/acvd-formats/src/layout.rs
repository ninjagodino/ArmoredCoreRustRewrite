//! Byte placement and decoding of PARAM row columns.
//!
//! Bitfields pack into a unit of their primitive's width. A new unit starts when the previous
//! field was not a bitfield, the primitive changes, or the unit would overflow. Bits are taken
//! least-significant first from the big-endian unit value (SoulsFormats convention).

use anyhow::{bail, ensure, Result};
use serde::{Serialize, Serializer};

use crate::reader::{sjis, until_nul, utf16be, Be};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Prim {
    U8,
    S8,
    U16,
    S16,
    U32,
    S32,
    F32,
    Angle32,
    Dummy8,
    FixStr,
    FixStrW,
}

impl Prim {
    pub const ALL: [Prim; 11] = [
        Prim::U8,
        Prim::S8,
        Prim::U16,
        Prim::S16,
        Prim::U32,
        Prim::S32,
        Prim::F32,
        Prim::Angle32,
        Prim::Dummy8,
        Prim::FixStr,
        Prim::FixStrW,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Prim::U8 => "u8",
            Prim::S8 => "s8",
            Prim::U16 => "u16",
            Prim::S16 => "s16",
            Prim::U32 => "u32",
            Prim::S32 => "s32",
            Prim::F32 => "f32",
            Prim::Angle32 => "angle32",
            Prim::Dummy8 => "dummy8",
            Prim::FixStr => "fixstr",
            Prim::FixStrW => "fixstrW",
        }
    }

    pub fn parse(s: &str) -> Option<Prim> {
        Prim::ALL.into_iter().find(|p| p.name() == s)
    }

    /// Bytes per element (per character for strings).
    pub fn size(self) -> usize {
        match self {
            Prim::U8 | Prim::S8 | Prim::Dummy8 | Prim::FixStr => 1,
            Prim::U16 | Prim::S16 | Prim::FixStrW => 2,
            Prim::U32 | Prim::S32 | Prim::F32 | Prim::Angle32 => 4,
        }
    }

    pub fn is_text(self) -> bool {
        matches!(self, Prim::FixStr | Prim::FixStrW)
    }

    pub fn is_float(self) -> bool {
        matches!(self, Prim::F32 | Prim::Angle32)
    }
}

impl Serialize for Prim {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.name())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ColumnSpec {
    pub prim: Prim,
    /// Element count for arrays, character count for strings, 1 otherwise.
    pub count: usize,
    pub bits: Option<u8>,
}

impl ColumnSpec {
    pub fn byte_len(&self) -> usize {
        self.prim.size() * self.count
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Placement {
    pub offset: usize,
    pub bit_offset: Option<u8>,
}

/// Places every column and returns the placements plus the total row size.
pub fn place(cols: &[ColumnSpec]) -> Result<(Vec<Placement>, usize)> {
    let mut out = Vec::with_capacity(cols.len());
    let mut offset = 0usize;
    // (primitive, unit offset, next free bit) of the open bitfield unit.
    let mut unit: Option<(Prim, usize, u8)> = None;

    for (i, col) in cols.iter().enumerate() {
        match col.bits {
            None => {
                unit = None;
                out.push(Placement {
                    offset,
                    bit_offset: None,
                });
                offset += col.byte_len();
            }
            Some(bits) => {
                ensure!(
                    col.count == 1 && !col.prim.is_text() && !col.prim.is_float(),
                    "column {i}: bitfield on {} x{}",
                    col.prim.name(),
                    col.count
                );
                let limit = (col.prim.size() * 8) as u8;
                ensure!(
                    bits > 0 && bits <= limit,
                    "column {i}: {bits}-bit field does not fit {}",
                    col.prim.name()
                );
                let (prim, at, used) = match unit {
                    Some((p, at, used)) if p == col.prim && used + bits <= limit => (p, at, used),
                    _ => {
                        let at = offset;
                        offset += col.prim.size();
                        (col.prim, at, 0)
                    }
                };
                out.push(Placement {
                    offset: at,
                    bit_offset: Some(used),
                });
                unit = Some((prim, at, used + bits));
            }
        }
    }
    Ok((out, offset))
}

/// JSON has no NaN/inf, so non-finite floats are stored as their exact bit pattern.
pub const NON_FINITE_PREFIX: &str = "f32bits:";

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f32),
    Text(String),
    Bytes(Vec<u8>),
    List(Vec<Value>),
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Value::Int(v) => s.serialize_i64(*v),
            Value::Float(v) if v.is_finite() => s.serialize_f32(*v),
            Value::Float(v) => s.serialize_str(&format!("{NON_FINITE_PREFIX}{:08x}", v.to_bits())),
            Value::Text(v) => s.serialize_str(v),
            Value::Bytes(v) => s.serialize_str(&hex(v)),
            Value::List(v) => v.serialize(s),
        }
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn scalar(r: Be, prim: Prim, at: usize) -> Result<Value> {
    Ok(match prim {
        Prim::U8 => Value::Int(r.u8(at)? as i64),
        Prim::S8 => Value::Int(r.i8(at)? as i64),
        Prim::U16 => Value::Int(r.u16(at)? as i64),
        Prim::S16 => Value::Int(r.i16(at)? as i64),
        Prim::U32 => Value::Int(r.u32(at)? as i64),
        Prim::S32 => Value::Int(r.i32(at)? as i64),
        Prim::F32 | Prim::Angle32 => Value::Float(r.f32(at)?),
        Prim::Dummy8 | Prim::FixStr | Prim::FixStrW => bail!("{} is not a scalar", prim.name()),
    })
}

fn unit_value(r: Be, prim: Prim, at: usize) -> Result<u32> {
    Ok(match prim.size() {
        1 => r.u8(at)? as u32,
        2 => r.u16(at)? as u32,
        _ => r.u32(at)?,
    })
}

fn put_scalar(out: &mut [u8], prim: Prim, at: usize, v: &Value) -> Result<()> {
    let bytes: Vec<u8> = match (prim, v) {
        (Prim::U8 | Prim::Dummy8, Value::Int(n)) => vec![*n as u8],
        (Prim::S8, Value::Int(n)) => vec![*n as i8 as u8],
        (Prim::U16, Value::Int(n)) => (*n as u16).to_be_bytes().to_vec(),
        (Prim::S16, Value::Int(n)) => (*n as i16).to_be_bytes().to_vec(),
        (Prim::U32, Value::Int(n)) => (*n as u32).to_be_bytes().to_vec(),
        (Prim::S32, Value::Int(n)) => (*n as i32).to_be_bytes().to_vec(),
        (Prim::F32 | Prim::Angle32, Value::Float(f)) => f.to_bits().to_be_bytes().to_vec(),
        _ => bail!("{} cannot hold {v:?}", prim.name()),
    };
    out[at..at + bytes.len()].copy_from_slice(&bytes);
    Ok(())
}

/// Inverse of [`decode`]; used to prove the decoded values represent the row bytes exactly.
pub fn encode(
    cols: &[ColumnSpec],
    placed: &[Placement],
    size: usize,
    values: &[Value],
) -> Result<Vec<u8>> {
    let mut out = vec![0u8; size];
    for ((col, p), v) in cols.iter().zip(placed).zip(values) {
        if let (Some(_), Some(shift), Value::Int(n)) = (col.bits, p.bit_offset, v) {
            let unit = match col.prim.size() {
                1 => out[p.offset] as u32,
                2 => u16::from_be_bytes([out[p.offset], out[p.offset + 1]]) as u32,
                _ => u32::from_be_bytes(out[p.offset..p.offset + 4].try_into().unwrap()),
            } | ((*n as u32) << shift);
            let be = unit.to_be_bytes();
            out[p.offset..p.offset + col.prim.size()].copy_from_slice(&be[4 - col.prim.size()..]);
            continue;
        }
        let len = col.byte_len();
        let field = &mut out[p.offset..p.offset + len];
        match (col.prim, v) {
            (Prim::Dummy8, Value::Bytes(b)) if b.len() == len => field.copy_from_slice(b),
            (Prim::FixStr, Value::Text(s)) => {
                let enc = encoding_rs::SHIFT_JIS.encode(s).0;
                ensure!(enc.len() <= len, "text longer than field");
                field[..enc.len()].copy_from_slice(&enc);
            }
            (Prim::FixStrW, Value::Text(s)) => {
                let enc: Vec<u8> = s.encode_utf16().flat_map(u16::to_be_bytes).collect();
                ensure!(enc.len() <= len, "text longer than field");
                field[..enc.len()].copy_from_slice(&enc);
            }
            (prim, Value::List(items)) => {
                for (k, item) in items.iter().enumerate() {
                    put_scalar(&mut out, prim, p.offset + k * prim.size(), item)?;
                }
            }
            (prim, v) => put_scalar(&mut out, prim, p.offset, v)?,
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(prim: Prim, count: usize, bits: Option<u8>) -> ColumnSpec {
        ColumnSpec { prim, count, bits }
    }

    #[test]
    fn bitfields_share_a_unit() {
        let cols = [
            col(Prim::U8, 1, Some(1)),
            col(Prim::U8, 1, Some(7)),
            col(Prim::U16, 1, None),
            col(Prim::U8, 1, Some(4)),
        ];
        let (placed, size) = place(&cols).unwrap();
        assert_eq!(
            placed.iter().map(|p| p.offset).collect::<Vec<_>>(),
            [0, 0, 1, 3]
        );
        assert_eq!(placed[1].bit_offset, Some(1));
        assert_eq!(size, 4);
    }

    #[test]
    fn round_trip_is_exact_and_detects_loss() {
        let cols = [
            col(Prim::U8, 1, Some(3)),
            col(Prim::U8, 1, Some(5)),
            col(Prim::FixStr, 4, None),
            col(Prim::F32, 2, None),
            col(Prim::Dummy8, 2, None),
        ];
        let (placed, size) = place(&cols).unwrap();
        let row = [
            0b1010_1101,
            b'A',
            b'B',
            0,
            0,
            0x3f,
            0x80,
            0,
            0,
            0x7f,
            0xc0,
            0,
            1,
            0xaa,
            0xbb,
        ];
        let vals = decode(&cols, &placed, &row).unwrap();
        assert_eq!(encode(&cols, &placed, size, &vals).unwrap(), row);

        let mut dirty = row;
        dirty[4] = b'Z';
        let vals = decode(&cols, &placed, &dirty).unwrap();
        assert_ne!(encode(&cols, &placed, size, &vals).unwrap(), dirty);
    }
}

pub fn decode(cols: &[ColumnSpec], placed: &[Placement], row: &[u8]) -> Result<Vec<Value>> {
    let r = Be(row);
    cols.iter()
        .zip(placed)
        .map(|(col, p)| {
            if let (Some(bits), Some(shift)) = (col.bits, p.bit_offset) {
                let mask = if bits == 32 {
                    u32::MAX
                } else {
                    (1u32 << bits) - 1
                };
                return Ok(Value::Int(
                    ((unit_value(r, col.prim, p.offset)? >> shift) & mask) as i64,
                ));
            }
            let bytes = r.bytes(p.offset, col.byte_len())?;
            Ok(match col.prim {
                Prim::Dummy8 => Value::Bytes(bytes.to_vec()),
                Prim::FixStr => Value::Text(sjis(until_nul(bytes))),
                Prim::FixStrW => Value::Text(utf16be(bytes)),
                prim if col.count == 1 => scalar(r, prim, p.offset)?,
                prim => Value::List(
                    (0..col.count)
                        .map(|k| scalar(r, prim, p.offset + k * prim.size()))
                        .collect::<Result<_>>()?,
                ),
            })
        })
        .collect()
}
