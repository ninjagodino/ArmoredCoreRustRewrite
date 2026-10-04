//! PARAM: a table of fixed-size rows described by a PARAMDEF.
//!
//! Header (0x30 bytes):
//! `u32 strings_offset, u16 data_start, u16 unk06, u16 data_version, u16 row_count,
//!  fixstr[0x20] param_type, u8 big_endian, u8 flags_2d, u8 flags_2e, u8 flags_2f`,
//! followed by `row_count` headers of `u32 id, u32 data_offset, u32 name_offset`.

use anyhow::{ensure, Result};
use serde::Serialize;

use crate::reader::Be;

#[derive(Debug, Clone, Serialize)]
pub struct Param {
    pub param_type: String,
    pub data_version: u16,
    pub unk06: u16,
    pub flags: [u8; 3],
    pub strings_offset: u32,
    pub data_start: u32,
    pub rows: Vec<RowHeader>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RowHeader {
    pub id: u32,
    pub data_offset: u32,
    pub name: String,
}

/// Cheap check used to pick PARAM files out of the hash-named dump.
pub fn looks_like_param(data: &[u8]) -> bool {
    let r = Be(data);
    let (Ok(rows), Ok(start), Ok(endian)) = (r.u16(0x0A), r.u16(0x04), r.u8(0x2C)) else {
        return false;
    };
    let Ok(ty) = r.fixstr(0x0C, 0x20) else { return false };
    endian == 0xFF
        && !ty.is_empty()
        && ty.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
        && start as usize == 0x30 + rows as usize * 12
}

pub fn read(data: &[u8]) -> Result<Param> {
    let r = Be(data);
    ensure!(r.u8(0x2C)? == 0xFF, "PARAM is not big-endian");
    let row_count = r.u16(0x0A)? as usize;
    let data_start = r.u16(0x04)? as u32;
    ensure!(
        data_start as usize == 0x30 + row_count * 12,
        "data_start {data_start:#x} does not follow {row_count} row headers"
    );

    let mut rows = Vec::with_capacity(row_count);
    for i in 0..row_count {
        let o = 0x30 + i * 12;
        let name_offset = r.u32(o + 8)? as usize;
        rows.push(RowHeader {
            id: r.u32(o)?,
            data_offset: r.u32(o + 4)?,
            name: if name_offset == 0 { String::new() } else { r.cstr_sjis(name_offset)? },
        });
    }

    Ok(Param {
        param_type: r.fixstr(0x0C, 0x20)?,
        data_version: r.u16(0x08)?,
        unk06: r.u16(0x06)?,
        flags: [r.u8(0x2D)?, r.u8(0x2E)?, r.u8(0x2F)?],
        strings_offset: r.u32(0x00)?,
        data_start,
        rows,
    })
}

impl Param {
    /// Smallest gap between distinct row data offsets; `None` when only one row has data.
    pub fn observed_stride(&self) -> Option<u32> {
        let mut offsets: Vec<u32> = self.rows.iter().map(|r| r.data_offset).collect();
        offsets.sort_unstable();
        offsets.dedup();
        offsets.windows(2).map(|w| w[1] - w[0]).min()
    }
}
