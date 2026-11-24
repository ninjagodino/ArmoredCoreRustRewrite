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
use bevy::prelude::{ChildOf, Commands, Component, Entity, Node, PositionType};
use bevy::ui::px;

use crate::Packs;

/// A single laid-out font glyph; [`crate::menu::MenuPlugin`] draws it with the Catmull-Rom
/// bicubic [`crate::menu::GlyphSprite`] shader so 720p CCM bitmap fonts stay smooth and crisp when upscaled.
#[derive(Component, Clone)]
pub struct GlyphNode {
    pub image: Handle<Image>,
    pub rect: Rect,
    pub color: Color,
}

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
        let row = acvd_data::textures_named(&tex).next().or_else(|| acvd_data::textures_named(&tex.to_lowercase()).next());
        let mut image = match row {
            Some(t) => packs.texture(disc, t)?,
            // Fonts the extracted sheets do not list (the 360 `s1_xbox*`) are read from their pack.
            None => {
                let pack = format!("font/{name}/{name}_t.bnd|#0");
                let index = packs.find(disc, &pack, &tex)?.with_context(|| format!("no texture `{tex}` in {pack}"))?;
                packs.texture_at(disc, &pack, index, &tex)?
            }
        };
        image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
            address_mode_u: ImageAddressMode::ClampToEdge,
            address_mode_v: ImageAddressMode::ClampToEdge,
            ..ImageSamplerDescriptor::linear()
        });
        let size = image.texture_descriptor.size;
        sheet_size.push([size.width as u16, size.height as u16]);
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
        let w = if uv.width() > 0.0 { uv.width() } else { g.width as f32 } * scale;
        let h = if uv.height() > 0.0 { uv.height() } else { ccm.line_height as f32 } * scale;
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
                left: px(origin.x.round()),
                top: px(origin.y.round()),
                width: px(size.x.round()),
                height: px(size.y.round()),
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
                width: px(q.size.x.max(1.0)),
                height: px(q.size.y.max(1.0)),
                ..Default::default()
            },
            GlyphNode { image, rect: q.uv, color },
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

    #[test]
    fn does_not_stretch_glyph_past_uv_rect() {
        let ccm = Ccm {
            version: 0x10001,
            line_height: 16,
            tex_width: 128,
            tex_height: 128,
            unk0e: 32,
            texture_count: 1,
            groups: vec![CodeGroup { first: b'1' as i32, last: b'1' as i32, glyph: 0 }],
            glyphs: vec![Glyph {
                uv0: [0.40625, 0.0078125],
                uv1: [0.4453125, 0.1328125], // 5x16 texels
                pre_space: 2,
                width: 9, // wider than 5-texel UV in e16_ext
                advance: 9,
                texture: 0,
                unk: None,
            }],
        };
        let (quads, _) = layout(&ccm, &[[128, 128]], "1", 1.0);
        assert_eq!(quads.len(), 1);
        assert!((quads[0].size.x - 5.0).abs() < 1e-4, "got width {}", quads[0].size.x);
        assert!((quads[0].size.y - 16.0).abs() < 1e-4, "got height {}", quads[0].size.y);
    }

    #[test]
    fn spawn_glyphs_preserves_subpixel_positions_for_bicubic_aa() {
        let mut world = bevy::ecs::world::World::new();
        let parent = world.spawn_empty().id();
        let ccm = font();
        let f = Font { ccm: ccm.clone(), sheets: vec![Handle::default()], sheet_size: vec![[256, 128]] };
        let (quads, _) = layout(&ccm, &f.sheet_size, "AB", 0.86);
        assert_eq!(quads.len(), 2);
        {
            let mut commands = world.commands();
            spawn_glyphs(&mut commands, &f, &quads, Vec2::new(0.25, 0.5), parent, Color::WHITE);
        }
        world.flush();
        let mut nodes: Vec<(f32, f32)> = world
            .query::<(&Node, &GlyphNode)>()
            .iter(&world)
            .map(|(n, _)| match (n.left, n.width) {
                (bevy::ui::Val::Px(l), bevy::ui::Val::Px(w)) => (l, w),
                other => panic!("unexpected vals {other:?}"),
            })
            .collect();
        nodes.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(nodes.len(), 2);
        assert!((nodes[0].0 - (0.25 + quads[0].pos.x)).abs() < 1e-4);
        assert!((nodes[0].1 - quads[0].size.x).abs() < 1e-4);
        assert!((nodes[1].0 - (0.25 + quads[1].pos.x)).abs() < 1e-4);
    }
}
