//! `font/fontdef.xml`: font ID -> CCM folder. Shift-JIS XML whose content is ASCII.
//!
//! Each `<Font>` has an `<ID>` and either `<CcmFile>$(FontData)\<folder>\<folder>.ccm</CcmFile>`
//! (plus `Lang="JP"` / `"KR"` / `"CN"` variants, ignored here: the untagged one is the
//! English/European default) or `<RefFontID>` naming another ID. Both discs ship the same file;
//! `$(Platform)` is [`crate::vfs::Disc::platform`] (`font/s1_xbox/` on the 360, `s1_PS3/` on
//! the PS3).

use anyhow::{bail, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontDef {
    /// `font/<folder>/<file>`; the extension matters (`e10` has both a `.ccm` and the `.ccf` fontdef
    /// names, with different metrics).
    File { folder: String, file: String },
    Ref(u32),
}

#[derive(Debug, Clone, Default)]
pub struct FontDefs(pub Vec<(u32, FontDef)>);

fn tag<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&format!("</{name}>"))? + start;
    Some(body[start..end].trim())
}

/// `platform` replaces `$(Platform)` in file paths.
pub fn read(data: &[u8], platform: &str) -> Result<FontDefs> {
    let text = String::from_utf8_lossy(data);
    let mut out = Vec::new();
    for block in text.split("<Font>").skip(1) {
        let block = block.split("</Font>").next().unwrap_or_default();
        let Some(id) = tag(block, "ID").and_then(|s| s.parse().ok()) else { bail!("<Font> without a numeric <ID>") };
        let def = if let Some(r) = tag(block, "RefFontID").and_then(|s| s.parse().ok()) {
            FontDef::Ref(r)
        } else if let Some(ccm) = tag(block, "CcmFile") {
            let ccm = ccm.replace("$(Platform)", platform);
            let mut parts = ccm.rsplit(['\\', '/']);
            let file = parts.next().unwrap_or_default().to_string();
            let Some(folder) = parts.next() else { bail!("font {id}: CcmFile `{ccm}` has no folder") };
            FontDef::File { folder: folder.to_string(), file }
        } else {
            bail!("font {id} has neither CcmFile nor RefFontID");
        };
        out.push((id, def));
    }
    Ok(FontDefs(out))
}

impl FontDefs {
    /// `(folder, file)` of the glyph map for `id`, following `RefFontID` links.
    pub fn file(&self, id: u32) -> Option<(&str, &str)> {
        let mut id = id;
        for _ in 0..8 {
            match &self.0.iter().find(|(i, _)| *i == id)?.1 {
                FontDef::File { folder, file } => return Some((folder, file)),
                FontDef::Ref(r) => id = *r,
            }
        }
        None
    }

    pub fn folder(&self, id: u32) -> Option<&str> {
        self.file(id).map(|(folder, _)| folder)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_folders_langs_and_refs() {
        let xml = br"<FontList><Font><ID>0</ID><CcmFile>$(FontData)\j1_16_ext\j1_16_ext.ccm</CcmFile>
            <CcmFile Lang='JP'>$(FontData)\j1_16\j1_16.ccm</CcmFile></Font>
            <Font><ID>10</ID><CcmFile>$(FontData)\s1_$(Platform)\s1_$(Platform).ccm</CcmFile></Font>
            <Font><ID>17</ID><RefFontID>0</RefFontID></Font></FontList>";
        let d = read(xml, "PS3").unwrap();
        assert_eq!(d.folder(0), Some("j1_16_ext"));
        assert_eq!(d.folder(10), Some("s1_PS3"));
        assert_eq!(read(xml, "xbox").unwrap().file(10), Some(("s1_xbox", "s1_xbox.ccm")));
        assert_eq!(d.folder(17), Some("j1_16_ext"));
        assert_eq!(d.folder(5), None);
    }

    #[test]
    fn disc_fontdef() {
        let root = crate::vfs::repo_root();
        for path in [root.join(crate::vfs::PS3_DUMP), root.join(crate::vfs::X360_ISO)] {
            let Ok(disc) = crate::vfs::Disc::open(&path) else { continue };
            let d = read(&disc.read("font/fontdef.xml").unwrap(), disc.platform()).unwrap();
            assert_eq!(d.0.len(), 20);
            assert_eq!(d.folder(1), Some("e1_ext"));
            assert_eq!(d.file(12), Some(("e10", "e10.ccf")));
            assert_eq!(d.folder(99), Some("j1_16_ext"));
            for (_, def) in &d.0 {
                if let FontDef::File { folder, file } = def {
                    assert!(disc.exists(&format!("font/{folder}/{file}")), "{}: font/{folder}/{file}", path.display());
                }
            }
        }
    }
}
