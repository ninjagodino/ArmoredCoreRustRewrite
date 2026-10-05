//! DRB menu layouts drawn as Bevy UI nodes.
//!
//! `path` is a `lang/<lang>/menu/<name>.drb.dcx` asset; its textures come from the sibling
//! `<name>.tpf.dcx`, matched by the DRB texture name or else its source TGA's file stem (en
//! `staffroll` lists `Titleback` but packs `Titleback_TM`). Layout pixels are 1280x720 screen
//! pixels; `spawn` scales them.
//!
//! Drawn: `Sprite` (texel rect, tint, flip and quarter-turn flags), `MonoRect`, `MonoFrame`,
//! `GouraudRect` (top-to-bottom gradient of its left corner colors), `GouraudFrame` (first
//! color), nested `Dialog`s, and `Text` with its `fontdef.xml` font, aligned in its rect: static
//! strings and bank-1 messages (`lang/<lang>/text/menu/menu.fmg`). Runtime texts are blank unless
//! [`Layout::placeholders`] is set (then they show the object name, or `000` in fonts without
//! its letters). Not yet: additive blend
//! (sprite blend 2 draws as alpha), runtime image slots (emblems, maps, movies), `NoiseSprite`.

use std::path::Path;

use std::collections::HashMap;

use acvd_formats::drb::{self, Drb, Shape, TextSource};
use acvd_formats::fmg::Fmg;
use anyhow::{Context, Result};
use bevy::asset::{Assets, Handle};
use bevy::color::Color;
use bevy::image::{Image, ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::math::{Rect, Rot2, Vec2};
use bevy::prelude::{BackgroundColor, BorderColor, ChildOf, Commands, Component, Entity, ImageNode, Node, PositionType};
use bevy::ui::{px, BackgroundGradient, ColorStop, LinearGradient, UiRect, UiTransform};

use crate::text::{self, Font};
use crate::Packs;

/// A parsed layout plus one GPU image per `IXET` texture (None when the pack lacks it), the
/// fonts its texts use (by `fontdef.xml` ID) and the menu message bank.
pub struct Layout {
    pub drb: Drb,
    pub textures: Vec<Option<Handle<Image>>>,
    pub fonts: HashMap<u8, Font>,
    pub messages: Option<Fmg>,
    pub placeholders: bool,
}

pub fn load(usrdir: &Path, path: &str, images: &mut Assets<Image>) -> Result<Layout> {
    let drb = drb::read(&acvd_formats::vfs::open(usrdir, path)?).with_context(|| format!("reading {path}"))?;
    let pack = path.strip_suffix(".drb.dcx").map(|p| format!("{p}.tpf.dcx")).with_context(|| format!("{path} is not a .drb.dcx"))?;
    let mut packs = Packs::default();
    let textures = drb
        .textures
        .iter()
        .map(|t| {
            let stem = t.path.rsplit(['\\', '/']).next().unwrap_or_default();
            let stem = stem.rsplit_once('.').map_or(stem, |(s, _)| s);
            let found = [t.name.as_str(), stem].into_iter().find_map(|n| acvd_data::textures_named(n).find(|r| r.pack.eq_ignore_ascii_case(&pack)));
            let Some(r) = found else {
                eprintln!("{path}: texture `{}` not in {pack}", t.name);
                return None;
            };
            match packs.texture(usrdir, r) {
                Ok(mut image) => {
                    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                        address_mode_u: ImageAddressMode::ClampToEdge,
                        address_mode_v: ImageAddressMode::ClampToEdge,
                        ..ImageSamplerDescriptor::linear()
                    });
                    Some(images.add(image))
                }
                Err(e) => {
                    eprintln!("{path}: {e:#}");
                    None
                }
            }
        })
        .collect();
    let fontdefs = acvd_formats::vfs::open(usrdir, "font/fontdef.xml").and_then(|d| acvd_formats::fontdef::read(&d));
    let mut fonts = HashMap::new();
    match fontdefs {
        Ok(defs) => {
            for o in drb.dialogs.iter().flat_map(|d| &d.objects) {
                let Shape::Text { font, .. } = o.shape else { continue };
                if fonts.contains_key(&font) {
                    continue;
                }
                let loaded = defs.file(font as u32).with_context(|| format!("font {font} not in fontdef.xml")).and_then(|(name, file)| text::load_file(usrdir, name, file, images));
                match loaded {
                    Ok(f) => {
                        fonts.insert(font, f);
                    }
                    Err(e) => eprintln!("{path}: font {font}: {e:#}"),
                }
            }
        }
        Err(e) => eprintln!("font/fontdef.xml: {e:#}"),
    }
    let menu_fmg = path.split_once("/menu/").map(|(lang, _)| format!("{lang}/text/menu/menu.fmg"));
    let messages = menu_fmg.and_then(|p| match acvd_formats::vfs::open(usrdir, &p).and_then(|d| acvd_formats::fmg::read(&d)) {
        Ok(f) => Some(f),
        Err(e) => {
            eprintln!("{p}: {e:#}");
            None
        }
    });
    Ok(Layout { drb, textures, fonts, messages, placeholders: false })
}

/// Marker on the root node of a spawned layout.
#[derive(Component)]
pub struct MenuRoot;

fn rgba(c: u32) -> Color {
    let [r, g, b, a] = c.to_be_bytes();
    Color::srgba_u8(r, g, b, a)
}

