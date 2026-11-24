//! Scalable ACVD-authentic GUI kit built from retail disc chrome (`lang/en/menu/assemble.drb.dcx`,
//! `assemble.tpf.dcx`, and `font/fontdef.xml` CCM bitmap fonts).
//!
//! Unlike fixed 1280×720 DRB dialog spawns ([`crate::menu::spawn`]), every widget here participates
//! in Bevy Flexbox layout while drawing with the real game's fonts, colors, gradients, corner
//! reticles, stepped traces, shaded buttons, stat rows, and capacity gauges.
//!
//! Requires [`crate::menu::MenuPlugin`] in the Bevy `App` so [`GlyphNode`]s (Catmull-Rom bicubic
//! `glyph_sprite.wgsl`) and [`MenuSprite`]s render.

use std::sync::Arc;

use acvd_formats::vfs::Disc;
use anyhow::Result;
use bevy::asset::{Assets, Handle};
use bevy::color::Color;
use bevy::image::Image;
use bevy::math::{Rect, Vec2};
use bevy::prelude::*;
use bevy::ui::{
    px, AlignItems, BackgroundGradient, ColorStop, FlexDirection, JustifyContent, LinearGradient,
    PositionType, UiRect, Val,
};

use crate::menu::{self, Layout, MenuSprite};
use crate::text::{self, Font};

/// Retail DRB layout that supplies the shared menu chrome sprites and fonts.
pub const KIT_DRB_PATH: &str = "lang/en/menu/assemble.drb.dcx";

/// Authentic ACVD menu color tokens extracted directly from `lang/en/menu/assemble.drb.dcx`.
pub mod palette {
    use bevy::color::Color;

    const fn rgba_u32(c: u32) -> Color {
        let [r, g, b, a] = c.to_be_bytes();
        Color::srgba(
            r as f32 / 255.0,
            g as f32 / 255.0,
            b as f32 / 255.0,
            a as f32 / 255.0,
        )
    }

    /// Warm military sage off-white (`0xA0A082FF`, `@ParamLine/@Name`, `@CategoryName`).
    pub const TEXT_PRIMARY: Color = rgba_u32(0xA0A0_82FF);
    /// Dimmed sage sub-label (`0xA0A082A0`, `parts_view_text` / `assemble_view_text`).
    pub const TEXT_MUTED: Color = rgba_u32(0xA0A0_82A0);
    /// High-contrast warm ivory for focused/selected labels.
    pub const TEXT_BRIGHT: Color = rgba_u32(0xECEE_DCFF);
    /// Alert orange-red (`0xFF4100FF`, `@TotalWeightText` / `@TotalEnText`).
    pub const TEXT_ALERT: Color = rgba_u32(0xFF41_00FF);
    /// Stat improvement highlight (ACVD cyan-blue).
    pub const TEXT_GOOD: Color = Color::srgb(0.35, 0.85, 0.98);
    /// Stat degradation highlight (ACVD coral-red).
    pub const TEXT_BAD: Color = Color::srgb(0.98, 0.36, 0.30);

    /// Main translucent panel fill (`0x000A0AC8`, `汎用ベース1` dialog 106).
    pub const BG_PANEL: Color = rgba_u32(0x000A_0AC8);
    /// Darker bottom stop for shaded tactical panels.
    pub const BG_PANEL_BOT: Color = rgba_u32(0x0004_06DC);
    /// Translucent header/helpline/alternating bar (`0x000A0A80`, `categry_base`, `2Line_Helpline`).
    pub const BG_BAR: Color = rgba_u32(0x000A_0A80);
    /// Dark slate-teal stat row / title plate (`0x00283C80`, `@ParamLine/shop_param_pleat1`, `Title_Plate`).
    pub const BG_ROW: Color = rgba_u32(0x0028_3C80);
    /// Transparent edge stop for `Title_Plate` horizontal fade (`0x00283C00`).
    pub const BG_ROW_TRANSPARENT: Color = rgba_u32(0x0028_3C00);
    /// Normal shaded button top fill (`0x00142880`, `Button_base/OK_button` dialog 108).
    pub const BG_BUTTON_TOP: Color = rgba_u32(0x001C_3098);
    /// Normal shaded button bottom fill.
    pub const BG_BUTTON_BOT: Color = rgba_u32(0x000B_16B8);
    /// Disabled button fill.
    pub const BG_BUTTON_DISABLED: Color = rgba_u32(0x000A_0A58);

    /// Subtle warm sage hairline (`0xA0A0823C`, `@WeightFrame`, `LinePatrs_UP`).
    pub const BORDER_SUBTLE: Color = rgba_u32(0xA0A0_823C);
    /// Cyan-sage button/frame hairline (`0xA0C0B564`, `Button_base/outline`, `Shop_Base1/outline`).
    pub const BORDER_BUTTON: Color = rgba_u32(0xA0C0_B564);
    /// Corner bracket tint (`0xA0C0B5B4`, `Shop_Base1/Corner_Parts`).
    pub const CORNER_BRACKET: Color = rgba_u32(0xA0C0_B5B4);
    /// Faint tactical reticle tint (boosted slightly from `base_window_L` `0xA0A0820F` for crisp 1080p visibility).
    pub const TRACE_RETICLE: Color = rgba_u32(0xA0A0_8248);
    /// Crisp horizontal trace line (`0xB4B4B480`, `Help_Line_UP` dialog 80).
    pub const TRACE_BRIGHT: Color = rgba_u32(0xB4B4_B480);
    /// Square section marker (`0xA0A08280`, `txt_block_1`).
    pub const SQUARE_MARK: Color = rgba_u32(0xA0A0_8280);

    /// ACVD signature amber focus border (`0xFF8000C8`, `パーツ選択カーソル`, `AC5_選択枠`).
    pub const ACCENT_AMBER: Color = rgba_u32(0xFF80_00C8);
    /// Amber selection fill top (`0xFF800040`, `パーツ選択カーソル/BG`).
    pub const ACCENT_AMBER_FILL_TOP: Color = rgba_u32(0xFF80_0048);
    /// Amber selection fill bottom (richer shaded amber).
    pub const ACCENT_AMBER_FILL_BOT: Color = rgba_u32(0xFF80_0090);
    /// Focus arrow tint (`0xFF5A14FF`, `Forcus` dialog 105).
    pub const ACCENT_ARROW: Color = rgba_u32(0xFF6A_18FF);

