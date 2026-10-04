//! PlayStation Edge compressed triangle lists (FLVER face sets with index size 8).
//!
//! Group header (0x10 bytes): `i16 segment_count, u16 unk02, u32 unk04, u32 0, u32 unk0c`,
//! then one 0x40-byte header per segment:
//! `u32 data_length, u32 data_offset (from group start), u32 0, u32 0, u16 unk10, u16 unk12,
//!  u16 base_vertex, u16 unk16, u32 unk18, u32 unk1c, 16 x 0, u16 x 4 unk30, u16 vertex_count,
//!  u16 index_count, i32 -1`.
//!
//! Segment payload: `u16 explicit_count, u16 bias, u16 flag_bytes, u8 value_bits, u8 0`, then
//! three MSB-first bit streams, each starting on a byte boundary:
//! - flags, `flag_bytes` long: one bit per index slot, 0 = next unused vertex, 1 = explicit value;
//! - codes, two bits per triangle: 3 = three new slots `(a, b, c)`; 0, 1, 2 = one new slot `n`
//!   sharing an edge of the previous triangle `(p0, p1, p2)`: `(p0, p2, n)`, `(p2, p1, n)`,
//!   `(p1, p0, n)`;
//! - values, `explicit_count` x `value_bits`: explicit index k is `value - bias + explicit[k - 8]`
//!   (eight running sums, one per SPU halfword lane), with `explicit[k - 8] = 0` for k < 8.
//!
//! Indices are local to the segment; add `base_vertex` for the mesh's vertex buffer. Triangles
//! wind clockwise around the stored normals. Every byte after the value stream is zero.

use anyhow::{bail, ensure, Context, Result};
use serde::Serialize;

use crate::reader::Be;

pub const GROUP_HEADER: usize = 0x10;
pub const SEGMENT_HEADER: usize = 0x40;
pub const PAYLOAD_HEADER: usize = 8;
pub const LANES: usize = 8;

#[derive(Debug, Clone, Serialize)]
pub struct Group {
    pub unk02: u16,
    pub unk04: u32,
    pub unk0c: u32,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Segment {
    pub data_length: u32,
    pub data_offset: u32,
    pub unk10: u16,
    pub unk12: u16,
    pub base_vertex: u16,
    pub unk16: u16,
    pub unk18: u32,
    pub unk1c: u32,
    pub unk30: [u16; 4],
    pub vertex_count: u16,
    pub index_count: u16,
}

pub fn read_group(group: &[u8]) -> Result<Group> {
    let r = Be(group);
    let count = r.i16(0)?;
    ensure!(count >= 0, "negative segment count {count}");
    ensure!(r.u32(8)? == 0, "group header 0x08 is nonzero");
    let mut segments = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        let at = GROUP_HEADER + SEGMENT_HEADER * i;
        ensure!(r.u32(at + 8)? == 0 && r.u32(at + 0xC)? == 0, "segment {i} header 0x08..0x10 is nonzero");
        ensure!(r.bytes(at + 0x20, 0x10)?.iter().all(|&b| b == 0), "segment {i} header 0x20..0x30 is nonzero");
        ensure!(r.i32(at + 0x3C)? == -1, "segment {i} header 0x3C is not -1");
        let seg = Segment {
            data_length: r.u32(at)?,
            data_offset: r.u32(at + 4)?,
            unk10: r.u16(at + 0x10)?,
            unk12: r.u16(at + 0x12)?,
            base_vertex: r.u16(at + 0x14)?,
            unk16: r.u16(at + 0x16)?,
            unk18: r.u32(at + 0x18)?,
            unk1c: r.u32(at + 0x1C)?,
            unk30: [r.u16(at + 0x30)?, r.u16(at + 0x32)?, r.u16(at + 0x34)?, r.u16(at + 0x36)?],
            vertex_count: r.u16(at + 0x38)?,
            index_count: r.u16(at + 0x3A)?,
        };
        r.bytes(seg.data_offset as usize, seg.data_length as usize).with_context(|| format!("segment {i} payload"))?;
        segments.push(seg);
    }
    Ok(Group { unk02: r.u16(2)?, unk04: r.u32(4)?, unk0c: r.u32(0xC)?, segments })
}

impl Segment {
    pub fn payload<'a>(&self, group: &'a [u8]) -> Result<&'a [u8]> {
        Be(group).bytes(self.data_offset as usize, self.data_length as usize)
    }
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn take(&mut self, n: u8) -> Result<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let Some(&byte) = self.data.get(self.pos >> 3) else { bail!("bit stream runs past its {} bytes", self.data.len()) };
            v = (v << 1) | ((byte >> (7 - (self.pos & 7))) & 1) as u32;
            self.pos += 1;
        }
        Ok(v)
    }
}

