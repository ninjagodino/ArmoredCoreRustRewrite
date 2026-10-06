//! DRB menu/HUD layout (`lang/**/*.drb.dcx`, DCX-expanded). Big-endian.
//!
//! A flat run of sections, each `[u8;4] tag, u32 body_size, u32 count, u32 0` then `body_size`
//! bytes; the next section follows immediately. Three-letter tags carry a leading NUL, and the
//! other tags are written reversed (`IXET` = TEXI, `RPHS` = SHPR, `PAHS` = SHAP, `LRTC` = CTRL,
//! `OGLD` = DLGO, `GLD` = DLG). The run is `\0BRD`, `\0RTS`, `IXET`, `RPHS`, `RPTC`, `PINA`,
//! `PTNI`, `PDCS`, `PAHS`, `LRTC`, `KINA`, `OINA`, `MINA`, `KDCS`, `ODCS`, `LDCS`, `OGLD`,
//! `\0GLD`, `\0DNE`.
//!
//! - `RTS`: `count` NUL-terminated UTF-16BE strings back to back; every name field elsewhere is a
//!   byte offset into this body.
//! - `IXET` (textures): 16 bytes each, `u32 name, u32 path, u32 0, u32 0` (path of the source
//!   TGA). The image is in the sibling `.tpf.dcx`, under `name` or the path's file stem.
//! - `PAHS` (shapes): 8 bytes each, `u32 class_name, u32 offset` of its record in `RPHS`.
//! - `LRTC` (controls): 8 bytes each, `u32 class_name, u32 offset` of its record in `RPTC`.
//! - `OGLD` (dialog objects): 0x20 bytes each, `u32 name, u32 shape, u32 control` (byte offsets
//!   into `PAHS` / `LRTC`), rest 0.
//! - `GLD` (dialogs): 0x40 bytes, the same three words, then at +0x20 `u32 object_count,
//!   u32 first_object` (byte offset into `OGLD`), `u32 0`, `u16 width, u16 height`.
//! - `PINA PTNI PDCS KINA OINA MINA KDCS ODCS LDCS`: animation / scheme tables, not decoded.
//!
//! Shape records (`RPHS`) all start with an `i16 x0, y0, x1, y1` rect in the parent dialog's
//! pixels (the full screen is 1280x720); colors are `u32` RGBA:
//!
//! | class | after the rect |
//! |---|---|
//! | `Null` | nothing |
//! | `Sprite` | `i16 u0, v0, u1, v1` (texel rect), `u16 texture` (`IXET` index; 0xffff = none; >= 1000 = runtime image slot, e.g. emblems 10000+, movies 101xx, maps 102xx), `u16 flags`, `u32 0`, `color` |
//! | `MonoRect`, `MonoFrame` | `u32 flags`, `u32 0`, `color` |
//! | `GouraudRect`, `GouraudFrame` | `u32 flags`, four corner colors |
//! | `Text` | `u32 flags`, `u32 unk`, `color`, `u8 0, u8 font, u8 align, u8 mode`, `u32 0x1c`, then by mode: 0 `u32 string`; 1 `u32 bank, u32 id` (36 bytes); 2 `u32 capacity` |
//! | `Dialog` | `u16 dialog` (`GLD` index drawn at the rect, 0xffff = set at runtime), `u16 flags`, `u32 0`, `color` |
//! | `AlphaAnimSprite` | the `Sprite` fields, then `i16 u0, v0, u1, v1` (+0x1c, texel rect of the mask), `u16 mask` (+0x24, `IXET` index), `u8 mirror` (+0x26; nonzero: the mask takes the sprite's flips); 39 bytes |
//!
//! `AlphaAnimSprite` (360 factory `0x824abaf8`, ctor `0x824ced28`, draw `0x824cedd0`) is a gauge:
//! the draw binds the sprite texture and the mask, puts `1 - fill` in pixel constant c0 and
//! draws with shader mode 3, `shader/boot_shader.bnd` `Sprite_AlphaRef.fpo` (kills texels whose
//! mask alpha is below c0.x). `fill` (+0x44, 0..1, starts at 1) is set through vtable slot
//! 0x3c (`0x824cecf8`).
//! Sprite flags: low byte = blend (1, 2), 0x100 flip X, 0x200 flip Y, bits 0xc00 = quarter turns
//! clockwise (screen corners of the frame pieces in `vssortie.drb` match only this reading).
//! Frame flags: low byte = line width.