fn node(rect: [i16; 4], scale: f32) -> Node {
    let (x0, x1) = (rect[0].min(rect[2]) as f32, rect[0].max(rect[2]) as f32);
    let (y0, y1) = (rect[1].min(rect[3]) as f32, rect[1].max(rect[3]) as f32);
    Node {
        position_type: PositionType::Absolute,
        left: px(x0 * scale),
        top: px(y0 * scale),
        width: px((x1 - x0) * scale),
        height: px((y1 - y0) * scale),
        ..Default::default()
    }
}

/// Spawns dialog `index` with its top-left at `origin` (logical pixels), layout pixels times
/// `scale`. Returns the root, tagged [`MenuRoot`].
pub fn spawn(commands: &mut Commands, layout: &Layout, index: usize, origin: Vec2, scale: f32) -> Entity {
    let size = layout.drb.dialogs.get(index).map_or([0, 0], |d| d.size);
    let mut root = node([0, 0, size[0] as i16, size[1] as i16], scale);
    root.left = px(origin.x);
    root.top = px(origin.y);
    let root = commands.spawn((MenuRoot, root)).id();
    spawn_dialog(commands, layout, index, root, scale, 0);
    root
}

fn spawn_dialog(commands: &mut Commands, layout: &Layout, index: usize, parent: Entity, scale: f32, depth: usize) {
    let Some(dialog) = layout.drb.dialogs.get(index) else { return };
    if depth > 16 {
        return;
    }
    for o in &dialog.objects {
        match &o.shape {
            &Shape::Sprite { rect, uv, texture: Some(t), flags, color } => {
                let Some(Some(image)) = layout.textures.get(t as usize).cloned() else { continue };
                let mut n = node(rect, scale);
                let turns = (flags >> 10) & 3;
                let mut transform = UiTransform::IDENTITY;
                if turns != 0 {
                    transform.rotation = Rot2::degrees(90.0 * turns as f32);
                    if turns % 2 == 1 {
                        // Lay the unrotated image out with swapped sides around the same center.
                        let (w, h) = ((rect[2] - rect[0]).abs() as f32 * scale, (rect[3] - rect[1]).abs() as f32 * scale);
                        let (x, y) = (rect[0].min(rect[2]) as f32 * scale, rect[1].min(rect[3]) as f32 * scale);
                        n.left = px(x + (w - h) / 2.0);
                        n.top = px(y + (h - w) / 2.0);
                        n.width = px(h);
                        n.height = px(w);
                    }
                }
                let uv = Rect::new(uv[0] as f32, uv[1] as f32, uv[2] as f32, uv[3] as f32);
                commands.spawn((
                    n,
                    transform,
                    ImageNode { image, color: rgba(color), rect: Some(uv), flip_x: flags & 0x100 != 0, flip_y: flags & 0x200 != 0, ..Default::default() },
                    ChildOf(parent),
                ));
            }
            &Shape::MonoRect { rect, color, .. } => {
                commands.spawn((node(rect, scale), BackgroundColor(rgba(color)), ChildOf(parent)));
            }
            &Shape::MonoFrame { rect, flags, color } => {
                let mut n = node(rect, scale);
                n.border = UiRect::all(px((flags & 0xff).max(1) as f32 * scale));
                commands.spawn((n, BorderColor::all(rgba(color)), ChildOf(parent)));
            }
            &Shape::GouraudRect { rect, colors, .. } => {
                let gradient = LinearGradient::to_bottom(vec![ColorStop::auto(rgba(colors[0])), ColorStop::auto(rgba(colors[2]))]);
                commands.spawn((node(rect, scale), BackgroundGradient::from(gradient), ChildOf(parent)));
            }
            &Shape::GouraudFrame { rect, flags, colors } => {
                let mut n = node(rect, scale);
                n.border = UiRect::all(px((flags & 0xff).max(1) as f32 * scale));
                commands.spawn((n, BorderColor::all(rgba(colors[0])), ChildOf(parent)));
            }
            Shape::Text { rect, color, font, align, source, .. } => {
                let string = match source {
                    TextSource::Static(s) => Some(s.clone()),
                    TextSource::Message { bank: 1, id } => layout.messages.as_ref().and_then(|m| m.get(*id as i32)).map(str::to_string),
                    _ => layout.placeholders.then(|| o.name.clone()),
                };
                let (Some(mut string), Some(f)) = (string, layout.fonts.get(font)) else { continue };
                if layout.placeholders && string == o.name && string.encode_utf16().any(|c| f.ccm.glyph(c).is_none()) {
                    string = "000".into();
                }
                let (quads, size) = text::layout(&f.ccm, &f.sheet_size, &string, scale);
                let n = node(*rect, scale);
                let (w, h) = ((rect[2] - rect[0]).abs() as f32 * scale, (rect[3] - rect[1]).abs() as f32 * scale);
                let x = match align & 3 {
                    1 => w - size.x,
                    2 => (w - size.x) / 2.0,
                    _ => 0.0,
                };
                let y = match align & 0xc {
                    8 => (h - size.y) / 2.0,
                    4 => h - size.y,
                    _ => 0.0,
                };
                let boxed = commands.spawn((n, ChildOf(parent))).id();
                text::spawn_glyphs(commands, f, &quads, Vec2::new(x, y), boxed, rgba(*color));
            }
            &Shape::Dialog { rect, dialog: Some(d), .. } => {
                let child = commands.spawn((node(rect, scale), ChildOf(parent))).id();
                spawn_dialog(commands, layout, d as usize, child, scale, depth + 1);
            }
            _ => {}
        }
    }
}
