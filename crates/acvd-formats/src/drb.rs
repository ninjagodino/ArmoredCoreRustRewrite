//! DRB menu/HUD layout (`lang/**/*.drb.dcx`, DCX-expanded). Big-endian.
//!
//! A flat run of sections, each `[u8;4] tag, u32 body_size, u32 count, u32 0` then `body_size`
//! bytes; the next section follows immediately. Three-letter tags carry a leading NUL, and the
//! other tags are written reversed (`IXET` = TEXI, `RPHS` = SHPR, `PAHS` = SHAP, `LRTC` = CTRL,
//! `OGLD` = DLGO, `GLD` = DLG). The run is `\0BRD`, `\0RTS`, `IXET`, `RPHS`, `RPTC`, `PINA`,
//! `PTNI`, `PDCS`, `PAHS`, `LRTC`, `KINA`, `OINA`, `MINA`, `KDCS`, `ODCS`, `LDCS`, `OGLD`,
//! `\0GLD`, `\0DNE`.
//!
//! - `RTS`: `count` NUL-terminated UTF-16BE strings back to back; every other name field is a byte
//!   offset into this body.
//! - `IXET` (textures): 16 bytes each, `u32 name, u32 path, u32 0, u32 0` (Windows path of the
//!   source TGA; the texture itself is in the sibling `.tpf.dcx`, by name).
//! - `PAHS` (shapes): 8 bytes each, `u32 kind, u32 offset` into the `RPHS` body, where the shape's
//!   record starts (its length depends on `kind`, not decoded yet).
//! - `LRTC` (controls): 8 bytes each, `u32 class_name, u32 offset` into the `RPTC` body.
//! - `OGLD` (dialog objects): 0x20 bytes each, `u32 name, ...` (rest kept raw).
//! - `GLD` (dialogs): 0x40 bytes each (kept raw).
//! - `PINA PTNI PDCS KINA OINA MINA KDCS ODCS LDCS` are animation / scheme tables that are empty
//!   (`size 0`) in the layouts inspected.

use anyhow::{bail, ensure, Result};
use serde::Serialize;

use crate::reader::{utf16be, Be};

pub const SECTION_HEADER: usize = 16;

#[derive(Debug, Clone, Serialize)]
pub struct Section {
    /// Tag with the leading NUL removed, as stored (reversed for four-letter tags).
    pub tag: String,
    pub count: u32,
    pub offset: usize,
    pub size: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Drb {
    pub sections: Vec<Section>,
}

pub fn is_drb(data: &[u8]) -> bool {
    data.starts_with(b"\0BRD")
}

pub fn read(data: &[u8]) -> Result<Drb> {
    ensure!(is_drb(data), "not a DRB");
    let r = Be(data);
    let (mut at, mut sections) = (0usize, Vec::new());
    loop {
        let raw = r.bytes(at, 4)?;
        let tag = String::from_utf8_lossy(&raw.iter().copied().filter(|&c| c != 0).collect::<Vec<_>>()).into_owned();
        ensure!(tag.bytes().all(|c| c.is_ascii_uppercase()) && !tag.is_empty(), "section at {at:#x} has tag {raw:02x?}");
        let (size, count) = (r.u32(at + 4)? as usize, r.u32(at + 8)?);
        ensure!(r.u32(at + 12)? == 0, "section `{tag}` header word 3 is nonzero");
        r.bytes(at + SECTION_HEADER, size)?;
        let end = tag == "DNE";
        sections.push(Section { tag, count, offset: at + SECTION_HEADER, size });
        at += SECTION_HEADER + size;
        if end {
            ensure!(at == data.len(), "{} bytes after the DNE section", data.len() - at);
            break;
        }
    }
    Ok(Drb { sections })
}

#[derive(Debug, Clone, Serialize)]
pub struct Texture {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ShapeRef {
    pub kind: u32,
    pub offset: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Control {
    pub class: String,
    pub props: u32,
}

impl Drb {
    pub fn section(&self, tag: &str) -> Option<&Section> {
        self.sections.iter().find(|s| s.tag == tag)
    }

    fn body<'a>(&self, data: &'a [u8], tag: &str) -> Result<&'a [u8]> {
        let Some(s) = self.section(tag) else { bail!("no `{tag}` section") };
        Be(data).bytes(s.offset, s.size)
    }

    /// The string whose byte offset in the `RTS` body is `at`.
    pub fn string(&self, data: &[u8], at: u32) -> Result<String> {
        let body = self.body(data, "RTS")?;
        ensure!((at as usize) < body.len(), "string offset {at:#x} past the {:#x}-byte table", body.len());
        Ok(utf16be(&body[at as usize..]))
    }