use anyhow::{bail, ensure, Context, Result};
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
    pub textures: Vec<Texture>,
    pub dialogs: Vec<Dialog>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Texture {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Dialog {
    pub name: String,
    pub size: [u16; 2],
    pub objects: Vec<Object>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Object {
    pub name: String,
    pub control: String,
    pub shape: Shape,
}

pub type Rect = [i16; 4];

/// Where a `Text` shape's string comes from (record byte +0x17; 360 factory `0x824ac270`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum TextSource {
    /// Mode 0: the `RTS` string at +0x1c.
    Static(String),
    /// Mode 1: message `id` (+0x20) of text bank `bank` (+0x1c), looked up by `0x82b1a8b0`.
    /// Bank 1 is `lang/<lang>/text/menu/menu.fmg`; bank 2 is filled by code (ids 1-3, 100, 101).
    Message { bank: u32, id: u32 },
    /// Mode 2: set by game code; +0x1c is the buffer length in characters.
    Runtime { capacity: u32 },
    /// Mode 3: one of four special text classes picked by +0x1c (unused in the en layouts read).
    Special { mode: u8, kind: u32 },
}

#[derive(Debug, Clone, Serialize)]
pub enum Shape {
    Null { rect: Rect },
    Sprite { rect: Rect, uv: Rect, texture: Option<u16>, flags: u16, color: u32 },
    MonoRect { rect: Rect, flags: u32, color: u32 },
    MonoFrame { rect: Rect, flags: u32, color: u32 },
    GouraudRect { rect: Rect, flags: u32, colors: [u32; 4] },
    GouraudFrame { rect: Rect, flags: u32, colors: [u32; 4] },
    /// `font` is a `fontdef.xml` ID; `align` low two bits = 0 left, 1 right, 2 center; bit 0x8 = vertical center (bit 0x4, likely bottom, is unverified).
    Text { rect: Rect, flags: u32, color: u32, font: u8, align: u8, source: TextSource, unk: u32 },
    Dialog { rect: Rect, dialog: Option<u16>, flags: u16, color: u32 },
    /// A gauge: `texture` drawn where the `mask` texture's alpha (over `mask_uv`) is at least
    /// `1 - fill`; `mirror` applies the sprite's flip flags to the mask too.
    AlphaAnimSprite { rect: Rect, uv: Rect, texture: Option<u16>, flags: u16, color: u32, mask_uv: Rect, mask: Option<u16>, mirror: bool },
    Other { class: String, rect: Rect },
}

impl Shape {
    pub fn rect(&self) -> Rect {
        match self {
            Shape::Null { rect }
            | Shape::Sprite { rect, .. }
            | Shape::MonoRect { rect, .. }
            | Shape::MonoFrame { rect, .. }
            | Shape::GouraudRect { rect, .. }
            | Shape::GouraudFrame { rect, .. }
            | Shape::Text { rect, .. }
            | Shape::Dialog { rect, .. }
            | Shape::AlphaAnimSprite { rect, .. }
            | Shape::Other { rect, .. } => *rect,
        }
    }
}

pub fn is_drb(data: &[u8]) -> bool {
    data.starts_with(b"\0BRD")
}

fn sections(data: &[u8]) -> Result<Vec<Section>> {
    let r = Be(data);
    let (mut at, mut out) = (0usize, Vec::new());
    loop {
        let raw = r.bytes(at, 4)?;
        let tag = String::from_utf8_lossy(&raw.iter().copied().filter(|&c| c != 0).collect::<Vec<_>>()).into_owned();
        ensure!(!tag.is_empty() && tag.bytes().all(|c| c.is_ascii_uppercase()), "section at {at:#x} has tag {raw:02x?}");
        let (size, count) = (r.u32(at + 4)? as usize, r.u32(at + 8)?);
        ensure!(r.u32(at + 12)? == 0, "section `{tag}` header word 3 is nonzero");
        r.bytes(at + SECTION_HEADER, size)?;
        let end = tag == "DNE";
        out.push(Section { tag, count, offset: at + SECTION_HEADER, size });
        at += SECTION_HEADER + size;
        if end {
            ensure!(at == data.len(), "{} bytes after the DNE section", data.len() - at);
            return Ok(out);
        }
    }
}

struct Ctx<'a> {
    data: &'a [u8],
    sections: &'a [Section],
}