/// Decodes one segment payload into `index_count` segment-local indices below `vertex_count`.
pub fn decode_segment(payload: &[u8], index_count: usize, vertex_count: usize) -> Result<Vec<u16>> {
    let r = Be(payload);
    let explicit_count = r.u16(0)? as usize;
    let bias = r.u16(2)? as i32;
    let flag_bytes = r.u16(4)? as usize;
    let value_bits = r.u8(6)?;
    ensure!(r.u8(7)? == 0, "payload byte 7 is nonzero");
    ensure!(value_bits <= 16, "value width {value_bits} bits");
    ensure!(index_count.is_multiple_of(3), "index count {index_count} is not a triangle list");
    let tris = index_count / 3;

    let codes_at = PAYLOAD_HEADER + flag_bytes;
    let values_at = codes_at + (2 * tris).div_ceil(8);
    let end = values_at + (explicit_count * value_bits as usize).div_ceil(8);
    ensure!(end <= payload.len(), "streams end at {end:#x}, payload is {:#x} bytes", payload.len());
    ensure!(payload[end..].iter().all(|&b| b == 0), "nonzero bytes after the value stream");

    let mut codes = Bits { data: &payload[codes_at..values_at], pos: 0 };
    let codes: Vec<u32> = (0..tris).map(|_| codes.take(2)).collect::<Result<_>>()?;
    let slots: usize = codes.iter().enumerate().map(|(t, &c)| if c == 3 || t == 0 { 3 } else { 1 }).sum();
    let flag_stream = &payload[PAYLOAD_HEADER..codes_at];
    ensure!(slots <= flag_bytes * 8, "{slots} slots need more than {flag_bytes} flag bytes");
    let mut flags = Bits { data: flag_stream, pos: 0 };
    let flags: Vec<bool> = (0..slots).map(|_| flags.take(1).map(|b| b == 1)).collect::<Result<_>>()?;
    let ones = flags.iter().filter(|&&f| f).count();
    ensure!(ones == explicit_count, "{ones} explicit flags, header says {explicit_count}");
    let mut pad = Bits { data: flag_stream, pos: slots };
    ensure!((slots..flag_bytes * 8).all(|_| pad.take(1).is_ok_and(|b| b == 0)), "nonzero flag padding");

    let mut values = Bits { data: &payload[values_at..end], pos: 0 };
    let mut explicit: Vec<i32> = Vec::with_capacity(explicit_count);
    for k in 0..explicit_count {
        let prior = if k >= LANES { explicit[k - LANES] } else { 0 };
        explicit.push(values.take(value_bits)? as i32 - bias + prior);
    }

    let (mut next_flag, mut next_explicit, mut next_new) = (0, 0, 0usize);
    let mut slot = || -> Result<u16> {
        let i = if flags[next_flag] {
            next_explicit += 1;
            explicit[next_explicit - 1]
        } else {
            next_new += 1;
            next_new as i32 - 1
        };
        next_flag += 1;
        ensure!((0..vertex_count as i32).contains(&i), "index {i} outside the segment's {vertex_count} vertices");
        Ok(i as u16)
    };
    let mut out = Vec::with_capacity(index_count);
    let mut last = [0u16; 3];
    for (t, &code) in codes.iter().enumerate() {
        let tri = match code {
            _ if t == 0 || code == 3 => [slot()?, slot()?, slot()?],
            0 => [last[0], last[2], slot()?],
            1 => [last[2], last[1], slot()?],
            _ => [last[1], last[0], slot()?],
        };
        out.extend_from_slice(&tri);
        last = tri;
    }
    Ok(out)
}

/// Decodes a whole group into mesh-relative indices (`base_vertex` added).
pub fn decode_group(group: &[u8]) -> Result<Vec<u32>> {
    let g = read_group(group)?;
    let mut out = Vec::new();
    for (i, s) in g.segments.iter().enumerate() {
        let local = decode_segment(s.payload(group)?, s.index_count as usize, s.vertex_count as usize).with_context(|| format!("segment {i}"))?;
        out.extend(local.into_iter().map(|v| s.base_vertex as u32 + v as u32));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 2-triangle quad and 4-triangle fan shapes seen on the disc.
    #[test]
    fn quad_and_strip() {
        // codes 3,2 (quad): slots 0,1,2 new, then new 3 sharing (p1, p0)
        let quad = [0, 0, 0, 0, 0, 1, 0, 0, 0x00, 0b1110_0000];
        assert_eq!(decode_segment(&quad, 6, 4).unwrap(), [0, 1, 2, 1, 0, 3]);
        // codes 3,0,1,1: every slot new
        let strip = [0, 0, 0, 0, 0, 1, 0, 0, 0x00, 0b1100_0101];
        assert_eq!(decode_segment(&strip, 12, 6).unwrap(), [0, 1, 2, 0, 2, 3, 3, 2, 4, 4, 2, 5]);
    }

    #[test]
    fn explicit_values_run_per_lane() {
        // one fresh triangle with 2 new slots then 1 explicit index (value 1, bias 0) -> vertex 1
        let p = [0, 1, 0, 0, 0, 1, 2, 0, 0b0010_0000, 0b1100_0000, 0b0100_0000];
        assert_eq!(decode_segment(&p, 3, 2).unwrap(), [0, 1, 1]);
    }

    #[test]
    fn rejects_out_of_range() {
        let quad = [0, 0, 0, 0, 0, 1, 0, 0, 0x00, 0b1110_0000];
        assert!(decode_segment(&quad, 6, 3).is_err());
    }
}
