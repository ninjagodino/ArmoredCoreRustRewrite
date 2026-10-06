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
//! strings and bank-1 messages (`lang/<lang>/text/menu/menu.fmg`). Runtime texts carry a
//! [`MenuText`] for game code to fill; they start blank unless [`Layout::placeholders`] is set
//! (then they show the object name, or `000` in fonts without its letters). Sprite blend 1 is
//! alpha, blend 2 additive ([`AdditiveSprite`]; apps add [`MenuPlugin`]). `AlphaAnimSprite`
//! gauges are [`MenuGauge`]s. Not yet: runtime image slots (emblems, maps, movies), `NoiseSprite`.

use std::collections::HashMap;
use std::sync::Arc;

use acvd_formats::drb::{self, Drb, Shape, TextSource};
use acvd_formats::vfs::Disc;
use acvd_formats::fmg::Fmg;
use anyhow::{Context, Result};
use bevy::asset::{embedded_asset, Asset, Assets, Handle};
use bevy::color::{Color, LinearRgba};
use bevy::image::{Image, ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::math::{Rect, Rot2, Vec2, Vec4};
use bevy::prelude::{App, BackgroundColor, BorderColor, Changed, ChildOf, Children, Commands, Component, Entity, ImageNode, Node, Plugin, PositionType, PostUpdate, Query, ResMut, TypePath};
use bevy::render::render_resource::{AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, RenderPipelineDescriptor};
use bevy::shader::ShaderRef;
use bevy::ui::{px, BackgroundGradient, ColorStop, LinearGradient, UiRect, UiTransform};
use bevy::ui_render::prelude::{MaterialNode, UiMaterial, UiMaterialPlugin};
use bevy::ui_render::ui_material::UiMaterialKey;

use crate::text::{self, Font};
use crate::Packs;

/// A parsed layout plus one GPU image per `IXET` texture (None when the pack lacks it), the
/// fonts its texts use (by `fontdef.xml` ID) and the menu message bank.
pub struct Layout {
    pub drb: Drb,
    pub textures: Vec<Option<Handle<Image>>>,
    pub fonts: HashMap<u8, Arc<Font>>,
    pub messages: Option<Fmg>,
    pub placeholders: bool,
}

pub fn load(disc: &Disc, path: &str, images: &mut Assets<Image>) -> Result<Layout> {
    let drb = drb::read(&disc.asset(path)?).with_context(|| format!("reading {path}"))?;
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
            match packs.texture(disc, r) {
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
    let fontdefs = disc.asset("font/fontdef.xml").and_then(|d| acvd_formats::fontdef::read(&d, disc.platform()));
    let mut fonts = HashMap::new();
    match fontdefs {
        Ok(defs) => {
            for o in drb.dialogs.iter().flat_map(|d| &d.objects) {
                let Shape::Text { font, .. } = o.shape else { continue };
                if fonts.contains_key(&font) {
                    continue;
                }
                let loaded = defs.file(font as u32).with_context(|| format!("font {font} not in fontdef.xml")).and_then(|(name, file)| text::load_file(disc, name, file, images));
                match loaded {
                    Ok(f) => {
                        fonts.insert(font, Arc::new(f));
                    }
                    Err(e) => eprintln!("{path}: font {font}: {e:#}"),
                }
            }
        }
        Err(e) => eprintln!("font/fontdef.xml: {e:#}"),
    }
    let menu_fmg = path.split_once("/menu/").map(|(lang, _)| format!("{lang}/text/menu/menu.fmg"));
    let messages = menu_fmg.and_then(|p| match disc.asset(&p).and_then(|d| acvd_formats::fmg::read(&d)) {
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

/// The DRB object name on each drawn object's node, for game code that fills runtime values
/// (digit sprites, texts) or toggles conditional parts.
#[derive(Component)]
pub struct MenuObject(pub String);

/// A layout sprite; [`MenuPlugin`] draws it as an `ImageNode` (blend 1) or with
/// [`AdditiveSprite`] (blend 2) and redraws it when changed. `rect` is in texels.
#[derive(Component, Clone)]
pub struct MenuSprite {
    pub image: Handle<Image>,
    pub rect: Rect,
    pub color: Color,
    pub flip_x: bool,
    pub flip_y: bool,
    pub additive: bool,
}

/// Sprite drawn with additive blending (source times alpha plus destination), the DRB sprite
/// blend mode 2: the lock-sight art is drawn on black that must add nothing.
#[derive(AsBindGroup, Asset, TypePath, Clone)]
pub struct AdditiveSprite {
    #[uniform(0)]
    tint: LinearRgba,
    #[uniform(1)]
    rect: Vec4,
    #[texture(2)]
    #[sampler(3)]
    image: Handle<Image>,
}

impl From<&MenuSprite> for AdditiveSprite {
    fn from(s: &MenuSprite) -> Self {
        Self { tint: s.color.into(), rect: flipped(s.rect, s.flip_x, s.flip_y), image: s.image.clone() }
    }
}

impl UiMaterial for AdditiveSprite {
    fn fragment_shader() -> ShaderRef {
        "embedded://acvd_render/additive_sprite.wgsl".into()
    }

    fn specialize(descriptor: &mut RenderPipelineDescriptor, _key: UiMaterialKey<Self>) {
        set_blend(descriptor, ADDITIVE);
    }
}

/// An `AlphaAnimSprite` gauge: the sprite drawn where the mask texture's alpha (over
/// `mask_rect`) is at least `1 - fill`. The masks are angular alpha ramps, so `fill` sweeps the
/// arc. Game code sets `fill` (the object's +0x44, 1.0 at creation, ctor `0x824ced28`) and
/// `color`; [`MenuPlugin`] redraws on change.
#[derive(Component, Clone)]
pub struct MenuGauge {
    pub image: Handle<Image>,
    pub rect: Rect,
    pub mask: Handle<Image>,
    pub mask_rect: Rect,
    pub color: Color,
    pub flip_x: bool,
    pub flip_y: bool,
    /// The record's mirror byte: the mask takes the sprite's flips too.
    pub mirror: bool,
    pub additive: bool,
    pub fill: f32,
}

/// [`MenuGauge`] material: `gauge_sprite.wgsl`, the `Sprite_AlphaRef.fpo` alpha test of
/// `shader/boot_shader.bnd` (draw `0x824cedd0` binds the mask as texture 1, `1 - fill` in c0).
#[derive(AsBindGroup, Asset, TypePath, Clone)]
#[bind_group_data(GaugeKey)]
pub struct GaugeSprite {
    #[uniform(0)]
    tint: LinearRgba,
    #[uniform(1)]
    rect: Vec4,
    #[uniform(2)]
    mask_rect: Vec4,
    #[uniform(3)]
    threshold: Vec4,
    #[texture(4)]
    #[sampler(5)]
    image: Handle<Image>,
    #[texture(6)]
    #[sampler(7)]
    mask: Handle<Image>,
    additive: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct GaugeKey {
    additive: bool,
}

impl From<&GaugeSprite> for GaugeKey {
    fn from(g: &GaugeSprite) -> Self {
        Self { additive: g.additive }
    }
}

fn flipped(r: Rect, flip_x: bool, flip_y: bool) -> Vec4 {
    let (mut u0, mut u1, mut v0, mut v1) = (r.min.x, r.max.x, r.min.y, r.max.y);
    if flip_x {
        (u0, u1) = (u1, u0);
    }
    if flip_y {
        (v0, v1) = (v1, v0);
    }
    Vec4::new(u0, v0, u1, v1)
}

impl From<&MenuGauge> for GaugeSprite {
    fn from(g: &MenuGauge) -> Self {
        let mask_rect = if g.mirror { flipped(g.mask_rect, g.flip_x, g.flip_y) } else { flipped(g.mask_rect, false, false) };
        Self {
            tint: g.color.into(),
            rect: flipped(g.rect, g.flip_x, g.flip_y),
            mask_rect,
            threshold: Vec4::splat(1.0 - g.fill.clamp(0.0, 1.0)),
            image: g.image.clone(),
            mask: g.mask.clone(),
            additive: g.additive,
        }
    }
}

const ADDITIVE: BlendState = BlendState {
    color: BlendComponent { src_factor: BlendFactor::SrcAlpha, dst_factor: BlendFactor::One, operation: BlendOperation::Add },
    alpha: BlendComponent { src_factor: BlendFactor::Zero, dst_factor: BlendFactor::One, operation: BlendOperation::Add },
};

fn set_blend(descriptor: &mut RenderPipelineDescriptor, blend: BlendState) {
    if let Some(fragment) = descriptor.fragment.as_mut() {
        for target in fragment.targets.iter_mut().flatten() {
            target.blend = Some(blend);
        }
    }
}

impl UiMaterial for GaugeSprite {
    fn fragment_shader() -> ShaderRef {
        "embedded://acvd_render/gauge_sprite.wgsl".into()
    }

    fn specialize(descriptor: &mut RenderPipelineDescriptor, key: UiMaterialKey<Self>) {
        if key.bind_group_data.additive {
            set_blend(descriptor, ADDITIVE);
        }
    }
}

/// A runtime `Text` object's box; game code sets `text` and [`MenuPlugin`] re-lays the glyphs.
#[derive(Component, Clone)]
pub struct MenuText {
    pub text: String,
    pub color: Color,
    font: Arc<Font>,
    align: u8,
    size: Vec2,
    scale: f32,
}

/// Draws [`MenuSprite`]s, [`MenuGauge`]s and [`MenuText`]s; layouts need it in the app.
pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "additive_sprite.wgsl");
        embedded_asset!(app, "gauge_sprite.wgsl");
        app.add_plugins((UiMaterialPlugin::<AdditiveSprite>::default(), UiMaterialPlugin::<GaugeSprite>::default()))
            .add_systems(PostUpdate, (draw_sprites, draw_gauges, draw_texts));
    }
}

fn draw_gauges(mut commands: Commands, mut materials: ResMut<Assets<GaugeSprite>>, gauges: Query<(Entity, &MenuGauge, Option<&MaterialNode<GaugeSprite>>), Changed<MenuGauge>>) {
    for (e, g, material) in &gauges {
        if let Some(mut m) = material.and_then(|m| materials.get_mut(&m.0)) {
            *m = GaugeSprite::from(g);
            continue;
        }
        commands.entity(e).insert(MaterialNode(materials.add(GaugeSprite::from(g))));
    }
}

fn draw_texts(mut commands: Commands, texts: Query<(Entity, &MenuText), Changed<MenuText>>) {
    for (e, t) in &texts {
        commands.entity(e).despawn_related::<Children>();
        write_glyphs(&mut commands, e, &t.font, &t.text, t.align, t.size, t.scale, t.color);
    }
}

/// Lays `string` out in the `size` box per the DRB text `align` (low 2 bits: 0 left, 1 right,
/// 2 centre; bits 2-3: 0 top, 1 bottom, 2 middle).
#[allow(clippy::too_many_arguments)]
fn write_glyphs(commands: &mut Commands, boxed: Entity, f: &Font, string: &str, align: u8, size: Vec2, scale: f32, color: Color) {
    let (quads, extent) = text::layout(&f.ccm, &f.sheet_size, string, scale);
    let x = match align & 3 {
        1 => size.x - extent.x,
        2 => (size.x - extent.x) / 2.0,
        _ => 0.0,
    };
    let y = match align & 0xc {
        8 => (size.y - extent.y) / 2.0,
        4 => size.y - extent.y,
        _ => 0.0,
    };
    text::spawn_glyphs(commands, f, &quads, Vec2::new(x, y), boxed, color);
}

fn draw_sprites(
    mut commands: Commands,
    mut materials: ResMut<Assets<AdditiveSprite>>,
    sprites: Query<(Entity, &MenuSprite, Option<&MaterialNode<AdditiveSprite>>), Changed<MenuSprite>>,
) {
    for (e, s, material) in &sprites {
        if !s.additive {
            commands.entity(e).insert(ImageNode { image: s.image.clone(), color: s.color, rect: Some(s.rect), flip_x: s.flip_x, flip_y: s.flip_y, ..Default::default() });
            continue;
        }
        if let Some(mut m) = material.and_then(|m| materials.get_mut(&m.0)) {
            *m = AdditiveSprite::from(s);
            continue;
        }
        commands.entity(e).insert(MaterialNode(materials.add(AdditiveSprite::from(s))));
    }
}

/// RGBA `c` times `tint`, per channel (0xff = 1).
fn modulate(c: u32, tint: u32) -> u32 {
    let (c, t) = (c.to_be_bytes(), tint.to_be_bytes());
    u32::from_be_bytes(std::array::from_fn(|i| (c[i] as u32 * t[i] as u32 / 255) as u8))
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

fn texels(uv: [i16; 4]) -> Rect {
    Rect::new(uv[0] as f32, uv[1] as f32, uv[2] as f32, uv[3] as f32)
}

/// A sprite's node and its quarter turns (flag bits 10-11).
fn sprite_node(rect: [i16; 4], flags: u16, scale: f32) -> (Node, UiTransform) {
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
    (n, transform)
}

/// Spawns dialog `index` with its layout origin at `origin` (logical pixels: the top-left for
/// screen dialogs, the center for ones laid out around (0,0) like `ACV_FE_LockSightCenter`),
/// layout pixels times `scale`. Returns the root, tagged [`MenuRoot`].
pub fn spawn(commands: &mut Commands, layout: &Layout, index: usize, origin: Vec2, scale: f32) -> Entity {
    let size = layout.drb.dialogs.get(index).map_or([0, 0], |d| d.size);
    let mut root = node([0, 0, size[0] as i16, size[1] as i16], scale);
    root.left = px(origin.x);
    root.top = px(origin.y);
    let root = commands.spawn((MenuRoot, root)).id();
    spawn_dialog(commands, layout, index, root, scale, u32::MAX, 0);
    root
}

/// `tint` is the product of the enclosing `Dialog` shapes' colors. That a Dialog's color
/// modulates its sub-dialog is read off the layouts, not traced: `ACV_FE_LockSightCenter` puts
/// the white `FE_font_base` plates behind its digits through `000000ff` Dialogs.
fn spawn_dialog(commands: &mut Commands, layout: &Layout, index: usize, parent: Entity, scale: f32, tint: u32, depth: usize) {
    let Some(dialog) = layout.drb.dialogs.get(index) else { return };
    if depth > 16 {
        return;
    }
    let rgba = |c: u32| {
        let [r, g, b, a] = modulate(c, tint).to_be_bytes();
        Color::srgba_u8(r, g, b, a)
    };
    for o in &dialog.objects {
        let spawned = match &o.shape {
            &Shape::Sprite { rect, uv, texture: Some(t), flags, color } => {
                let Some(Some(image)) = layout.textures.get(t as usize).cloned() else { continue };
                let (n, transform) = sprite_node(rect, flags, scale);
                let sprite = MenuSprite { image, rect: texels(uv), color: rgba(color), flip_x: flags & 0x100 != 0, flip_y: flags & 0x200 != 0, additive: flags & 0xff == 2 };
                commands.spawn((n, transform, sprite, ChildOf(parent))).id()
            }
            &Shape::AlphaAnimSprite { rect, uv, texture: Some(t), flags, color, mask_uv, mask: Some(m), mirror } => {
                let (Some(Some(image)), Some(Some(mask))) = (layout.textures.get(t as usize).cloned(), layout.textures.get(m as usize).cloned()) else { continue };
                let (n, transform) = sprite_node(rect, flags, scale);
                let gauge = MenuGauge {
                    image,
                    rect: texels(uv),
                    mask,
                    mask_rect: texels(mask_uv),
                    color: rgba(color),
                    flip_x: flags & 0x100 != 0,
                    flip_y: flags & 0x200 != 0,
                    mirror,
                    additive: flags & 0xff == 2,
                    fill: 1.0,
                };
                commands.spawn((n, transform, gauge, ChildOf(parent))).id()
            }
            &Shape::MonoRect { rect, color, .. } => commands.spawn((node(rect, scale), BackgroundColor(rgba(color)), ChildOf(parent))).id(),
            &Shape::MonoFrame { rect, flags, color } => {
                let mut n = node(rect, scale);
                n.border = UiRect::all(px((flags & 0xff).max(1) as f32 * scale));
                commands.spawn((n, BorderColor::all(rgba(color)), ChildOf(parent))).id()
            }
            &Shape::GouraudRect { rect, colors, .. } => {
                let gradient = LinearGradient::to_bottom(vec![ColorStop::auto(rgba(colors[0])), ColorStop::auto(rgba(colors[2]))]);
                commands.spawn((node(rect, scale), BackgroundGradient::from(gradient), ChildOf(parent))).id()
            }
            &Shape::GouraudFrame { rect, flags, colors } => {
                let mut n = node(rect, scale);
                n.border = UiRect::all(px((flags & 0xff).max(1) as f32 * scale));
                commands.spawn((n, BorderColor::all(rgba(colors[0])), ChildOf(parent))).id()
            }
            Shape::Text { rect, color, font, align, source, .. } => {
                let Some(f) = layout.fonts.get(font) else { continue };
                let size = Vec2::new((rect[2] - rect[0]).abs() as f32, (rect[3] - rect[1]).abs() as f32) * scale;
                let string = match source {
                    TextSource::Static(s) => Some(s.clone()),
                    TextSource::Message { bank: 1, id } => layout.messages.as_ref().and_then(|m| m.get(*id as i32)).map(str::to_string),
                    TextSource::Runtime { .. } => {
                        let mut string = if layout.placeholders { o.name.clone() } else { String::new() };
                        if layout.placeholders && string.encode_utf16().any(|c| f.ccm.glyph(c).is_none()) {
                            string = "000".into();
                        }
                        let text = MenuText { text: string, color: rgba(*color), font: f.clone(), align: *align, size, scale };
                        commands.entity(parent).with_child((node(*rect, scale), text, MenuObject(o.name.clone())));
                        continue;
                    }
                    _ => layout.placeholders.then(|| o.name.clone()),
                };
                let Some(mut string) = string else { continue };
                if layout.placeholders && string == o.name && string.encode_utf16().any(|c| f.ccm.glyph(c).is_none()) {
                    string = "000".into();
                }
                let boxed = commands.spawn((node(*rect, scale), ChildOf(parent))).id();
                write_glyphs(commands, boxed, f, &string, *align, size, scale, rgba(*color));
                boxed
            }
            &Shape::Dialog { rect, dialog: Some(d), color, .. } => {
                let child = commands.spawn((node(rect, scale), ChildOf(parent))).id();
                spawn_dialog(commands, layout, d as usize, child, scale, modulate(color, tint), depth + 1);
                child
            }
            _ => continue,
        };
        commands.entity(spawned).insert(MenuObject(o.name.clone()));
    }
}
