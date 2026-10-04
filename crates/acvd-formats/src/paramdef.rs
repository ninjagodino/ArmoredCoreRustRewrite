//! PARAMDEF: the column schema for a PARAM table.
//!
//! Header (0x30 bytes):
//! `u32 file_size, u16 header_size, u16 data_version, u16 field_count, u16 field_size,
//!  fixstr[0x20] param_type, u8 big_endian (0xFF), u8 unicode, u16 format_version`.
//!
//! Field records, by format version:
//! - 101: 0x8C bytes, no internal name.
//! - 103: 0xAC bytes, adds `fixstr[0x20] internal_name`. The header's field_size (0x6C) is wrong
//!   on disc, so the stride is taken from the version instead.
//! - 104: 0xB0 bytes, adds `i32 sort_id` after the internal name.

use anyhow::{bail, ensure, Result};
use serde::Serialize;

use crate::reader::Be;

#[derive(Debug, Clone, Serialize)]
pub struct ParamDef {
    pub param_type: String,
    pub data_version: u16,
    pub format_version: u16,
    pub declared_field_size: u16,
    pub fields: Vec<DefField>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DefField {
    pub display_name: String,
    pub display_type: String,
    pub display_format: String,
    pub default: f32,
    pub min: f32,
    pub max: f32,
    pub increment: f32,
    pub edit_flags: i32,
    pub byte_count: i32,
    pub description: String,
    /// Display type for plain fields, or an enum name such as `ON_OFF`.
    pub internal_type: String,
    /// Raw name including any `[N]` array or `:N` bitfield suffix.
    pub internal_name: Option<String>,
    pub sort_id: Option<i32>,
}

pub fn field_stride(format_version: u16) -> Option<usize> {
    match format_version {
        101 => Some(0x8C),
        103 => Some(0xAC),
        104 => Some(0xB0),
        _ => None,
    }
}

pub fn read(data: &[u8]) -> Result<ParamDef> {
    let r = Be(data);
    let header_size = r.u16(0x04)?;
    ensure!(header_size == 0x30, "unexpected PARAMDEF header size {header_size:#x}");
    ensure!(r.u8(0x2C)? == 0xFF, "PARAMDEF is not big-endian");
    ensure!(r.u8(0x2D)? == 0, "unicode PARAMDEF strings are not used by this game");
    let format_version = r.u16(0x2E)?;
    let Some(stride) = field_stride(format_version) else {
        bail!("unsupported PARAMDEF format version {format_version}");
    };
    let field_count = r.u16(0x08)? as usize;

    let mut fields = Vec::with_capacity(field_count);
    for i in 0..field_count {
        let o = 0x30 + i * stride;
        let description_offset = r.i32(o + 0x68)?;
        fields.push(DefField {
            display_name: r.fixstr_sjis(o, 0x40)?,
            display_type: r.fixstr(o + 0x40, 8)?,
            display_format: r.fixstr(o + 0x48, 8)?,
            default: r.f32(o + 0x50)?,
            min: r.f32(o + 0x54)?,
            max: r.f32(o + 0x58)?,
            increment: r.f32(o + 0x5C)?,
            edit_flags: r.i32(o + 0x60)?,
            byte_count: r.i32(o + 0x64)?,
            description: if description_offset > 0 { r.cstr_sjis(description_offset as usize)? } else { String::new() },
            internal_type: r.fixstr(o + 0x6C, 0x20)?,
            internal_name: (stride >= 0xAC).then(|| r.fixstr(o + 0x8C, 0x20)).transpose()?,
            sort_id: (stride >= 0xB0).then(|| r.i32(o + 0xAC)).transpose()?,
        });
    }

    Ok(ParamDef {
        param_type: r.fixstr(0x0C, 0x20)?,
        data_version: r.u16(0x06)?,
        format_version,
        declared_field_size: r.u16(0x0A)?,
        fields,
    })
}