    /// Active tab/filter teal border (matches `ALL TYPES` / selected slot in `assemble.drb.dcx`).
    pub const ACCENT_TEAL: Color = Color::srgba(0.28, 0.78, 0.59, 0.95);
    /// Active tab/filter translucent teal fill.
    pub const ACCENT_TEAL_FILL_TOP: Color = Color::srgba(0.14, 0.44, 0.36, 0.48);
    pub const ACCENT_TEAL_FILL_BOT: Color = Color::srgba(0.08, 0.26, 0.22, 0.68);

    /// Metallic grey-to-black gauge gradient (`@WeightBar` / `@EnergyBar`: `0x808080FF` -> `0x000000FF`).
    pub const GAUGE_TOP: Color = rgba_u32(0x8080_80FF);
    pub const GAUGE_BOT: Color = rgba_u32(0x1414_14FF);
    /// Alert gauge gradient (overweight / EN shortage).
    pub const GAUGE_ALERT_TOP: Color = Color::srgb(0.96, 0.32, 0.16);
    pub const GAUGE_ALERT_BOT: Color = Color::srgb(0.50, 0.10, 0.06);
    /// Gauge recommend threshold `I` marker (`0xC0FFFFFF`, `Gauge_parts` dialog 65).
    pub const GAUGE_MARKER: Color = rgba_u32(0xC0FF_FFFF);
}

/// Retail CCM bitmap font roles from `font/fontdef.xml` (as used by `assemble.drb.dcx`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontRole {
    /// Section/screen header font (`fontdef` ID 18, `@CategoryName`).
    Header,
    /// Primary body/button/stat font (`fontdef` ID 17, `@ParamLine`, `2Line_Helpline`).
    Body,
    /// Compact micro-label font (`fontdef` ID 21, `PARTS VIEW` / `ASSEMBLY VIEW`).
    Small,
}

/// One reusable sprite region from a retail DRB/TPF atlas.
#[derive(Clone)]
pub struct SpriteSlice {
    pub image: Handle<Image>,
    pub rect: Rect,
}

impl SpriteSlice {
    pub fn size(&self) -> Vec2 {
        self.rect.size()
    }

    /// Spawns this sprite slice as a [`MenuSprite`] child of `parent`.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        &self,
        commands: &mut Commands,
        parent: Entity,
        node: Node,
        color: Color,
        flip_x: bool,
        flip_y: bool,
        additive: bool,
    ) -> Entity {
        commands
            .spawn((
                node,
                MenuSprite {
                    image: self.image.clone(),
                    rect: self.rect,
                    color,
                    flip_x,
                    flip_y,
                    additive,
                },
                ChildOf(parent),
            ))
            .id()
    }
}

/// Visual chrome style for [`AcvdUiKit::spawn_panel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelStyle {
    /// Deep teal-black shaded container with retail `base_window_L` corner reticles and tactical L-traces.
    Tactical,
    /// `Shop_Base1` container with cyan-sage hairline border and 4 crisp `Corner_Parts` brackets.
    Framed,
    /// Translucent header/helpline bar (`BG_BAR`) with top & bottom subtle sage hairlines.
    Bar,
    /// Lighter slate-teal inset plate (`BG_ROW`) for inner card groupings.
    Subtle,
}

/// Interactive visual state for [`AcvdUiKit::spawn_button`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonState {
    Normal,
    Selected,
    Active,
    Disabled,
}

/// Horizontal trace divider style for [`AcvdUiKit::spawn_trace`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceStyle {
    /// `Help_Line_UP` 1px crisp horizontal line with vertical end ticks.
    Simple,
    /// `LinePatrs_UP` stepped horizontal trace using the real disc diagonal step sprites.
    Stepped,
}

/// Direction for [`AcvdUiKit::spawn_arrow`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrowDir {
    Left,
    Right,
    Up,
    Down,
}

/// Optional comparison delta shown on the right side of [`AcvdUiKit::spawn_stat_row`].
#[derive(Debug, Clone, Copy)]
pub struct StatDelta<'a> {
    pub text: &'a str,
    /// `> 0` = better (`TEXT_GOOD`), `< 0` = worse (`TEXT_BAD`), `0` = neutral (`TEXT_PRIMARY`).
    pub cmp: i8,
}

/// Entities returned by [`AcvdUiKit::spawn_slider_row`] so callers can attach their own
/// click-action components to the row itself and its `<` / `>` stepper buttons.
#[derive(Debug, Clone, Copy)]
pub struct SliderRowEntities {
    pub row: Entity,
    pub track: Entity,
    pub dec_btn: Entity,
    pub inc_btn: Entity,
}

/// Loaded retail ACVD UI kit: CCM bitmap fonts + reusable DRB chrome sprites.
///
/// Insert once as a Bevy [`Resource`] via [`AcvdUiKit::load`] or [`AcvdUiKit::from_layout`].
#[derive(Resource, Clone, Default)]
pub struct AcvdUiKit {
    pub font_header: Option<Arc<Font>>,
    pub font_body: Option<Arc<Font>>,
    pub font_small: Option<Arc<Font>>,

    /// `base_window_L` outer corner reticle (`Parts_01`, `uv=[35,4,48,19]`).
    pub reticle_corner: Option<SpriteSlice>,
    /// `base_window_L` inner corner vertical accent (`Parts_01`, `uv=[1,22,13,39]`).
    pub reticle_side: Option<SpriteSlice>,
    /// `Shop_Base1` 5×5 corner bracket (`ACV_FE_Locksight_02`, `uv=[326,398,331,403]`).
    pub corner_bracket: Option<SpriteSlice>,
    /// `LinePatrs_UP` stepped trace diagonal (`ACV_frame`, `uv=[93,286,136,290]`).
    pub trace_step: Option<SpriteSlice>,
    /// `Forcus` selection arrow (`ACV_MENU_parts1`, `uv=[214,7,231,33]`).
    pub focus_arrow: Option<SpriteSlice>,
    /// `@PartsNormal/@Arrow_Left` horizontal nav arrow (`Parts_01`, `uv=[3,3,12,19]`).
    pub arrow_horiz: Option<SpriteSlice>,
    /// `@PartsNormal/ScrollTop` vertical nav arrow (`Parts_01`, `uv=[17,6,33,15]`).
    pub arrow_vert: Option<SpriteSlice>,
}