impl<'a> Ctx<'a> {
    fn body(&self, tag: &str) -> Result<Be<'a>> {
        let Some(s) = self.sections.iter().find(|s| s.tag == tag) else { bail!("no `{tag}` section") };
        Ok(Be(Be(self.data).bytes(s.offset, s.size)?))
    }

    fn string(&self, at: u32) -> Result<String> {
        let body = self.body("RTS")?.0;
        ensure!((at as usize) < body.len(), "string offset {at:#x} past the {:#x}-byte table", body.len());
        Ok(utf16be(&body[at as usize..]))
    }

    fn rect(r: &Be, at: usize) -> Result<Rect> {
        Ok([r.i16(at)?, r.i16(at + 2)?, r.i16(at + 4)?, r.i16(at + 6)?])
    }

    /// The shape whose `PAHS` entry is at byte offset `at`.
    fn shape(&self, at: u32) -> Result<Shape> {
        let pahs = self.body("PAHS")?;
        let class = self.string(pahs.u32(at as usize)?)?;
        let o = pahs.u32(at as usize + 4)? as usize;
        let r = self.body("RPHS")?;
        let rect = Self::rect(&r, o)?;
        Ok(match class.as_str() {
            "Null" => Shape::Null { rect },
            "Sprite" => {
                let texture = r.u16(o + 16)?;
                Shape::Sprite { rect, uv: Self::rect(&r, o + 8)?, texture: (texture != 0xffff).then_some(texture), flags: r.u16(o + 18)?, color: r.u32(o + 24)? }
            }
            "MonoRect" => Shape::MonoRect { rect, flags: r.u32(o + 8)?, color: r.u32(o + 16)? },
            "MonoFrame" => Shape::MonoFrame { rect, flags: r.u32(o + 8)?, color: r.u32(o + 16)? },
            "GouraudRect" | "GouraudFrame" => {
                let (flags, colors) = (r.u32(o + 8)?, [r.u32(o + 12)?, r.u32(o + 16)?, r.u32(o + 20)?, r.u32(o + 24)?]);
                if class == "GouraudRect" {
                    Shape::GouraudRect { rect, flags, colors }
                } else {
                    Shape::GouraudFrame { rect, flags, colors }
                }
            }
            "Text" => {
                let source = match r.u8(o + 0x17)? {
                    0 => TextSource::Static(self.string(r.u32(o + 0x1c)?)?),
                    1 => TextSource::Message { bank: r.u32(o + 0x1c)?, id: r.u32(o + 0x20)? },
                    2 => TextSource::Runtime { capacity: r.u32(o + 0x1c)? },
                    m => TextSource::Special { mode: m, kind: r.u32(o + 0x1c)? },
                };
                Shape::Text { rect, flags: r.u32(o + 8)?, color: r.u32(o + 16)?, font: r.u8(o + 0x15)?, align: r.u8(o + 0x16)?, source, unk: r.u32(o + 12)? }
            }
            "Dialog" => Shape::Dialog { rect, dialog: Some(r.u16(o + 8)?).filter(|&d| d != 0xffff), flags: r.u16(o + 10)?, color: r.u32(o + 16)? },
            "AlphaAnimSprite" => {
                let (texture, mask) = (r.u16(o + 16)?, r.u16(o + 0x24)?);
                Shape::AlphaAnimSprite {
                    rect,
                    uv: Self::rect(&r, o + 8)?,
                    texture: (texture != 0xffff).then_some(texture),
                    flags: r.u16(o + 18)?,
                    color: r.u32(o + 24)?,
                    mask_uv: Self::rect(&r, o + 0x1c)?,
                    mask: (mask != 0xffff).then_some(mask),
                    mirror: r.u8(o + 0x26)? != 0,
                }
            }
            _ => Shape::Other { class, rect },
        })
    }

    fn control(&self, at: u32) -> Result<String> {
        self.string(self.body("LRTC")?.u32(at as usize)?)
    }
}

