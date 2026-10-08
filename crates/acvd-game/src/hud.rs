//! In-game HUD: the lock-sight dialogs of `lang/en/menu/sortie.drb.dcx`, drawn over gameplay
//! with `acvd_render::menu` and scaled from the 1280x720 layout to the window (letterboxed,
//! re-laid out on resize). The 360 HUD is set up by `0x829d0290` and updated each frame by
//! `0x829d0b30` (vtable `0x820c3010` slot 0x5c).
//!
//! Digits: each digit sprite is authored as '0' (texel rect (317,135)-(330,151) of
//! `ACV_FE_Locksight_02`, table `0x82598218`); the strip holds 0-9 at that width (13 texels) per
//! step. `AP*` and the weapon digits are named by place value and show every place, leading
//! zeros included (`0x825979e8`). The energy digits `EN1`-`EN5` read the percentage with two
//! decimals, `EN1 EN2 EN3 . EN4 EN5` (`0x82596b48` on `int(EN / EN max * 10000)`), so a full
//! generator reads 100.00.
//!
//! Gauges (`AlphaAnimSprite`): `Gauge_AP` and `Gauge_APEffect` fill with AP / max AP, `Gauge_EN`
//! with the energy fraction, `Gauge_LWeapon`/`Gauge_RWeapon` with rounds left / magazine, all
//! times a show scale that rises over `gauge_show_time` after the lock sight appears. Below its
//! red zone (`ap_red_zone`, `en_red_zone`, `ammo_red_zone`) a gauge and its digits fade to the
//! low colour over `gauge_color_time` (`0x82599090`). The arcs of `ACV_LockSight_base` and the
//! gauge tracks are drawn in the lock-sight digit color. Not drawn: `Top_outline` (a faint frame
//! around the whole screen), the filled quarters under the ring, and `Locksight_bar` (a solid
//! band across the crosshair). Player coloring and gauge shading are not applied yet.
//!
//! Side panels: the `Weapon` dialog at the `LeftArm`/`RightArm` anchors of `ACV_FE_Normal`
//! (`0x82598b30` via `0x82597f38`) shows rounds left in `CurAmmo` (`%d` up to 9999), or `Empty`
//! at zero (`0x825970f0`).
//!
//! Not drawn: shoulder and hanger weapons (the rewrite has none), the damage blink of
//! `Gauge_APEffect`, the conditional warnings of `ACV_FE_Normal`. AP and energy stay full until
//! damage and energy drain exist.

use acvd_data::generated::ac_unit::AcAssemblyDesignSt;
use acvd_formats::vfs::Disc;
use acvd_render::menu::{self, Layout, MenuGauge, MenuObject, MenuRoot, MenuSprite, MenuText};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::weapons::{Armament, Gun, Kind};

const LAYOUT: &str = "lang/en/menu/sortie.drb.dcx";
const SCREEN: Vec2 = Vec2::new(1280.0, 720.0);
/// Dialogs drawn back to front, and whether they are laid out around the screen center.
const DIALOGS: [(&str, bool); 2] = [
    ("ACV_LockSight_base", false),
    ("ACV_FE_LockSightCenter", true),
];
/// `ACV_FE_Normal` anchors of the side `Weapon` panels and the hand they show
/// (`Armament::hands` is [right, left]).
const PANELS: [(&str, usize); 2] = [("LeftArm", 1), ("RightArm", 0)];
const STATIC_DRAW: &str = "StaticDrawParam";

#[derive(Resource)]
pub struct Sortie {
    layout: Layout,
    /// Window size the HUD was last laid out for.
    size: Vec2,
    /// Gauge show scale (`0x829d0b30` this+0x3f0), 0 when the lock sight appears.
    show: f32,
    /// Per-readout blend toward the low colour (gauge controller +0x14).
    low: [f32; 5],
}