fn find_tex(layout: &Layout, name: &str) -> Option<Handle<Image>> {
    layout
        .drb
        .textures
        .iter()
        .enumerate()
        .find(|(_, t)| t.name.eq_ignore_ascii_case(name))
        .and_then(|(i, _)| layout.textures.get(i).and_then(|h| h.clone()))
}

fn slice(image: Option<Handle<Image>>, u0: f32, v0: f32, u1: f32, v1: f32) -> Option<SpriteSlice> {
    image.map(|image| SpriteSlice {
        image,
        rect: Rect::new(u0, v0, u1, v1),
    })
}

impl AcvdUiKit {
    /// Loads the kit directly from the 360 disc (`lang/en/menu/assemble.drb.dcx` + `assemble.tpf.dcx`
    /// + `font/fontdef.xml`).
    pub fn load(disc: &Disc, images: &mut Assets<Image>) -> Result<Self> {
        let layout = menu::load(disc, KIT_DRB_PATH, images)?;
        Ok(Self::from_layout(&layout))
    }

    /// Extracts the kit's fonts and chrome sprites from an already-loaded `assemble.drb.dcx` [`Layout`]
    /// with zero extra disc reads or GPU texture allocations.
    pub fn from_layout(layout: &Layout) -> Self {
        let font_body = layout.fonts.get(&17).cloned().or_else(|| layout.fonts.values().next().cloned());
        let font_header = layout.fonts.get(&18).cloned().or_else(|| font_body.clone());
        let font_small = layout.fonts.get(&21).cloned().or_else(|| font_body.clone());

        let parts_01 = find_tex(layout, "Parts_01");
        let locksight = find_tex(layout, "ACV_FE_Locksight_02");
        let frame = find_tex(layout, "ACV_frame");
        let menu_parts = find_tex(layout, "ACV_MENU_parts1")
            .or_else(|| find_tex(layout, "ACV_MENU_parts01"));

        Self {
            font_header,
            font_body,
            font_small,
            reticle_corner: slice(parts_01.clone(), 35.0, 4.0, 48.0, 19.0),
            reticle_side: slice(parts_01.clone(), 1.0, 22.0, 13.0, 39.0),
            corner_bracket: slice(locksight, 326.0, 398.0, 331.0, 403.0),
            trace_step: slice(frame, 93.0, 286.0, 136.0, 290.0),
            focus_arrow: slice(menu_parts, 214.0, 7.0, 231.0, 33.0),
            arrow_horiz: slice(parts_01.clone(), 3.0, 3.0, 12.0, 19.0),
            arrow_vert: slice(parts_01, 17.0, 6.0, 33.0, 15.0),
        }
    }

    /// Resolves the best loaded CCM font for `(role, text)`, falling back to `Body` if a specialized
    /// font (`Header` / `Small`) is missing glyphs present in `text`, or when `Small` (`e16_ext`,
    /// authored for all-caps micro-labels and carrying uneven lowercase bearings) is given lowercase text.
    pub fn resolve_font(&self, role: FontRole, text_str: &str) -> Option<&Arc<Font>> {
        if role == FontRole::Small && text_str.chars().any(|c| c.is_ascii_lowercase()) {
            return self.font_body.as_ref().or(self.font_small.as_ref());
        }
        let primary = match role {
            FontRole::Header => self.font_header.as_ref(),
            FontRole::Body => self.font_body.as_ref(),
            FontRole::Small => self.font_small.as_ref(),
        };
        if let Some(f) = primary {
            let all_present = text_str
                .encode_utf16()
                .filter(|&u| u != b'\n' as u16 && u != b' ' as u16)
                .all(|u| f.ccm.glyph(u).is_some());
            if all_present {
                return Some(f);
            }
        }
        self.font_body.as_ref().or(primary)
    }

    /// Measures `text_str` in logical pixels at `scale` using the resolved disc CCM font.
    pub fn measure_text(&self, role: FontRole, text_str: &str, scale: f32) -> Vec2 {
        let Some(f) = self.resolve_font(role, text_str) else {
            let px_h = match role {
                FontRole::Header => 18.0,
                FontRole::Body => 14.0,
                FontRole::Small => 11.0,
            } * scale;
            return Vec2::new(text_str.len() as f32 * px_h * 0.6, px_h);
        };
        text::layout(&f.ccm, &f.sheet_size, text_str, scale).1
    }

    /// Spawns flexbox-compatible ACVD disc bitmap text (`PositionType::Relative`, Catmull-Rom
    /// bicubic [`GlyphNode`]s) inside `parent`. Falls back to Bevy `Text` only when no disc font is loaded.
    pub fn spawn_text(
        &self,
        commands: &mut Commands,
        parent: Entity,
        role: FontRole,
        text_str: &str,
        scale: f32,
        color: Color,
    ) -> Entity {
        if let Some(f) = self.resolve_font(role, text_str) {
            let (quads, size) = text::layout(&f.ccm, &f.sheet_size, text_str, scale);
            let boxed = commands
                .spawn((
                    Node {
                        position_type: PositionType::Relative,
                        width: px(size.x.ceil().max(1.0)),
                        height: px(size.y.ceil().max(1.0)),
                        flex_shrink: 0.0,
                        ..Default::default()
                    },
                    ChildOf(parent),
                ))
                .id();
            text::spawn_glyphs(commands, f, &quads, Vec2::ZERO, boxed, color);
            return boxed;
        }

        let font_size = match role {
            FontRole::Header => 17.0,
            FontRole::Body => 13.0,
            FontRole::Small => 10.5,
        } * scale;
        commands
            .spawn((
                Text::new(text_str.to_string()),
                TextFont {
                    font_size: FontSize::Px(font_size),
                    ..Default::default()
                },
                TextColor(color),
                ChildOf(parent),
            ))
            .id()
    }

