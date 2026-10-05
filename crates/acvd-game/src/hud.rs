//! In-game HUD: the lock-sight dialogs of `lang/en/menu/sortie.drb.dcx`, drawn over gameplay
//! with `acvd_render::menu` and scaled from the 1280x720 layout to the window (letterboxed,
//! re-laid out on resize).
//!
//! Live: the hand-weapon ammo digits (`LWep*`, `RWep*` in `ACV_FE_LockSightCenter`) from
//! [`Armament`]. Each digit sprite is authored as '0' (texel rect (317,135)-(330,151) of
//! `ACV_FE_Locksight_02`); the strip holds 0-9 at that width (13 texels) per step. Hidden until
//! the game has the state: AP (`AP*`), energy (`EN*`, `ENDot0`) and shoulder ammo (`SWep*`).
//! Not drawn: the `AlphaAnimSprite` gauges, the side `Weapon` panels (part name, `CurAmmo`
//! runtime text) and the conditional warnings of `ACV_FE_Normal`.

use std::path::Path;

use acvd_render::menu::{self, Layout, MenuObject, MenuRoot, MenuSprite};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::weapons::{Armament, Kind};

const LAYOUT: &str = "lang/en/menu/sortie.drb.dcx";
const SCREEN: Vec2 = Vec2::new(1280.0, 720.0);
/// Dialogs drawn back to front, and whether they are laid out around the screen center.
const DIALOGS: [(&str, bool); 3] = [("Top_outline", false), ("ACV_LockSight_base", false), ("ACV_FE_LockSightCenter", true)];

#[derive(Resource)]
pub struct Sortie {
    layout: Layout,
    /// Window size the HUD was last laid out for.
    size: Vec2,
}

pub fn load(usrdir: &Path, images: &mut Assets<Image>) -> anyhow::Result<Sortie> {
    Ok(Sortie { layout: menu::load(usrdir, LAYOUT, images)?, size: Vec2::ZERO })
}

pub fn layout(mut commands: Commands, hud: Option<ResMut<Sortie>>, window: Query<&Window, With<PrimaryWindow>>, roots: Query<Entity, With<MenuRoot>>) {
    let (Some(mut hud), Ok(window)) = (hud, window.single()) else { return };
    let size = window.size();
    if size == hud.size || size.min_element() <= 0.0 {
        return;
    }
    hud.size = size;
    for e in &roots {
        commands.entity(e).despawn();
    }
    let scale = (size / SCREEN).min_element();
    let corner = (size - SCREEN * scale) / 2.0;
    for (name, centered) in DIALOGS {
        let Some(index) = hud.layout.drb.dialogs.iter().position(|d| d.name == name) else {
            warn!("{LAYOUT}: no dialog {name}");
            continue;
        };
        let origin = if centered { corner + SCREEN * scale / 2.0 } else { corner };
        menu::spawn(&mut commands, &hud.layout, index, origin, scale);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Readout {
    LeftArm,
    RightArm,
    Shoulder,
    Ap,
    Energy,
}

/// A digit sprite's authored ('0') texel rect.
#[derive(Component)]
pub struct Zero(Rect);

fn readout(name: &str) -> Option<(Readout, u32)> {
    if name == "ENDot0" {
        return Some((Readout::Energy, 0));
    }
    [("LWep", Readout::LeftArm), ("RWep", Readout::RightArm), ("SWep", Readout::Shoulder), ("AP", Readout::Ap), ("EN", Readout::Energy)]
        .into_iter()
        .find_map(|(prefix, r)| name.strip_prefix(prefix)?.parse::<u32>().ok().map(|place| (r, place)))
}

pub fn readouts(mut commands: Commands, arms: Query<&Armament>, mut digits: Query<(Entity, &MenuObject, &mut MenuSprite, &mut Visibility, Option<&Zero>)>) {
    let arms = arms.single().ok();
    let ammo = |hand: usize| arms.map(|a| a.hands[hand]).filter(|g| g.kind != Kind::Skip).map(|g| g.remaining as u32);
    for (e, object, mut sprite, mut visibility, zero) in &mut digits {
        let Some((which, place)) = readout(&object.0) else { continue };
        let rect = zero.map_or(sprite.rect, |z| z.0);
        if zero.is_none() {
            commands.entity(e).insert(Zero(rect));
        }
        // Armament.hands is [right, left].
        let value = match which {
            Readout::LeftArm => ammo(1),
            Readout::RightArm => ammo(0),
            Readout::Shoulder | Readout::Ap | Readout::Energy => None,
        };
        let shown = if value.is_some() { Visibility::Inherited } else { Visibility::Hidden };
        visibility.set_if_neq(shown);
        let Some(value) = value else { continue };
        let digit = (value / place.max(1) % 10) as f32;
        let uv = Rect { min: rect.min + Vec2::X * rect.width() * digit, max: rect.max + Vec2::X * rect.width() * digit };
        if sprite.rect != uv {
            sprite.rect = uv;
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
        assert!(readout("EN4") == Some((Readout::Energy, 4)));
        assert!(readout("ENDot0") == Some((Readout::Energy, 0)));
        assert!(readout("BG_ap").is_none() && readout("APfont").is_none());
    }
}
