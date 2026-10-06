//! Disc font: a CCM glyph map plus its BC3 sheets, laid out as UI image nodes.
//!
//! `name` is a `font/<name>/` folder (`fontdef.xml` ID 1 is `e1_ext` for English). The map is
//! `<name>.ccm`, or `<name>.ccf` when that is what fontdef lists; sheets are
//! `font/<name>/<name>_t.bnd|<name>.tpf`, named `{name}_{index:04}` in the texture sheet.

use acvd_formats::ccm::{self, Ccm};
use acvd_formats::vfs::Disc;
use anyhow::{Context, Result};
use bevy::asset::{Assets, Handle};
use bevy::color::Color;
use bevy::image::{Image, ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::math::{Rect, Vec2};
use bevy::prelude::{ChildOf, Commands, Component, Entity, ImageNode, Node, PositionType};
use bevy::ui::px;

use crate::Packs;

/// A loaded CCM and one GPU image per texture index.
#[derive(Clone)]
pub struct Font {
    pub ccm: Ccm,
    pub sheets: Vec<Handle<Image>>,
    pub sheet_size: Vec<[u16; 2]>,
}

/// One glyph of a laid-out string, in the same pixel space as [`Ccm::line_height`].
#[derive(Debug, Clone, Copy)]
pub struct GlyphQuad {
    pub texture: usize,
    pub pos: Vec2,
    pub size: Vec2,
    pub uv: Rect,
}

pub fn load(disc: &Disc, name: &str, images: &mut Assets<Image>) -> Result<Font> {
    let file = ["ccm", "ccf"].iter().map(|ext| format!("{name}.{ext}")).find(|f| disc.exists(&format!("font/{name}/{f}")));
    load_file(disc, name, &file.with_context(|| format!("no font/{name}/{name}.ccm or .ccf"))?, images)
}

/// Like [`load`] with the glyph map named explicitly (`fontdef.xml` `CcmFile`).
pub fn load_file(disc: &Disc, name: &str, file: &str, images: &mut Assets<Image>) -> Result<Font> {
    let data = disc.asset(&format!("font/{name}/{file}"))?;
    let ccm = ccm::read(&data)?;
    let mut packs = Packs::default();
    let mut sheets = Vec::with_capacity(ccm.texture_count as usize);
    let mut sheet_size = Vec::with_capacity(ccm.texture_count as usize);
    for i in 0..ccm.texture_count {
        let tex = format!("{name}_{i:04}");
        let t = acvd_data::textures_named(&tex)
            .next()
            .or_else(|| acvd_data::textures_named(&tex.to_lowercase()).next())
            .with_context(|| format!("no texture `{tex}`"))?;
        let mut image = packs.texture(disc, t)?;
        image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
            address_mode_u: ImageAddressMode::ClampToEdge,
            address_mode_v: ImageAddressMode::ClampToEdge,
            ..ImageSamplerDescriptor::linear()
        });
        sheet_size.push([t.width, t.height]);
        sheets.push(images.add(image));
    }
    Ok(Font { ccm, sheets, sheet_size })
}

/// Glyph quads and the box they occupy. `\n` starts a new line; missing codes are skipped.
pub fn layout(ccm: &Ccm, sheet_size: &[[u16; 2]], text: &str, scale: f32) -> (Vec<GlyphQuad>, Vec2) {
    let line = (ccm.line_height as f32).max(1.0) * scale;
    let mut quads = Vec::new();
    let (mut x, mut y, mut width) = (0.0f32, 0.0f32, 0.0f32);
    for unit in text.encode_utf16() {
        if unit == b'\n' as u16 {
            width = width.max(x);
            x = 0.0;
            y += line;
            continue;
        }
        let Some(g) = ccm.glyph(unit) else { continue };
        let tex = g.texture as usize;
        let [tw, th] = sheet_size.get(tex).copied().filter(|s| s[0] > 0 && s[1] > 0).or_else(|| (ccm.tex_width > 0).then_some([ccm.tex_width as u16, ccm.tex_height as u16])).unwrap_or([1, 1]);
        let (tw, th) = (tw as f32, th as f32);
        let uv = Rect::new(g.uv0[0] * tw, g.uv0[1] * th, g.uv1[0] * tw, g.uv1[1] * th);
        let w = (g.width as f32).max(uv.width()) * scale;
        let h = uv.height().max(ccm.line_height as f32) * scale;
        if w > 0.0 {
            quads.push(GlyphQuad { texture: tex, pos: Vec2::new(x + g.pre_space as f32 * scale, y), size: Vec2::new(w, h), uv });
        }
        x += g.advance as f32 * scale;
    }
    (quads, Vec2::new(width.max(x), y + line))
}

/// Marker on the root of a spawned label, so the caller can despawn it.
#[derive(Component)]
pub struct Label;

/// Spawns a screen-space label at `origin` (top-left, logical pixels) using the font's sheets.
pub fn spawn(commands: &mut Commands, font: &Font, text: &str, origin: Vec2, scale: f32, color: Color) -> Entity {
    let (quads, size) = layout(&font.ccm, &font.sheet_size, text, scale);
    let root = commands
        .spawn((
            Label,
            Node {
                position_type: PositionType::Absolute,
                left: px(origin.x),
                top: px(origin.y),
                width: px(size.x),
                height: px(size.y),
                ..Default::default()
            },
        ))
        .id();
    spawn_glyphs(commands, font, &quads, Vec2::ZERO, root, color);
    root
}

/// Spawns `quads` as children of `parent`, offset by `origin` (logical pixels).
pub fn spawn_glyphs(commands: &mut Commands, font: &Font, quads: &[GlyphQuad], origin: Vec2, parent: Entity, color: Color) {
    for q in quads {
        let Some(image) = font.sheets.get(q.texture).cloned() else { continue };
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(origin.x + q.pos.x),
                top: px(origin.y + q.pos.y),
                width: px(q.size.x),
                height: px(q.size.y),
                ..Default::default()
            },
            ImageNode { image, color, rect: Some(q.uv), ..Default::default() },
            ChildOf(parent),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acvd_formats::ccm::{CodeGroup, Glyph};

    fn font() -> Ccm {
        Ccm {
            version: 0x10000,
            line_height: 20,
            tex_width: 256,
            tex_height: 128,
            unk0e: 32,
            texture_count: 1,
            groups: vec![CodeGroup { first: 0x41, last: 0x42, glyph: 0 }],
            glyphs: vec![
                Glyph { uv0: [0.0, 0.0], uv1: [0.05, 0.15], pre_space: 1, width: 10, advance: 12, texture: 0, unk: None },
                Glyph { uv0: [0.05, 0.0], uv1: [0.09, 0.15], pre_space: 0, width: 8, advance: 9, texture: 0, unk: None },
            ],
        }
    }

    #[test]
    fn lays_out_advance_and_newlines() {
        let ccm = font();
        let (quads, size) = layout(&ccm, &[[256, 128]], "AB\nA", 2.0);
        assert_eq!(quads.len(), 3);
        assert_eq!(quads[0].pos, Vec2::new(2.0, 0.0));
        assert_eq!(quads[1].pos.x, 24.0);
        assert_eq!(quads[2].pos, Vec2::new(2.0, 40.0));
        assert_eq!(size, Vec2::new(42.0, 80.0));
    }
}