    /// Spawns a scalable ACVD container panel with retail shading, hairlines, and corner chrome.
    ///
    /// Decorative corner reticles/brackets use `PositionType::Absolute`, so child widgets flow
    /// naturally inside the returned entity's flexbox layout.
    pub fn spawn_panel(
        &self,
        commands: &mut Commands,
        parent: Option<Entity>,
        style: PanelStyle,
        mut node: Node,
        scale: f32,
    ) -> Entity {
        let bw = (1.0 * scale).max(1.0);
        let entity = match style {
            PanelStyle::Tactical => {
                node.border = UiRect::all(px(bw));
                commands
                    .spawn((
                        node,
                        BackgroundGradient::from(LinearGradient::to_bottom(vec![
                            ColorStop::auto(palette::BG_PANEL),
                            ColorStop::auto(palette::BG_PANEL_BOT),
                        ])),
                        BorderColor::all(palette::BORDER_SUBTLE),
                    ))
                    .id()
            }
            PanelStyle::Framed => {
                node.border = UiRect::all(px(bw));
                commands
                    .spawn((
                        node,
                        BackgroundGradient::from(LinearGradient::to_bottom(vec![
                            ColorStop::auto(palette::BG_PANEL),
                            ColorStop::auto(palette::BG_PANEL_BOT),
                        ])),
                        BorderColor::all(palette::BORDER_BUTTON),
                    ))
                    .id()
            }
            PanelStyle::Bar => {
                node.border = UiRect {
                    top: px(bw),
                    bottom: px(bw),
                    ..Default::default()
                };
                commands
                    .spawn((
                        node,
                        BackgroundColor(palette::BG_BAR),
                        BorderColor::all(palette::BORDER_SUBTLE),
                    ))
                    .id()
            }
            PanelStyle::Subtle => {
                node.border = UiRect::all(px(bw));
                commands
                    .spawn((
                        node,
                        BackgroundColor(palette::BG_ROW),
                        BorderColor::all(palette::BORDER_SUBTLE),
                    ))
                    .id()
            }
        };

        if let Some(p) = parent {
            commands.entity(entity).insert(ChildOf(p));
        }

        // Pin real disc corner reticles/brackets to all 4 corners without disturbing flex children.
        match style {
            PanelStyle::Tactical => {
                if let Some(corner) = &self.reticle_corner {
                    let s = corner.size() * scale;
                    let inset = (4.0 * scale).round();
                    let placements = [
                        (Some(inset), None, Some(inset), None, false, false),
                        (None, Some(inset), Some(inset), None, true, false),
                        (Some(inset), None, None, Some(inset), false, true),
                        (None, Some(inset), None, Some(inset), true, true),
                    ];
                    for (left, right, top, bottom, fx, fy) in placements {
                        corner.spawn(
                            commands,
                            entity,
                            Node {
                                position_type: PositionType::Absolute,
                                left: left.map(px).unwrap_or(Val::Auto),
                                right: right.map(px).unwrap_or(Val::Auto),
                                top: top.map(px).unwrap_or(Val::Auto),
                                bottom: bottom.map(px).unwrap_or(Val::Auto),
                                width: px(s.x.round()),
                                height: px(s.y.round()),
                                ..Default::default()
                            },
                            palette::TRACE_RETICLE,
                            fx,
                            fy,
                            false,
                        );
                    }
                }
                // Faint tactical corner L-traces (from `base_window_L`).
                let arm = (32.0 * scale).round();
                let t = (1.0 * scale).max(1.0);
                let off = (6.0 * scale).round();
                for (left, right, top, bottom) in [
                    (Some(off), None, Some(off), None),
                    (None, Some(off), Some(off), None),
                    (Some(off), None, None, Some(off)),
                    (None, Some(off), None, Some(off)),
                ] {
                    commands.spawn((
                        Node {
                            position_type: PositionType::Absolute,
                            left: left.map(px).unwrap_or(Val::Auto),
                            right: right.map(px).unwrap_or(Val::Auto),
                            top: top.map(px).unwrap_or(Val::Auto),
                            bottom: bottom.map(px).unwrap_or(Val::Auto),
                            width: px(arm),
                            height: px(t),
                            ..Default::default()
                        },
                        BackgroundColor(palette::TRACE_RETICLE),
                        ChildOf(entity),
                    ));
                }
            }
            PanelStyle::Framed => {
                if let Some(br) = &self.corner_bracket {
                    let s = br.size() * scale;
                    let off = (-2.0 * scale).round();
                    for (left, right, top, bottom, fx, fy) in [
                        (Some(off), None, Some(off), None, false, false),
                        (None, Some(off), Some(off), None, true, false),
                        (Some(off), None, None, Some(off), false, true),
                        (None, Some(off), None, Some(off), true, true),
                    ] {
                        br.spawn(
                            commands,
                            entity,
                            Node {
                                position_type: PositionType::Absolute,
                                left: left.map(px).unwrap_or(Val::Auto),
                                right: right.map(px).unwrap_or(Val::Auto),
                                top: top.map(px).unwrap_or(Val::Auto),
                                bottom: bottom.map(px).unwrap_or(Val::Auto),
                                width: px(s.x.round()),
                                height: px(s.y.round()),
                                ..Default::default()
                            },
                            palette::CORNER_BRACKET,
                            fx,
                            fy,
                            true,
                        );
                    }
                }
            }
            PanelStyle::Bar | PanelStyle::Subtle => {}
        }

        entity
    }