pub fn read(data: &[u8]) -> Result<Drb> {
    ensure!(is_drb(data), "not a DRB");
    let sections = sections(data)?;
    let c = Ctx { data, sections: &sections };
    let tex = c.body("IXET")?;
    let textures = (0..tex.len() / 16).map(|i| Ok(Texture { name: c.string(tex.u32(16 * i)?)?, path: c.string(tex.u32(16 * i + 4)?)? })).collect::<Result<_>>()?;
    let (dlg, obj) = (c.body("GLD")?, c.body("OGLD")?);
    let mut dialogs = Vec::with_capacity(dlg.len() / 0x40);
    for i in 0..dlg.len() / 0x40 {
        let at = 0x40 * i;
        let name = c.string(dlg.u32(at)?)?;
        let (count, first) = (dlg.u32(at + 0x20)? as usize, dlg.u32(at + 0x24)? as usize);
        let objects = (0..count)
            .map(|j| {
                let o = first + 0x20 * j;
                Ok(Object { name: c.string(obj.u32(o)?)?, shape: c.shape(obj.u32(o + 4)?)?, control: c.control(obj.u32(o + 8)?)? })
            })
            .collect::<Result<_>>()
            .with_context(|| format!("dialog `{name}`"))?;
        dialogs.push(Dialog { name, size: [dlg.u16(at + 0x2c)?, dlg.u16(at + 0x2e)?], objects });
    }
    Ok(Drb { sections, textures, dialogs })
}

impl Drb {
    pub fn section(&self, tag: &str) -> Option<&Section> {
        self.sections.iter().find(|s| s.tag == tag)
    }

    pub fn dialog(&self, name: &str) -> Option<&Dialog> {
        self.dialogs.iter().find(|d| d.name == name)
    }

