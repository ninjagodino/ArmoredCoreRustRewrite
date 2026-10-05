//! DRB menu layouts drawn as Bevy UI nodes.
//!
//! `path` is a `lang/<lang>/menu/<name>.drb.dcx` asset; its textures come from the sibling
//! `<name>.tpf.dcx`, matched by the DRB texture name or else its source TGA's file stem (en
//! `staffroll` lists `Titleback` but packs `Titleback_TM`). Layout pixels are 1280x720 screen
//! pixels; `spawn` scales them.
//!
//! Drawn: `Sprite` (texel rect, tint, flip and quarter-turn flags), `MonoRect`, `MonoFrame`,
//! `GouraudRect` (top-to-bottom gradient of its left corner colors), `GouraudFrame` (first
//! color), and nested `Dialog`s. Not yet: `Text` (strings come from the controls), additive blend
//! (sprite blend 2 draws as alpha), runtime image slots (emblems, maps, movies), `NoiseSprite`.

use std::path::Path;

use acvd_formats::drb::{self, Drb, Shape};
use anyhow::{Context, Result};
use bevy::asset::{Assets, Handle};
use bevy::color::Color;
use bevy::image::{Image, ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::math::{Rect, Rot2, Vec2};
use bevy::prelude::{BackgroundColor, BorderColor, ChildOf, Commands, Component, Entity, ImageNode, Node, PositionType};
use bevy::ui::{px, BackgroundGradient, ColorStop, LinearGradient, UiRect, UiTransform};

use crate::Packs;

/// A parsed layout plus one GPU image per `IXET` texture (None when the pack lacks it).
pub struct Layout {
    pub drb: Drb,
    pub textures: Vec<Option<Handle<Image>>>,
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
    Ok(Layout { drb, textures })
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
            &Shape::Dialog { rect, dialog: Some(d), .. } => {
                let child = commands.spawn((node(rect, scale), ChildOf(parent))).id();
                spawn_dialog(commands, layout, d as usize, child, scale, depth + 1);
            }
            _ => {}
        }
    }
}