    pub fn strings(&self, data: &[u8]) -> Result<Vec<(u32, String)>> {
        let body = self.body(data, "RTS")?;
        let (mut at, mut out) = (0usize, Vec::new());
        for _ in 0..self.section("RTS").map_or(0, |s| s.count) {
            let s = utf16be(&body[at.min(body.len())..]);
            let next = at + 2 * (s.encode_utf16().count() + 1);
            out.push((at as u32, s));
            at = next;
        }
        Ok(out)
    }

    pub fn textures(&self, data: &[u8]) -> Result<Vec<Texture>> {
        let b = Be(self.body(data, "IXET")?);
        (0..b.len() / 16).map(|i| Ok(Texture { name: self.string(data, b.u32(16 * i)?)?, path: self.string(data, b.u32(16 * i + 4)?)? })).collect()
    }

    pub fn shapes(&self, data: &[u8]) -> Result<Vec<ShapeRef>> {
        let b = Be(self.body(data, "PAHS")?);
        (0..b.len() / 8).map(|i| Ok(ShapeRef { kind: b.u32(8 * i)?, offset: b.u32(8 * i + 4)? })).collect()
    }

    pub fn controls(&self, data: &[u8]) -> Result<Vec<Control>> {
        let b = Be(self.body(data, "LRTC")?);
        (0..b.len() / 8).map(|i| Ok(Control { class: self.string(data, b.u32(8 * i)?)?, props: b.u32(8 * i + 4)? })).collect()
    }

    /// Dialog object names (`OGLD`).
    pub fn objects(&self, data: &[u8]) -> Result<Vec<String>> {
        let b = Be(self.body(data, "OGLD")?);
        (0..b.len() / 0x20).map(|i| self.string(data, b.u32(0x20 * i)?)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().chain([0]).flat_map(u16::to_be_bytes).collect()
    }

    fn section(tag: &[u8; 4], count: u32, body: &[u8]) -> Vec<u8> {
        let mut v = tag.to_vec();
        v.extend((body.len() as u32).to_be_bytes());
        v.extend(count.to_be_bytes());
        v.extend(0u32.to_be_bytes());
        v.extend(body);
        v
    }

    #[test]
    fn reads_names_textures_controls() {
        let strings = [utf16("Static"), utf16("Back"), utf16("c:\\a.tga")].concat();
        let off = [0u32, utf16("Static").len() as u32, (utf16("Static").len() + utf16("Back").len()) as u32];
        let mut tex = Vec::new();
        for v in [off[1], off[2], 0, 0] {
            tex.extend(v.to_be_bytes());
        }
        let mut ctl = Vec::new();
        for v in [off[0], 4u32] {
            ctl.extend(v.to_be_bytes());
        }
        let mut file = section(b"\0BRD", 1, &[0; 16]);
        file.extend(section(b"\0RTS", 3, &strings));
        file.extend(section(b"IXET", 1, &tex));
        file.extend(section(b"LRTC", 1, &ctl));
        file.extend(section(b"\0DNE", 0, &[]));
        let d = read(&file).unwrap();
        assert_eq!(d.strings(&file).unwrap().len(), 3);
        let t = d.textures(&file).unwrap();
        assert_eq!((t[0].name.as_str(), t[0].path.as_str()), ("Back", "c:\\a.tga"));
        assert_eq!(d.controls(&file).unwrap()[0].class, "Static");
    }

    #[test]
    fn disc_layouts() {
        let usrdir = crate::vfs::usrdir(&std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join("ACVD Unbound"));
        let menu = usrdir.join("lang");
        if !menu.is_dir() {
            return;
        }
        let mut n = 0;
        for lang in std::fs::read_dir(&menu).unwrap().flatten() {
            let dir = lang.path().join("menu");
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for f in rd.flatten() {
                let name = f.file_name().to_string_lossy().into_owned();
                if !name.ends_with(".drb.dcx") {
                    continue;
                }
                let data = crate::dcx::decompress(&std::fs::read(f.path()).unwrap()).unwrap();
                let d = read(&data).unwrap_or_else(|e| panic!("{}: {e:#}", f.path().display()));
                d.strings(&data).unwrap();
                let tex = d.textures(&data).unwrap();
                d.controls(&data).unwrap();
                d.shapes(&data).unwrap();
                d.objects(&data).unwrap();
                assert!(tex.iter().all(|t| !t.name.is_empty()), "{name}");
                n += 1;
            }
        }
        assert!(n >= 60, "{n} DRBs");
    }
}