    /// Dialogs no `Dialog` shape places: the ones the game instantiates directly.
    pub fn roots(&self) -> impl Iterator<Item = (usize, &Dialog)> {
        let placed: std::collections::HashSet<u16> = self
            .dialogs
            .iter()
            .flat_map(|d| &d.objects)
            .filter_map(|o| match o.shape {
                Shape::Dialog { dialog, .. } => dialog,
                _ => None,
            })
            .collect();
        self.dialogs.iter().enumerate().filter(move |(i, _)| !placed.contains(&(*i as u16)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().chain([0]).flat_map(u16::to_be_bytes).collect()
    }

    fn words(w: &[u32]) -> Vec<u8> {
        w.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    fn section(tag: &[u8; 4], count: u32, body: &[u8]) -> Vec<u8> {
        let mut v = tag.to_vec();
        v.extend(words(&[body.len() as u32, count, 0]));
        v.extend(body);
        v
    }

    #[test]
    fn reads_dialog_with_sprite() {
        let names = ["Root", "Sprite", "Static", "Back", "c:\\Back_TM.tga", "BG"];
        let mut off = Vec::new();
        let mut strings = Vec::new();
        for n in names {
            off.push(strings.len() as u32);
            strings.extend(utf16(n));
        }
        // Sprite (0,0)-(1280,720) from texels (0,0)-(1024,720), texture 0, flags 0x0c01, white.
        let sprite = words(&[0, 0x0500_02d0, 0, 0x0400_02d0, 0x0000_0c01, 0, 0xffff_ffff]);
        let mut file = section(b"\0BRD", 1, &[0; 16]);
        file.extend(section(b"\0RTS", names.len() as u32, &strings));
        file.extend(section(b"IXET", 1, &words(&[off[3], off[4], 0, 0])));
        file.extend(section(b"RPHS", 1, &sprite));
        file.extend(section(b"PAHS", 1, &words(&[off[1], 0])));
        file.extend(section(b"LRTC", 1, &words(&[off[2], 0])));
        file.extend(section(b"OGLD", 1, &words(&[off[5], 0, 0, 0, 0, 0, 0, 0])));
        file.extend(section(b"\0GLD", 1, &words(&[off[0], 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0x0500_02d0, 0, 0, 0, 0])));
        file.extend(section(b"\0DNE", 0, &[]));
        let d = read(&file).unwrap();
        assert_eq!((d.textures[0].name.as_str(), d.textures[0].path.as_str()), ("Back", "c:\\Back_TM.tga"));
        let dlg = d.dialog("Root").unwrap();
        assert_eq!(dlg.size, [1280, 720]);
        assert_eq!((dlg.objects[0].name.as_str(), dlg.objects[0].control.as_str()), ("BG", "Static"));
        let Shape::Sprite { rect, uv, texture, flags, color } = dlg.objects[0].shape else { panic!() };
        assert_eq!((rect, uv, texture, flags, color), ([0, 0, 1280, 720], [0, 0, 1024, 720], Some(0), 0x0c01, 0xffff_ffff));
        assert_eq!(d.roots().count(), 1);
    }

    #[test]
    fn disc_layouts() {
        let Some(disc) = crate::vfs::test_disc() else { return };
        let mut n = 0;
        for name in disc.files() {
            let lower = name.to_ascii_lowercase();
            if !lower.starts_with("lang/") || !lower.contains("/menu/") || !lower.ends_with(".drb.dcx") {
                continue;
            }
            let d = read(&disc.asset(&name).unwrap()).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            for o in d.dialogs.iter().flat_map(|d| &d.objects) {
                match o.shape {
                    Shape::Sprite { texture: Some(t), .. } => assert!((t as usize) < d.textures.len() || t >= 1000, "{name}: `{}` texture {t}", o.name),
                    Shape::AlphaAnimSprite { texture, mask, .. } => {
                        for t in [texture, mask].into_iter().flatten() {
                            assert!((t as usize) < d.textures.len() || t >= 1000, "{name}: `{}` texture {t}", o.name);
                        }
                    }
                    Shape::Dialog { dialog: Some(dialog), .. } => assert!((dialog as usize) < d.dialogs.len(), "{name}: `{}` dialog {dialog}", o.name),
                    _ => {}
                }
            }
            assert!(d.roots().count() > 0, "{name}");
            n += 1;
        }
        assert!(n >= 60, "{n} DRBs");
        let d = read(&disc.asset("lang/en/menu/staffroll.drb.dcx").unwrap()).unwrap();
        assert_eq!(d.textures[0].name, "Titleback");
        let base = &d.dialogs[0];
        assert_eq!((base.name.as_str(), base.size, base.objects.len()), ("@StaffRollBase", [1280, 720], 2));
        assert!(matches!(base.objects[1].shape, Shape::Sprite { rect: [1024, 0, 1280, 720], uv: [0, 736, 720, 992], texture: Some(0), flags: 0x0c01, .. }));
        let text = |file: &str, dialog: &str, object: &str| {
            let d = read(&disc.asset(&format!("lang/{file}")).unwrap()).unwrap();
            let o = d.dialog(dialog).unwrap().objects.iter().find(|o| o.name == object).unwrap().shape.clone();
            let Shape::Text { font, align, source, .. } = o else { panic!("{object} is not Text") };
            (font, align, source)
        };
        assert_eq!(text("en/menu/staffroll.drb.dcx", "@TermOfServiceItem", "@Text_1"), (0, 0x09, TextSource::Runtime { capacity: 0x80 }));
        assert_eq!(text("en/menu/vssortie.drb.dcx", "SelfScore Ex", "Slash"), (0x0c, 0x09, TextSource::Static("/".into())));
        let d = read(&disc.asset("lang/en/menu/sortie.drb.dcx").unwrap()).unwrap();
        let gauge = |name: &str| d.dialog("ACV_FE_LockSightCenter").unwrap().objects.iter().find(|o| o.name == name).unwrap().shape.clone();
        let Shape::AlphaAnimSprite { rect, uv, texture, flags, color, mask_uv, mask, mirror } = gauge("Gauge_LWeapon") else { panic!("Gauge_LWeapon") };
        assert_eq!((rect, uv, texture, flags, color), ([-149, -149, -8, -22], [361, 3, 502, 130], Some(18), 2, 0x00ff_a232));
        assert_eq!((mask_uv, mask, mirror), ([0, 0, 128, 128], Some(20), false));
        assert_eq!(d.textures[20].name, "ACV_FE_SightGaugeAnim2");
        let Shape::AlphaAnimSprite { flags, mask, mirror, .. } = gauge("Gauge_EN") else { panic!("Gauge_EN") };
        assert_eq!((flags, mask, mirror), (0x102, Some(16), true));
    }
}