pub fn load(disc: &Disc, images: &mut Assets<Image>) -> anyhow::Result<Sortie> {
    Ok(Sortie {
        layout: menu::load(disc, LAYOUT, images)?,
        size: Vec2::ZERO,
        show: 0.0,
        low: [0.0; 5],
    })
}

/// The piloted AC's AP and energy, as the HUD status block (`0x829d0b30` r5: AP +8, max +0xc,
/// EN +0x10, max +0x14) holds them.
#[derive(Component, Clone, Copy, Debug)]
pub struct Status {
    pub ap: u32,
    pub ap_max: u32,
    /// Energy over capacity; the HUD only shows the fraction.
    pub energy: f32,
}

impl Status {
    /// Full AP: the sum of the frame parts' `ap` (`sheets/ac_part_fields.csv`, `0x8286a600`).
    pub fn from_design(design: &AcAssemblyDesignSt) -> Self {
        let parts = [
            (0, design.head),
            (1, design.core),
            (2, design.arms),
            (3, design.legs),
        ];
        let ap_max = parts
            .iter()
            .map(|&(category, id)| acvd_data::part_field(id as i64, category, "ap") as u32)
            .sum();
        Self {
            ap: ap_max,
            ap_max,
            energy: 1.0,
        }
    }
}

/// A side `Weapon` panel and the hand it shows.
#[derive(Component)]
pub struct Panel(usize);

fn tuning(index: usize) -> f32 {
    acvd_data::tuning(STATIC_DRAW, index).unwrap_or_default() as f32
}

/// `gauge_low_r..a` (StaticDrawParam +0x444).
fn low_color() -> Color {
    let [r, g, b, a] = [305, 306, 307, 308].map(|i| tuning(i) as u8);
    Color::srgba_u8(r, g, b, a)
}