    /// Spawns a retail `Title_Plate`-style section header with `■` square marker, disc header font,
    /// optional right-aligned tag, and crisp bottom trace.
    pub fn spawn_header(
        &self,
        commands: &mut Commands,
        parent: Entity,
        title: &str,
        right_tag: Option<&str>,
        scale: f32,
    ) -> Entity {
        let wrap = commands
            .spawn((
                Node {
                    width: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    row_gap: px((3.0 * scale).round()),
                    ..Default::default()
                },
                ChildOf(parent),
            ))
            .id();

        let plate = commands
            .spawn((
                Node {
                    width: Val::Percent(100.0),
                    padding: UiRect::axes(px((8.0 * scale).round()), px((5.0 * scale).round())),
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::SpaceBetween,
                    column_gap: px((8.0 * scale).round()),
                    ..Default::default()
                },
                BackgroundGradient::from(LinearGradient::to_right(vec![
                    ColorStop::new(palette::BG_ROW, Val::Percent(0.0)),
                    ColorStop::new(palette::BG_ROW, Val::Percent(84.0)),
                    ColorStop::new(palette::BG_ROW_TRANSPARENT, Val::Percent(100.0)),
                ])),
                ChildOf(wrap),
            ))
            .id();

        let left = commands
            .spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: px((7.0 * scale).round()),
                    ..Default::default()
                },
                ChildOf(plate),
            ))
            .id();

        // `txt_block_1` 7×7 sage square marker
        let sq = (7.0 * scale).round().max(4.0);
        commands.spawn((
            Node {
                width: px(sq),
                height: px(sq),
                flex_shrink: 0.0,
                ..Default::default()
            },
            BackgroundColor(palette::SQUARE_MARK),
            ChildOf(left),
        ));

        self.spawn_text(commands, left, FontRole::Header, title, scale, palette::TEXT_PRIMARY);

        if let Some(tag) = right_tag {
            self.spawn_text(commands, plate, FontRole::Small, tag, scale, palette::TEXT_MUTED);
        }

        self.spawn_trace(commands, wrap, TraceStyle::Simple, scale);
        wrap
    }

    /// Spawns a scalable horizontal trace divider (`Help_Line_UP` or `LinePatrs_UP` stepped trace).
    pub fn spawn_trace(
        &self,
        commands: &mut Commands,
        parent: Entity,
        style: TraceStyle,
        scale: f32,
    ) -> Entity {
        let t = (1.0 * scale).max(1.0);
        match style {
            TraceStyle::Simple => {
                let tick_h = (5.0 * scale).round().max(3.0);
                let row = commands
                    .spawn((
                        Node {
                            width: Val::Percent(100.0),
                            height: px(tick_h),
                            position_type: PositionType::Relative,
                            ..Default::default()
                        },
                        ChildOf(parent),
                    ))
                    .id();
                // Horizontal line
                commands.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: px(0.0),
                        right: px(0.0),
                        top: px(0.0),
                        height: px(t),
                        ..Default::default()
                    },
                    BackgroundColor(palette::TRACE_BRIGHT),
                    ChildOf(row),
                ));
                // Left & right end ticks
                for is_right in [false, true] {
                    commands.spawn((
                        Node {
                            position_type: PositionType::Absolute,
                            left: if is_right { Val::Auto } else { px(0.0) },
                            right: if is_right { px(0.0) } else { Val::Auto },
                            top: px(t),
                            width: px(t),
                            height: px((tick_h - t).max(1.0)),
                            ..Default::default()
                        },
                        BackgroundColor(palette::TRACE_BRIGHT),
                        ChildOf(row),
                    ));
                }
                row
            }
            TraceStyle::Stepped => {
                let h = (4.0 * scale).round().max(3.0);
                let thin = (2.0 * scale).round().max(1.0);
                let row = commands
                    .spawn((
                        Node {
                            width: Val::Percent(100.0),
                            height: px(h),
                            flex_direction: FlexDirection::Row,
                            align_items: AlignItems::FlexStart,
                            ..Default::default()
                        },
                        ChildOf(parent),
                    ))
                    .id();
                // Left thick rail
                commands.spawn((
                    Node {
                        flex_grow: 1.0,
                        height: px(h),
                        ..Default::default()
                    },
                    BackgroundColor(palette::BORDER_SUBTLE),
                    ChildOf(row),
                ));
                if let Some(step) = &self.trace_step {
                    let s = step.size() * scale;
                    step.spawn(
                        commands,
                        row,
                        Node {
                            width: px(s.x.round()),
                            height: px(h),
                            flex_shrink: 0.0,
                            ..Default::default()
                        },
                        palette::BORDER_SUBTLE,
                        false,
                        false,
                        true,
                    );
                }
                // Center thin bridge
                commands.spawn((
                    Node {
                        flex_grow: 2.0,
                        height: px(thin),
                        ..Default::default()
                    },
                    BackgroundColor(palette::BORDER_SUBTLE),
                    ChildOf(row),
                ));
                if let Some(step) = &self.trace_step {
                    let s = step.size() * scale;
                    step.spawn(
                        commands,
                        row,
                        Node {
                            width: px(s.x.round()),
                            height: px(h),
                            flex_shrink: 0.0,
                            ..Default::default()
                        },
                        palette::BORDER_SUBTLE,
                        true,
                        false,
                        true,
                    );
                }
                // Right thick rail
                commands.spawn((
                    Node {
                        flex_grow: 1.0,
                        height: px(h),
                        ..Default::default()
                    },
                    BackgroundColor(palette::BORDER_SUBTLE),
                    ChildOf(row),
                ));
                row
            }
        }
    }

    /// Spawns a scalable ACVD shaded menu button/row (`Button_base` / `AC5_選択枠` / `Forcus`).
    ///
    /// Returns the button [`Entity`] so callers can attach `Button`, `UiRow(i)`, or `UiAction`.
    pub fn spawn_button(
        &self,
        commands: &mut Commands,
        parent: Entity,
        label: &str,
        right_label: Option<&str>,
        state: ButtonState,
        scale: f32,
    ) -> Entity {
        let bw = (1.0 * scale).max(1.0);
        let pad_x = (10.0 * scale).round();
        let pad_y = (5.0 * scale).round();

        let (top_c, bot_c, border_c, text_c, right_c) = match state {
            ButtonState::Normal => (
                palette::BG_BUTTON_TOP,
                palette::BG_BUTTON_BOT,
                palette::BORDER_BUTTON,
                palette::TEXT_PRIMARY,
                palette::TEXT_MUTED,
            ),
            ButtonState::Selected => (
                palette::ACCENT_AMBER_FILL_TOP,
                palette::ACCENT_AMBER_FILL_BOT,
                palette::ACCENT_AMBER,
                palette::TEXT_BRIGHT,
                palette::TEXT_BRIGHT,
            ),
            ButtonState::Active => (
                palette::ACCENT_TEAL_FILL_TOP,
                palette::ACCENT_TEAL_FILL_BOT,
                palette::ACCENT_TEAL,
                palette::TEXT_BRIGHT,
                palette::ACCENT_TEAL,
            ),
            ButtonState::Disabled => (
                palette::BG_BUTTON_DISABLED,
                palette::BG_BUTTON_DISABLED,
                palette::BORDER_SUBTLE,
                palette::TEXT_MUTED,
                palette::TEXT_MUTED,
            ),
        };

        let btn = commands
            .spawn((
                Node {
                    width: Val::Percent(100.0),
                    padding: UiRect::axes(px(pad_x), px(pad_y)),
                    border: UiRect::all(px(bw)),
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::SpaceBetween,
                    column_gap: px((8.0 * scale).round()),
                    position_type: PositionType::Relative,
                    ..Default::default()
                },
                BackgroundGradient::from(LinearGradient::to_bottom(vec![
                    ColorStop::auto(top_c),
                    ColorStop::auto(bot_c),
                ])),
                BorderColor::all(border_c),
                ChildOf(parent),
            ))
            .id();

        let left = commands
            .spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: px((6.0 * scale).round()),
                    ..Default::default()
                },
                ChildOf(btn),
            ))
            .id();

        if state == ButtonState::Selected {
            if let Some(arrow) = &self.focus_arrow {
                let h = (14.0 * scale).round().max(8.0);
                let w = (h * (arrow.size().x / arrow.size().y.max(1.0))).round().max(6.0);
                arrow.spawn(
                    commands,
                    left,
                    Node {
                        width: px(w),
                        height: px(h),
                        flex_shrink: 0.0,
                        ..Default::default()
                    },
                    palette::TEXT_BRIGHT,
                    true,
                    false,
                    true,
                );
            }
            // Crisp amber corner brackets (from `パーツ選択カーソル` dialog 38)
            let tick = (8.0 * scale).round().max(4.0);
            let t = (2.0 * scale).round().max(1.0);
            for (l, r, top, bot) in [
                (Some(-1.0), None, Some(-1.0), None),
                (None, Some(-1.0), Some(-1.0), None),
                (Some(-1.0), None, None, Some(-1.0)),
                (None, Some(-1.0), None, Some(-1.0)),
            ] {
                commands.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: l.map(px).unwrap_or(Val::Auto),
                        right: r.map(px).unwrap_or(Val::Auto),
                        top: top.map(px).unwrap_or(Val::Auto),
                        bottom: bot.map(px).unwrap_or(Val::Auto),
                        width: px(tick),
                        height: px(t),
                        ..Default::default()
                    },
                    BackgroundColor(palette::ACCENT_AMBER),
                    ChildOf(btn),
                ));
            }
        }

        self.spawn_text(commands, left, FontRole::Body, label, scale, text_c);

        if let Some(r) = right_label {
            self.spawn_text(commands, btn, FontRole::Small, r, scale, right_c);
        }

        btn
    }

    /// Spawns a retail `@ParamLine` alternating stat row with optional comparison delta (`> NEW`).
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_stat_row(
        &self,
        commands: &mut Commands,
        parent: Entity,
        label: &str,
        value: &str,
        delta: Option<StatDelta<'_>>,
        even_row: bool,
        scale: f32,
    ) -> Entity {
        let bg = if even_row { palette::BG_ROW } else { palette::BG_BAR };
        let row = commands
            .spawn((
                Node {
                    width: Val::Percent(100.0),
                    padding: UiRect::axes(px((6.0 * scale).round()), px((3.0 * scale).round())),
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::SpaceBetween,
                    column_gap: px((8.0 * scale).round()),
                    ..Default::default()
                },
                BackgroundColor(bg),
                ChildOf(parent),
            ))
            .id();

        self.spawn_text(commands, row, FontRole::Body, label, scale, palette::TEXT_PRIMARY);

        let right = commands
            .spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: px((6.0 * scale).round()),
                    ..Default::default()
                },
                ChildOf(row),
            ))
            .id();

        self.spawn_text(commands, right, FontRole::Body, value, scale, palette::TEXT_PRIMARY);

        if let Some(d) = delta {
            let color = match d.cmp.cmp(&0) {
                std::cmp::Ordering::Greater => palette::TEXT_GOOD,
                std::cmp::Ordering::Less => palette::TEXT_BAD,
                std::cmp::Ordering::Equal => palette::TEXT_PRIMARY,
            };
            self.spawn_text(commands, right, FontRole::Body, ">", scale, palette::TEXT_MUTED);
            self.spawn_text(commands, right, FontRole::Body, d.text, scale, color);
        }

        row
    }

    /// Spawns a retail `@WeightFrame`/`@WeightBar`/`Gauge_parts` capacity gauge.
    ///
    /// `fill` is clamped to `0.0..=1.0`; `recommend_mark` optionally draws the pale cyan `I`
    /// threshold bracket at that normalized horizontal fraction.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_gauge(
        &self,
        commands: &mut Commands,
        parent: Entity,
        label: &str,
        fill: f32,
        recommend_mark: Option<f32>,
        alert: bool,
        scale: f32,
    ) -> Entity {
        let row = commands
            .spawn((
                Node {
                    width: Val::Percent(100.0),
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: px((10.0 * scale).round()),
                    ..Default::default()
                },
                ChildOf(parent),
            ))
            .id();

        let label_box = commands
            .spawn((
                Node {
                    width: px((88.0 * scale).round()),
                    flex_shrink: 0.0,
                    ..Default::default()
                },
                ChildOf(row),
            ))
            .id();
        self.spawn_text(
            commands,
            label_box,
            FontRole::Body,
            label,
            scale,
            if alert { palette::TEXT_ALERT } else { palette::TEXT_PRIMARY },
        );

        let track_h = (10.0 * scale).round().max(6.0);
        let bw = (1.0 * scale).max(1.0);
        let track = commands
            .spawn((
                Node {
                    flex_grow: 1.0,
                    height: px(track_h),
                    border: UiRect::all(px(bw)),
                    padding: UiRect::all(px((1.0 * scale).round().max(1.0))),
                    position_type: PositionType::Relative,
                    align_items: AlignItems::Center,
                    ..Default::default()
                },
                BackgroundColor(palette::BG_BAR),
                BorderColor::all(if alert { palette::TEXT_ALERT } else { palette::BORDER_SUBTLE }),
                ChildOf(row),
            ))
            .id();

        let (g_top, g_bot) = if alert {
            (palette::GAUGE_ALERT_TOP, palette::GAUGE_ALERT_BOT)
        } else {
            (palette::GAUGE_TOP, palette::GAUGE_BOT)
        };
        commands.spawn((
            Node {
                width: Val::Percent(fill.clamp(0.0, 1.0) * 100.0),
                height: Val::Percent(100.0),
                ..Default::default()
            },
            BackgroundGradient::from(LinearGradient::to_bottom(vec![
                ColorStop::auto(g_top),
                ColorStop::auto(g_bot),
            ])),
            ChildOf(track),
        ));

        if let Some(mark) = recommend_mark {
            let pct = mark.clamp(0.0, 1.0) * 100.0;
            let mh = (16.0 * scale).round().max(10.0);
            let mw = (7.0 * scale).round().max(5.0);
            let bar = (1.0 * scale).max(1.0);
            let marker = commands
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Percent(pct),
                        top: px(-((mh - track_h) * 0.5).round()),
                        width: px(mw),
                        height: px(mh),
                        ..Default::default()
                    },
                    ChildOf(track),
                ))
                .id();
            // `Gauge_parts` `I` bracket: top cap, center stem, bottom cap
            commands.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: px(0.0),
                    top: px(0.0),
                    width: px(mw),
                    height: px(bar),
                    ..Default::default()
                },
                BackgroundColor(palette::GAUGE_MARKER),
                ChildOf(marker),
            ));
            commands.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: px((mw * 0.5).floor()),
                    top: px(bar),
                    width: px(bar),
                    height: px((mh - 2.0 * bar).max(1.0)),
                    ..Default::default()
                },
                BackgroundColor(palette::GAUGE_MARKER),
                ChildOf(marker),
            ));
            commands.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: px(0.0),
                    bottom: px(0.0),
                    width: px(mw),
                    height: px(bar),
                    ..Default::default()
                },
                BackgroundColor(palette::GAUGE_MARKER),
                ChildOf(marker),
            ));
        }

        row
    }

    /// Spawns a compact ACVD interactive slider bar row with a focus indicator, label,
    /// `<` / `>` disc arrow steppers, metallic gauge track (with optional `I` default marker),
    /// and right-aligned value readout.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_slider_row(
        &self,
        commands: &mut Commands,
        parent: Entity,
        label: &str,
        value_text: &str,
        fill: f32,
        recommend_mark: Option<f32>,
        selected: bool,
        label_width: f32,
        scale: f32,
    ) -> SliderRowEntities {
        let bw = (1.0 * scale).max(1.0);
        let (top_c, bot_c, border_c, fg_c) = if selected {
            (
                palette::ACCENT_AMBER_FILL_TOP,
                palette::ACCENT_AMBER_FILL_BOT,
                palette::ACCENT_AMBER,
                palette::TEXT_BRIGHT,
            )
        } else {
            (
                palette::BG_BUTTON_TOP,
                palette::BG_BUTTON_BOT,
                palette::BORDER_BUTTON,
                palette::TEXT_PRIMARY,
            )
        };

        let row = commands
            .spawn((
                Node {
                    width: Val::Percent(100.0),
                    min_height: px((32.0 * scale).round()),
                    padding: UiRect::axes(px((10.0 * scale).round()), px((5.0 * scale).round())),
                    border: UiRect::all(px(bw)),
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::SpaceBetween,
                    column_gap: px((8.0 * scale).round()),
                    position_type: PositionType::Relative,
                    ..Default::default()
                },
                BackgroundGradient::from(LinearGradient::to_bottom(vec![
                    ColorStop::auto(top_c),
                    ColorStop::auto(bot_c),
                ])),
                BorderColor::all(border_c),
                ChildOf(parent),
            ))
            .id();

        // Left: focus arrow + label
        let left = commands
            .spawn((
                Node {
                    width: px((label_width * scale).round()),
                    flex_shrink: 0.0,
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: px((5.0 * scale).round()),
                    ..Default::default()
                },
                ChildOf(row),
            ))
            .id();

        if selected {
            if let Some(arrow) = &self.focus_arrow {
                let s = arrow.size() * (0.58 * scale);
                arrow.spawn(
                    commands,
                    left,
                    Node {
                        width: px(s.x.round()),
                        height: px(s.y.round()),
                        flex_shrink: 0.0,
                        ..Default::default()
                    },
                    palette::ACCENT_ARROW,
                    false,
                    false,
                    true,
                );
            }
        }
        self.spawn_text(commands, left, FontRole::Body, label, scale * 0.92, fg_c);

        // Middle: `<` button + gauge track + `>` button
        let controls = commands
            .spawn((
                Node {
                    flex_grow: 1.0,
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: px((6.0 * scale).round()),
                    ..Default::default()
                },
                ChildOf(row),
            ))
            .id();

        let step_pad_x = (5.0 * scale).round().max(3.0);
        let step_pad_y = (3.0 * scale).round().max(2.0);
        let dec_btn = commands
            .spawn((
                Node {
                    padding: UiRect::axes(px(step_pad_x), px(step_pad_y)),
                    border: UiRect::all(px(bw)),
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::Center,
                    flex_shrink: 0.0,
                    ..Default::default()
                },
                BackgroundColor(palette::BG_BAR),
                BorderColor::all(if selected {
                    palette::ACCENT_AMBER
                } else {
                    palette::BORDER_SUBTLE
                }),
                ChildOf(controls),
            ))
            .id();
        self.spawn_arrow(commands, dec_btn, ArrowDir::Left, scale * 0.82, fg_c);

        let track_h = (10.0 * scale).round().max(6.0);
        let track = commands
            .spawn((
                Node {
                    flex_grow: 1.0,
                    height: px(track_h),
                    border: UiRect::all(px(bw)),
                    padding: UiRect::all(px((1.0 * scale).round().max(1.0))),
                    position_type: PositionType::Relative,
                    align_items: AlignItems::Center,
                    ..Default::default()
                },
                BackgroundColor(palette::BG_BAR),
                BorderColor::all(if selected {
                    palette::ACCENT_AMBER
                } else {
                    palette::BORDER_SUBTLE
                }),
                ChildOf(controls),
            ))
            .id();

        let (g_top, g_bot) = if selected {
            (palette::ACCENT_AMBER, palette::ACCENT_AMBER_FILL_BOT)
        } else {
            (palette::GAUGE_TOP, palette::GAUGE_BOT)
        };
        commands.spawn((
            Node {
                width: Val::Percent(fill.clamp(0.0, 1.0) * 100.0),
                height: Val::Percent(100.0),
                ..Default::default()
            },
            BackgroundGradient::from(LinearGradient::to_bottom(vec![
                ColorStop::auto(g_top),
                ColorStop::auto(g_bot),
            ])),
            ChildOf(track),
        ));

        if let Some(mark) = recommend_mark {
            let pct = mark.clamp(0.0, 1.0) * 100.0;
            let bar_w = (2.0 * scale).round().max(1.0);
            let cap_w = (6.0 * scale).round().max(4.0);
            let cap_h = (2.0 * scale).round().max(1.0);
            let marker = commands
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Percent(pct),
                        top: px(-2.0 * scale),
                        bottom: px(-2.0 * scale),
                        width: px(cap_w),
                        margin: UiRect::left(px(-cap_w * 0.5)),
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::SpaceBetween,
                        ..Default::default()
                    },
                    ChildOf(track),
                ))
                .id();
            commands.spawn((
                Node {
                    width: px(cap_w),
                    height: px(cap_h),
                    ..Default::default()
                },
                BackgroundColor(palette::GAUGE_MARKER),
                ChildOf(marker),
            ));
            commands.spawn((
                Node {
                    width: px(bar_w),
                    flex_grow: 1.0,
                    ..Default::default()
                },
                BackgroundColor(palette::GAUGE_MARKER),
                ChildOf(marker),
            ));
            commands.spawn((
                Node {
                    width: px(cap_w),
                    height: px(cap_h),
                    ..Default::default()
                },
                BackgroundColor(palette::GAUGE_MARKER),
                ChildOf(marker),
            ));
        }

        let inc_btn = commands
            .spawn((
                Node {
                    padding: UiRect::axes(px(step_pad_x), px(step_pad_y)),
                    border: UiRect::all(px(bw)),
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::Center,
                    flex_shrink: 0.0,
                    ..Default::default()
                },
                BackgroundColor(palette::BG_BAR),
                BorderColor::all(if selected {
                    palette::ACCENT_AMBER
                } else {
                    palette::BORDER_SUBTLE
                }),
                ChildOf(controls),
            ))
            .id();
        self.spawn_arrow(commands, inc_btn, ArrowDir::Right, scale * 0.82, fg_c);

        // Right: fixed-width percentage readout
        let val_box = commands
            .spawn((
                Node {
                    width: px((48.0 * scale).round()),
                    flex_shrink: 0.0,
                    justify_content: JustifyContent::FlexEnd,
                    align_items: AlignItems::Center,
                    ..Default::default()
                },
                ChildOf(row),
            ))
            .id();
        self.spawn_text(
            commands,
            val_box,
            FontRole::Small,
            value_text,
            scale,
            if selected {
                palette::TEXT_BRIGHT
            } else {
                palette::TEXT_MUTED
            },
        );

        SliderRowEntities {
            row,
            track,
            dec_btn,
            inc_btn,
        }
    }

    /// Spawns a compact tactical tag/badge (`■ PARTS VIEW`, `ALL TYPES`, `KE`, `CE`, `TE`).
    pub fn spawn_badge(
        &self,
        commands: &mut Commands,
        parent: Entity,
        label: &str,
        accent: Color,
        active: bool,
        scale: f32,
    ) -> Entity {
        let bw = (1.0 * scale).max(1.0);
        let sq = (6.0 * scale).round().max(4.0);
        let badge = commands
            .spawn((
                Node {
                    padding: UiRect::axes(px((7.0 * scale).round()), px((3.0 * scale).round())),
                    border: UiRect::all(px(bw)),
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: px((5.0 * scale).round()),
                    ..Default::default()
                },
                BackgroundColor(if active { palette::ACCENT_TEAL_FILL_TOP } else { palette::BG_BAR }),
                BorderColor::all(if active { accent } else { palette::BORDER_SUBTLE }),
                ChildOf(parent),
            ))
            .id();

        commands.spawn((
            Node {
                width: px(sq),
                height: px(sq),
                flex_shrink: 0.0,
                ..Default::default()
            },
            BackgroundColor(accent),
            ChildOf(badge),
        ));

        self.spawn_text(
            commands,
            badge,
            FontRole::Small,
            label,
            scale,
            if active { palette::TEXT_BRIGHT } else { palette::TEXT_MUTED },
        );

        badge
    }

    /// Spawns a real disc navigation arrow (`Parts_01` `@Arrow_Left` / `ScrollTop`).
    pub fn spawn_arrow(
        &self,
        commands: &mut Commands,
        parent: Entity,
        dir: ArrowDir,
        scale: f32,
        color: Color,
    ) -> Option<Entity> {
        let (slice, fx, fy) = match dir {
            ArrowDir::Left => (self.arrow_horiz.as_ref()?, false, false),
            ArrowDir::Right => (self.arrow_horiz.as_ref()?, true, false),
            ArrowDir::Up => (self.arrow_vert.as_ref()?, false, false),
            ArrowDir::Down => (self.arrow_vert.as_ref()?, false, true),
        };
        let s = slice.size() * scale;
        Some(slice.spawn(
            commands,
            parent,
            Node {
                width: px(s.x.round()),
                height: px(s.y.round()),
                flex_shrink: 0.0,
                ..Default::default()
            },
            color,
            fx,
            fy,
            true,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acvd_formats::vfs;

    #[test]
    fn ui_kit_loads_retail_fonts_and_chrome_sprites_from_disc() {
        let iso = vfs::repo_root().join(vfs::X360_ISO);
        if !iso.is_file() {
            return;
        }
        let disc = Disc::open(&iso).unwrap();
        let mut images = Assets::<Image>::default();
        let kit = AcvdUiKit::load(&disc, &mut images).unwrap();

        assert!(kit.font_header.is_some(), "header CCM font (ID 18) missing");
        assert!(kit.font_body.is_some(), "body CCM font (ID 17) missing");
        assert!(kit.font_small.is_some(), "small CCM font (ID 21) missing");
        assert!(kit.reticle_corner.is_some(), "Parts_01 corner reticle missing");
        assert!(kit.reticle_side.is_some(), "Parts_01 side reticle missing");
        assert!(kit.corner_bracket.is_some(), "ACV_FE_Locksight_02 corner bracket missing");
        assert!(kit.trace_step.is_some(), "ACV_frame stepped trace missing");
        assert!(kit.focus_arrow.is_some(), "ACV_MENU_parts1 focus arrow missing");
        assert!(kit.arrow_horiz.is_some(), "Parts_01 horiz arrow missing");
        assert!(kit.arrow_vert.is_some(), "Parts_01 vert arrow missing");

        // Verify flexbox text measurement produces non-zero bounds for all three font roles.
        for role in [FontRole::Header, FontRole::Body, FontRole::Small] {
            let sz = kit.measure_text(role, "AC DESIGN PRESETS 6312", 1.0);
            assert!(sz.x > 40.0 && sz.y >= 10.0, "{role:?} measured {sz:?}");
        }

        // Small keeps e16_ext (16px) for all-caps micro-labels, but falls back to Body
        // (j1_16_ext, 18px) for lowercase/mixed-case strings so uneven e16_ext lowercase
        // bearings don't produce broken kerning.
        let small_caps = kit.resolve_font(FontRole::Small, "PARTS VIEW").unwrap();
        let small_lower = kit
            .resolve_font(FontRole::Small, "Click anywhere on the AC to stamp your image.")
            .unwrap();
        assert!(Arc::ptr_eq(small_caps, kit.font_small.as_ref().unwrap()));
        assert!(Arc::ptr_eq(small_lower, kit.font_body.as_ref().unwrap()));
    }
}