pub fn layout(
    mut commands: Commands,
    hud: Option<ResMut<Sortie>>,
    window: Query<&Window, With<PrimaryWindow>>,
    roots: Query<Entity, With<MenuRoot>>,
) {
    let (Some(mut hud), Ok(window)) = (hud, window.single()) else {
        return;
    };
    let size = window.size();
    if size == hud.size || size.min_element() <= 0.0 {
        return;
    }
    hud.size = size;
    hud.show = 0.0;
    for e in &roots {
        commands.entity(e).despawn();
    }
    let scale = (size / SCREEN).min_element();
    let corner = (size - SCREEN * scale) / 2.0;
    let dialog = |name: &str| hud.layout.drb.dialogs.iter().position(|d| d.name == name);
    for (name, centered) in DIALOGS {
        let Some(index) = dialog(name) else {
            warn!("{LAYOUT}: no dialog {name}");
            continue;
        };
        let origin = if centered {
            corner + SCREEN * scale / 2.0
        } else {
            corner
        };
        menu::spawn(&mut commands, &hud.layout, index, origin, scale);
    }
    let (Some(normal), Some(weapon)) = (dialog("ACV_FE_Normal"), dialog("Weapon")) else {
        warn!("{LAYOUT}: no ACV_FE_Normal or Weapon dialog");
        return;
    };
    for (anchor, hand) in PANELS {
        let Some(o) = hud.layout.drb.dialogs[normal]
            .objects
            .iter()
            .find(|o| o.name == anchor)
        else {
            continue;
        };
        let rect = o.shape.rect();
        let origin =
            corner + Vec2::new(rect[0].min(rect[2]) as f32, rect[1].min(rect[3]) as f32) * scale;
        let root = menu::spawn(&mut commands, &hud.layout, weapon, origin, scale);
        commands.entity(root).insert(Panel(hand));
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Readout {
    LeftArm,
    RightArm,
    Shoulder,
    Ap,
    Energy,
}

/// A digit sprite's authored ('0') texel rect, or a gauge's or digit's authored colour.
#[derive(Component)]
pub struct Authored {
    rect: Rect,
    color: Color,
}

/// Digit `name` and its divisor.
fn readout(name: &str) -> Option<(Readout, u32)> {
    if name == "ENDot0" {
        return Some((Readout::Energy, 0));
    }
    if let Some(n) = name.strip_prefix("EN").and_then(|n| n.parse::<u32>().ok()) {
        return (1..=5)
            .contains(&n)
            .then(|| (Readout::Energy, 10u32.pow(5 - n)));
    }
    [
        ("LWep", Readout::LeftArm),
        ("RWep", Readout::RightArm),
        ("SWep", Readout::Shoulder),
        ("AP", Readout::Ap),
    ]
    .into_iter()
    .find_map(|(prefix, r)| {
        name.strip_prefix(prefix)?
            .parse::<u32>()
            .ok()
            .filter(|&p| p > 0)
            .map(|place| (r, place))
    })
}

fn gauge(name: &str) -> Option<Readout> {
    Some(match name {
        "Gauge_AP" | "Gauge_APEffect" => Readout::Ap,
        "Gauge_EN" => Readout::Energy,
        "Gauge_LWeapon" => Readout::LeftArm,
        "Gauge_RWeapon" => Readout::RightArm,
        "Gauge_SWeapon" => Readout::Shoulder,
        _ => return None,
    })
}

/// The lock-sight digit color (`AP1` in `ACV_FE_LockSightCenter`, 0x00ffa5ff). The ring art is
/// authored white; the coloring option repaints it (the reference sight is the selected swatch).
/// That option is not wired, so the ring uses this color instead of sitting on screen as a white veil.
const SIGHT: [u8; 3] = [0x00, 0xff, 0xa5];

/// White ring art: the filled quarters and arcs of `ACV_LockSight_base`, and the gauge tracks
/// under the `AlphaAnimSprite`s. `SHWeaponBar_base` is handled with the shoulder readout.
fn sight_art(name: &str) -> bool {
    name.starts_with("LockSight_Parts")
        || name.starts_with("Locksight_bar")
        || name.starts_with("AP_ENBar_base")
        || name.starts_with("WeaponBar_base")
}

/// Digit plates and the extra shoulder-weapon track; hidden with their readout.
fn chrome(name: &str) -> Option<Readout> {
    Some(match name {
        "Back_LWeapon" => Readout::LeftArm,
        "Back_RWeapon" => Readout::RightArm,
        "Back_SWeapon" | "SHWeaponBar_base" => Readout::Shoulder,
        _ => return None,
    })
}

fn gun(arms: Option<&Armament>, which: Readout) -> Option<Gun> {
    let hand = match which {
        Readout::LeftArm => 1,
        Readout::RightArm => 0,
        _ => return None,
    };
    arms.map(|a| a.hands[hand]).filter(|g| g.kind != Kind::Skip)
}

/// Shown value (digits), fill and whether it is below its red zone; None hides the readout.
fn reading(
    which: Readout,
    status: Option<&Status>,
    arms: Option<&Armament>,
) -> Option<(u32, f32, bool)> {
    let fraction = |cur: f32, max: f32| if max > 0.0 { cur / max } else { 0.0 };
    match which {
        Readout::Ap => status.map(|s| {
            let f = fraction(s.ap as f32, s.ap_max as f32);
            (s.ap.min(999_999), f, s.ap_max > 0 && f < tuning(322))
        }),
        Readout::Energy => status.map(|s| {
            (
                (s.energy * 10000.0) as u32,
                s.energy,
                s.energy < tuning(323),
            )
        }),
        Readout::LeftArm | Readout::RightArm => gun(arms, which).map(|g| {
            let f = fraction(g.remaining as f32, g.magazine as f32);
            (g.remaining as u32, f, g.magazine > 0 && f < tuning(324))
        }),
        Readout::Shoulder => None,
    }
}

fn slot(which: Readout) -> usize {
    which as usize
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    let (a, b) = (a.to_srgba(), b.to_srgba());
    Color::srgba(
        a.red + (b.red - a.red) * t,
        a.green + (b.green - a.green) * t,
        a.blue + (b.blue - a.blue) * t,
        a.alpha + (b.alpha - a.alpha) * t,
    )
}

/// Eases the show scale and the low-colour blends.
pub fn tick(
    time: Res<Time>,
    hud: Option<ResMut<Sortie>>,
    status: Query<&Status>,
    arms: Query<&Armament>,
) {
    let Some(mut hud) = hud else { return };
    let dt = time.delta_secs();
    let show_time = tuning(299);
    hud.show = if show_time > 0.0 {
        (hud.show + dt / show_time).min(1.0)
    } else {
        1.0
    };
    let color_time = tuning(298);
    let (status, arms) = (status.single().ok(), arms.single().ok());
    for which in [
        Readout::LeftArm,
        Readout::RightArm,
        Readout::Shoulder,
        Readout::Ap,
        Readout::Energy,
    ] {
        let low = reading(which, status, arms).is_some_and(|r| r.2);
        let t = &mut hud.low[slot(which)];
        let step = if color_time > 0.0 {
            dt / color_time
        } else {
            1.0
        };
        *t = (*t + if low { step } else { -step }).clamp(0.0, 1.0);
    }
}

pub fn readouts(
    mut commands: Commands,
    hud: Option<Res<Sortie>>,
    status: Query<&Status>,
    arms: Query<&Armament>,
    mut digits: Query<
        (
            Entity,
            &MenuObject,
            &mut MenuSprite,
            &mut Visibility,
            Option<&Authored>,
        ),
        Without<MenuGauge>,
    >,
    mut gauges: Query<
        (
            Entity,
            &MenuObject,
            &mut MenuGauge,
            &mut Visibility,
            Option<&Authored>,
        ),
        Without<MenuSprite>,
    >,
    mut plates: Query<(&MenuObject, &mut Visibility), (Without<MenuSprite>, Without<MenuGauge>)>,
) {
    let Some(hud) = hud else { return };
    let (status, arms) = (status.single().ok(), arms.single().ok());
    let low = low_color();
    for (object, mut visibility) in &mut plates {
        let Some(which) = chrome(&object.0) else {
            continue;
        };
        visibility.set_if_neq(if reading(which, status, arms).is_some() {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
    }
    for (e, object, mut sprite, mut visibility, authored) in &mut digits {
        if let Some(which) = chrome(&object.0) {
            visibility.set_if_neq(if reading(which, status, arms).is_some() {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            });
            continue;
        }
        if sight_art(&object.0) && authored.is_none() {
            let a = (sprite.color.to_srgba().alpha * 255.0).round() as u8;
            // Filled quarters (alpha 10) edge as a disc. `Locksight_bar` is a solid slab across
            // the crosshair; tinted, it is the green block behind the digits.
            let filled = (object.0.starts_with("LockSight_Parts") && a <= 12)
                || object.0.starts_with("Locksight_bar");
            if filled {
                visibility.set_if_neq(Visibility::Hidden);
                commands.entity(e).insert(Authored {
                    rect: sprite.rect,
                    color: sprite.color,
                });
                continue;
            }
            sprite.color = Color::srgba_u8(SIGHT[0], SIGHT[1], SIGHT[2], a);
            commands.entity(e).insert(Authored {
                rect: sprite.rect,
                color: sprite.color,
            });
            continue;
        }
        let Some((which, place)) = readout(&object.0) else {
            continue;
        };
        let (rect, color) = authored.map_or((sprite.rect, sprite.color), |a| (a.rect, a.color));
        if authored.is_none() {
            commands.entity(e).insert(Authored { rect, color });
        }
        let value = reading(which, status, arms);
        visibility.set_if_neq(if value.is_some() {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
        let Some((value, ..)) = value else { continue };
        let color = mix(color, low, hud.low[slot(which)]);
        if sprite.color != color {
            sprite.color = color;
        }
        if place == 0 {
            continue;
        }
        let digit = (value / place % 10) as f32;
        let uv = Rect {
            min: rect.min + Vec2::X * rect.width() * digit,
            max: rect.max + Vec2::X * rect.width() * digit,
        };
        if sprite.rect != uv {
            sprite.rect = uv;
        }
    }
    for (e, object, mut g, mut visibility, authored) in &mut gauges {
        let Some(which) = gauge(&object.0) else {
            continue;
        };
        let color = authored.map_or(g.color, |a| a.color);
        if authored.is_none() {
            commands.entity(e).insert(Authored {
                rect: g.rect,
                color,
            });
        }
        let value = reading(which, status, arms);
        visibility.set_if_neq(if value.is_some() {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
        let Some((_, fraction, _)) = value else {
            continue;
        };
        let fill = fraction.clamp(0.0, 1.0) * hud.show;
        let color = if object.0 == "Gauge_APEffect" {
            color
        } else {
            mix(color, low, hud.low[slot(which)])
        };
        if g.fill != fill || g.color != color {
            g.fill = fill;
            g.color = color;
        }
    }
}

/// Fills the side panels' `CurAmmo` and shows `Empty` at zero (`0x825970f0`).
pub fn panels(
    arms: Query<&Armament>,
    panels: Query<(Entity, &Panel)>,
    children: Query<&Children>,
    mut objects: Query<(&MenuObject, &mut Visibility, Option<&mut MenuText>)>,
) {
    let arms = arms.single().ok();
    for (root, panel) in &panels {
        let gun = arms
            .map(|a| a.hands[panel.0])
            .filter(|g| g.kind != Kind::Skip);
        for e in children.iter_descendants(root) {
            let Ok((object, mut visibility, text)) = objects.get_mut(e) else {
                continue;
            };
            let shown = match (object.0.as_str(), gun) {
                (_, None) => false,
                ("CurAmmo", Some(g)) => {
                    let s = if g.remaining == 0 {
                        String::new()
                    } else {
                        g.remaining.min(9999).to_string()
                    };
                    if let Some(mut t) = text {
                        if t.text != s {
                            t.text = s;
                        }
                    }
                    true
                }
                ("Empty", Some(g)) => g.remaining == 0,
                ("Purge", _) => false,
                _ => true,
            };
            visibility.set_if_neq(if shown {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digit_names() {
        assert!(readout("LWep1000") == Some((Readout::LeftArm, 1000)));
        assert!(readout("RWep1") == Some((Readout::RightArm, 1)));
        assert!(readout("AP10000") == Some((Readout::Ap, 10000)));
        assert!(readout("ENDot0") == Some((Readout::Energy, 0)));
        assert!(readout("BG_ap").is_none() && readout("APfont").is_none());
    }

    /// Full energy reads 100.00 on EN1 EN2 EN3 . EN4 EN5.
    #[test]
    fn energy_percent() {
        let v = (1.0f32 * 10000.0) as u32;
        let shown: Vec<u32> = (1..=5)
            .map(|n| readout(&format!("EN{n}")).unwrap().1)
            .map(|d| v / d % 10)
            .collect();
        assert_eq!(shown, [1, 0, 0, 0, 0]);
        let v = (0.4567f32 * 10000.0) as u32;
        let shown: Vec<u32> = (1..=5)
            .map(|n| readout(&format!("EN{n}")).unwrap().1)
            .map(|d| v / d % 10)
            .collect();
        assert_eq!(shown, [0, 4, 5, 6, 7]);
    }

    #[test]
    fn tuning_rows() {
        assert!((tuning(298) - 0.3).abs() < 1e-6 && (tuning(322) - 0.3).abs() < 1e-6);
        assert_eq!(low_color().to_srgba().to_u8_array(), [255, 0, 0, 255]);
    }

    #[test]
    fn design_5001_ap() {
        let d = acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 5001)
            .unwrap();
        assert_eq!(Status::from_design(&d.data).ap, 34695);
    }
}
