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
//! `Gauge_APEffect`, the conditional warnings of `ACV_FE_Normal`. AP stays full until damage
//! exists; EN is driven live from `Pilot::en` / `Pilot::en_max`.

use acvd_data::generated::ac_unit::AcAssemblyDesignSt;
use acvd_formats::vfs::Disc;
use acvd_render::app::ShotMark;
use acvd_render::menu::{self, Layout, MenuGauge, MenuObject, MenuRoot, MenuSprite, MenuText};
use bevy::prelude::*;
use bevy::ui::{px, UiTransform};
use bevy::window::PrimaryWindow;

use crate::control::Pilot;
use crate::damage::PlayerHealth;
use crate::fcs::Fcs;
use crate::scan::{ReconUnit, ScanState, MAX_RECON_SLOTS};
use crate::weapons::{Armament, Gun, Kind, OwPhase};

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

/// 360 AP lag tracker (`0x8258c808` / `0x8258c868` / `0x8258c7d8`) & `Gauge_APEffect` blink state
/// (`0x829d0b30`: `StaticDrawParam +0x430` `ap_damage_blink_hz`, `+0x434` `ap_double_gauge_threshold`,
/// `+0x484..+0x487` `ap_damage_r..a`, `NewAcBehavior +0x57c` `ap_lag_decay_frames`).
#[derive(Clone, Copy, Debug, Default)]
pub struct ApEffectState {
    pub cur_ap: f32,
    pub lag_ap: f32,
    pub peak_ap: f32,
    pub blink_timer: f32,
    pub effect_alpha: f32,
    /// `0..=1` blend toward `ap_damage_r..a` while `lag_ap != cur_ap` (`0x829d0d28` -> `0x829d00c8`).
    pub damage_blend: f32,
    initialized: bool,
}

impl ApEffectState {
    /// Steps the AP lag bar and blink alpha for `dt` seconds (`0x8258c868` + `0x829d0b30`).
    pub fn step(&mut self, cur_ap: f32, ap_max: f32, dt: f32) {
        if !self.initialized || cur_ap > self.lag_ap {
            // Initial spawn, design change, or respawn: snap lag bar up immediately (`0x8258c8e0`).
            self.cur_ap = cur_ap;
            self.lag_ap = cur_ap;
            self.peak_ap = cur_ap;
            self.blink_timer = 0.0;
            self.effect_alpha = 0.0;
            self.damage_blend = 0.0;
            self.initialized = true;
            return;
        }
        self.cur_ap = cur_ap;
        // `0x825b36e8`: `NewAcBehavior +0x57c` (`ap_lag_decay_frames` = 120) * 1/60 = 2.0 s.
        let decay_secs = (acvd_data::tuning("NewAcBehavior", 379).unwrap_or(120.0) as f32 / 60.0)
            .max(0.05);
        if self.lag_ap > cur_ap {
            // `0x8258c7d8`: `rate = (peak_ap - cur_ap) / decay_secs`.
            let rate = ((self.peak_ap - cur_ap) / decay_secs).max(0.0);
            self.lag_ap = (self.lag_ap - rate * dt).max(cur_ap);
            if self.lag_ap <= cur_ap {
                self.peak_ap = cur_ap;
            }
        } else {
            self.peak_ap = cur_ap;
        }

        // `0x829d0c58..0x829d0d08`: `Gauge_APEffect` double-gauge threshold & sine alpha blink.
        let diff_frac = if ap_max > 0.0 {
            (self.lag_ap - cur_ap) / ap_max
        } else {
            0.0
        };
        let threshold = tuning(301);
        if diff_frac < threshold {
            self.blink_timer = 0.0;
            self.effect_alpha = 0.0;
        } else {
            self.blink_timer += dt;
            let blink_hz = tuning(300);
            // `0x829d0cdc..0x829d0cf4`: `((sin(timer * blink_hz * PI) + 1.0) * 127.5) / 255.0`.
            self.effect_alpha =
                ((self.blink_timer * blink_hz * std::f32::consts::PI).sin() + 1.0) * 0.5;
        }

        // `0x829d0d28..0x829d0d64` -> `0x829d00c8`: ease damaged AP digit tint over `gauge_color_time`.
        let color_time = tuning(298);
        let step = if color_time > 0.0 {
            dt / color_time
        } else {
            1.0
        };
        let damaged = self.lag_ap > cur_ap + 0.5;
        self.damage_blend = (self.damage_blend + if damaged { step } else { -step }).clamp(0.0, 1.0);
    }

    /// Trailing fill fraction `lag_ap / ap_max` (`0x829d0c0c`).
    pub fn lag_fraction(&self, ap_max: f32) -> f32 {
        if ap_max > 0.0 {
            (self.lag_ap / ap_max).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

#[derive(Resource)]
pub struct Sortie {
    layout: Layout,
    /// Window size the HUD was last laid out for.
    size: Vec2,
    /// UI scale multiplier (`GameSettings::ui_scale`) the HUD was last laid out for.
    ui_scale: f32,
    /// Current layout scale factor (`min(size / SCREEN) * ui_scale`).
    scale: f32,
    /// Gauge show scale (`0x829d0b30` this+0x3f0), 0 when the lock sight appears.
    show: f32,
    /// Per-readout blend toward the low colour (gauge controller +0x14).
    low: [f32; 5],
    /// Trailing AP lag bar and damage-blink state (`0x8258c808` + `0x829d0b30`).
    pub ap_effect: ApEffectState,
}

impl Sortie {
    /// Forces [`layout`] to respawn all lock-sight HUD dialogs on the next frame (used when
    /// re-entering Sortie mode from the 3D Garage).
    pub fn reset_layout(&mut self) {
        self.size = Vec2::ZERO;
        self.show = 0.0;
    }
}

pub fn load(disc: &Disc, images: &mut Assets<Image>) -> anyhow::Result<Sortie> {
    Ok(Sortie {
        layout: menu::load(disc, LAYOUT, images)?,
        size: Vec2::ZERO,
        ui_scale: 1.0,
        scale: 1.0,
        show: 0.0,
        low: [0.0; 5],
        ap_effect: ApEffectState::default(),
    })
}

/// The piloted AC's AP and energy, as the HUD status block (`0x829d0b30` r5: AP +8, max +0xc,
/// EN +0x10, max +0x14) holds them.
#[derive(Component, Clone, Copy, Debug)]
pub struct Status {
    pub ap: u32,
    pub ap_max: u32,
    /// Current generator energy (`0x829d0b30` status `+0x10`, from `AC + 0x214` `+0x04`).
    pub en: f32,
    /// Generator energy capacity (`0x829d0b30` status `+0x14`, from `AC + 0x214` `+0x08`).
    pub en_max: f32,
    /// Live Overed Weapon phase (`0x82885c40..0x828865f8`).
    pub ow_phase: OwPhase,
    /// Overed Weapon charge gauge fraction in `0..=1` (`0x82885f10`).
    pub ow_charge: f32,
    /// Overed Weapon limit gauge fraction in `0..=1` (`0x82885fa8`).
    pub ow_limit: f32,
}

impl Status {
    /// Full AP (sum of frame parts' `ap`, `0x8286a600`) and full EN (`gene.en_capacity`,
    /// `0x8247a390` -> `0x82858408`).
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
        let en_max = acvd_data::part_field(design.gene as i64, 5, "en_capacity").max(0.0);
        Self {
            ap: ap_max,
            ap_max,
            en: en_max,
            en_max,
            ow_phase: OwPhase::Disarmed,
            ow_charge: 0.0,
            ow_limit: 0.0,
        }
    }

    /// Current EN fraction in `0..=1` (`0x82596b48` / `0x82593980`).
    pub fn energy_fraction(&self) -> f32 {
        if self.en_max > 0.0 {
            (self.en / self.en_max).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

/// A side `Weapon` panel and the hand it shows.
#[derive(Component)]
pub struct Panel(usize);

/// Screen-space target lock-on cursor wing from `sortie.drb` (`ACV_OutLine_LockCursor_E`,
/// `_R`, `_L`; `0x829d1b40` / `0x829d1910`).
#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub enum LockCursorWing {
    /// `ACV_OutLine_LockCursor_E` (bottom-right: `CursorLine`, `EnemyName`, `EnemyAP`).
    Enemy,
    /// `ACV_OutLine_LockCursor_R` (top-left: Right-arm `CursorLine`, `LockOnText`, `ReloadBar`).
    RightArm,
    /// `ACV_OutLine_LockCursor_L` (top-right: Left-arm `CursorLine`, `LockOnText`, `ReloadBar`).
    LeftArm,
}

/// Per-target multi-lock missile cursor dialog from `sortie.drb` (`ACV_OutLine_LockCursor_T`:
/// `CursorLine`, `LockOnText`, `MissileLock1..6`, `0x829d1910` / `0x829d1a10`), indexed `0..6`.
#[derive(Component)]
pub struct MissileLockDialog(usize);

/// Marker on the Combat Mode `ACV_LockSight_base` root (hidden while Scan Mode is active).
#[derive(Component)]
pub struct CombatSightRoot;

/// How a Lock-Sight root dialog tracks the 3D-projected aim point (`0x829d2120` via `0x829d3a08` /
/// `0x829d3a98`, `StaticDrawParam` 51/53 = 700 m) and aim/camera rotation (`0x829d1df0` mode 3 +
/// `0x829d24ec` / `0x829d2740`).
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct LockSightTrack {
    /// Center of the dialog in logical pixels relative to its root `Node` top-left (`Vec2::ZERO`
    /// for dialogs authored around `(0, 0)` like `ACV_FE_LockSightCenter`, `SCREEN * 0.5 * scale`
    /// for `ACV_LockSight_base`, `Vec2::splat(201.0 * rot_scale)` for `rocksight_out_rotation*`).
    pub center_offset: Vec2,
    /// `0` = camera roll only, `+1` = `rocksight_out_rotation`, `-1` = `rocksight_out_rotationInv`.
    pub spin_sign: i8,
    /// True for Overed Weapon lock-sight dialogs (`StaticDrawParam` 53, `+0xd0` = 700 m).
    pub ow: bool,
}

/// Relative scale of the 360 main lock-sight matrix (`0x829d2c08`, `sx = 0.6654`) to the center
/// matrix (`0x829d2c48`, `sx = 1.1336`) times the `rocksight_out` `[-275..275]` / `402` sub-dialog
/// ratio (`(550 / 402) * (0.6654 / 1.1336) ≈ 0.803`), placing `rocksight_out_rotation*` right on
/// the `163` px `Gauge_AP` / `Gauge_EN` outer ring (Xenia `private/xenia/hud_move.jsonl`).
const ROT_RING_SCALE: f32 = (550.0 / 402.0) * (0.6654177 / 1.1335863);

/// 3D world-space position of the Lock Sight billboard (`0x829d2120`: `origin + aim_dir * offset_dist`).
pub fn sight_world_pos(origin: Vec3, aim_dir: Vec3, offset_dist: f32) -> Vec3 {
    origin + aim_dir.normalize_or(Vec3::NEG_Z) * offset_dist
}

/// Lock-Sight rotating ring angle in Bevy Y-down UI radians (`0x829d1df0` mode 3:
/// `(pitch + yaw)` in 360 where right-turn is +yaw; in Bevy right-turn is -yaw, so `-(yaw + pitch)`
/// rotates clockwise when turning right and counter-clockwise when pitching up, matching Xenia
/// shots `private/xenia/shots/hud_turn_right.png` / `hud_pitch_up.png`).
pub fn sight_ring_rotation(yaw_rad: f32, pitch_rad: f32) -> f32 {
    -(yaw_rad + pitch_rad)
}

/// Top-left logical pixel origin for a `LockSightTrack` root given the projected screen point.
pub fn sight_root_origin(sight_vp: Vec2, center_offset: Vec2) -> Vec2 {
    sight_vp - center_offset
}

/// Marker on the Scan Mode `ACV_ScanModeLockSight` root (`0x825a26a0`, 1280×720 centered).
#[derive(Component)]
pub struct ScanSightRoot;

/// Marker on `ACV_Scan_Cur_Center` (`0x825a5978`, 70×70 target bracket + distance).
#[derive(Component)]
pub struct ScanCursorCenter;

/// Marker on `ACV_Scan_Cur_Scaning` (`0x825a26a0`, 100×100 scan progress bar + percent).
#[derive(Component)]
pub struct ScanProgressHud;

/// Marker on `ACV_Scan_Cur_EnemyParamInfo` (`0x825a5978` / `0x825a4460`, 300×187 AP/KE/CE/TE).
#[derive(Component)]
pub struct ScanEnemyParamHud;

/// Marker on `ACV_FE_Normal::Stagger` (`ErrorLog_Stagger`, dialog 208, rect `[158, 126, 346, 196]`).
#[derive(Component)]
pub struct StaggerHud;

/// Marker on Overed Weapon lock-sight dialogs (`ACV_OWLockSight` / `ACV_OWLockSightCenter`, `0x829d4e40`).
#[derive(Component)]
pub struct OwSightHud;

/// Marker on Overed Weapon overload / error-log dialog (`ACV_OW_ErrorLog`, `0x825aefe0`).
#[derive(Component)]
pub struct OwErrorHud;

/// Marker on `FE_EspialMachineInfo` (anchored at `ACV_FE_Normal::EspialMachineInfo`: recon ammo).
#[derive(Component)]
pub struct ReconAmmoHud;

/// Marker on `FE_ReconRaderMode` (anchored at `ACV_FE_Normal::ReconRadarMode`: detected count).
#[derive(Component)]
pub struct ReconRadarHud;

/// World-projected recon unit icon (`ACV_MapScan_EspialMachine` / `_InEnemy`, 320×320).
#[derive(Component, Clone, Copy)]
pub struct ReconIconHud {
    slot: usize,
    in_enemy: bool,
}

/// World-projected recon unit info label (`ACV_MapScan_ReconInfo` / `_InEnemy`, 128×128).
#[derive(Component, Clone, Copy)]
pub struct ReconInfoHud {
    slot: usize,
    in_enemy: bool,
}

/// World-projected marker for enemies detected inside a deployed recon's range (`ACV_MapScan_SubTargetS`).
#[derive(Component)]
pub struct ReconEnemyMarker(usize);

const MAX_RECON_ENEMY_MARKERS: usize = 12;

fn tuning(index: usize) -> f32 {
    acvd_data::tuning(STATIC_DRAW, index).unwrap_or_default() as f32
}

/// Primary (1st) or secondary (2nd) lock overlay color (`StaticDrawParam` indices 9..12 / 17..20).
fn lock_color(secondary: bool) -> Color {
    let base = if secondary { 17 } else { 9 };
    Color::srgba(
        tuning(base).clamp(0.0, 1.0),
        tuning(base + 1).clamp(0.0, 1.0),
        tuning(base + 2).clamp(0.0, 1.0),
        tuning(base + 3).clamp(0.5, 1.0),
    )
}

/// `gauge_low_r..a` (StaticDrawParam +0x444).
fn low_color() -> Color {
    let [r, g, b, a] = [305, 306, 307, 308].map(|i| tuning(i) as u8);
    Color::srgba_u8(r, g, b, a)
}

/// `ap_damage_r..a` (StaticDrawParam +0x484, `0x829d0290` at `0x829d03e8..0x829d0424`).
fn ap_damage_color() -> Color {
    let [r, g, b, a] = [325, 326, 327, 328].map(|i| tuning(i) as u8);
    Color::srgba_u8(r, g, b, a)
}

pub fn layout(
    mut commands: Commands,
    hud: Option<ResMut<Sortie>>,
    settings: Option<Res<crate::settings::GameSettings>>,
    window: Query<&Window, With<PrimaryWindow>>,
    roots: Query<Entity, With<MenuRoot>>,
) {
    let (Some(mut hud), Ok(window)) = (hud, window.single()) else {
        return;
    };
    let size = window.size();
    let ui_scale = settings
        .as_ref()
        .map(|s| {
            s.ui_scale
                .clamp(crate::settings::UI_SCALE_MIN, crate::settings::UI_SCALE_MAX)
        })
        .unwrap_or(1.0);
    if (size == hud.size && (ui_scale - hud.ui_scale).abs() < 1e-4) || size.min_element() <= 0.0 {
        return;
    }
    let only_ui_scale = size == hud.size && hud.size != Vec2::ZERO;
    hud.size = size;
    hud.ui_scale = ui_scale;
    if !only_ui_scale {
        hud.show = 0.0;
    }
    for e in &roots {
        commands.entity(e).despawn();
    }
    let base_scale = (size / SCREEN).min_element();
    let base_corner = (size - SCREEN * base_scale) / 2.0;
    let scale = base_scale * ui_scale;
    hud.scale = scale;
    let corner = (size - SCREEN * scale) / 2.0;
    // Anchor edge-placed 1280x720 HUD panels relative to their nearest screen edge so
    // scaling above 100% grows inward instead of clipping off-screen (100% is exact).
    let anchor_origin = |rect: [i16; 4]| -> Vec2 {
        let l = rect[0].min(rect[2]) as f32;
        let t = rect[1].min(rect[3]) as f32;
        let r = rect[0].max(rect[2]) as f32;
        let b = rect[1].max(rect[3]) as f32;
        let w = (r - l) * scale;
        let h = (b - t) * scale;
        let x = if (l + r) * 0.5 < SCREEN.x * 0.5 {
            base_corner.x + l * base_scale
        } else {
            base_corner.x + SCREEN.x * base_scale - (SCREEN.x - r) * base_scale - w
        };
        let y = if (t + b) * 0.5 < SCREEN.y * 0.5 {
            base_corner.y + t * base_scale
        } else {
            base_corner.y + SCREEN.y * base_scale - (SCREEN.y - b) * base_scale - h
        };
        Vec2::new(x, y)
    };
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
        let center_offset = if centered {
            Vec2::ZERO
        } else {
            SCREEN * scale * 0.5
        };
        let root = menu::spawn(&mut commands, &hud.layout, index, origin, scale);
        commands.entity(root).insert((
            LockSightTrack {
                center_offset,
                spin_sign: 0,
                ow: false,
            },
            UiTransform::IDENTITY,
        ));
        if name == "ACV_LockSight_base" {
            commands.entity(root).insert(CombatSightRoot);
        }
    }
    // Rotating outer lock-sight ring & triangles (`0x829d4e40` -> `0x829d2120`, driven by
    // `0x829d1df0` mode 3: `rocksight_out_rotation` rotates by `+rot`, `_Inv` by `-rot`).
    // Spawned directly from the 402x402 sub-dialogs so their `(0.5, 0.5)` pivot is dead-center.
    let rot_scale = scale * ROT_RING_SCALE;
    for (name, spin_sign) in [
        ("rocksight_out_rotation", 1i8),
        ("rocksight_out_rotationInv", -1i8),
    ] {
        if let Some(dlg) = dialog(name) {
            let sz = hud.layout.drb.dialogs[dlg].size;
            let center_offset = Vec2::new(sz[0] as f32, sz[1] as f32) * 0.5 * rot_scale;
            let origin = corner + SCREEN * scale * 0.5 - center_offset;
            let root = menu::spawn(&mut commands, &hud.layout, dlg, origin, rot_scale);
            commands.entity(root).insert((
                CombatSightRoot,
                LockSightTrack {
                    center_offset,
                    spin_sign,
                    ow: false,
                },
                UiTransform::IDENTITY,
            ));
        }
    }
    // Scan Mode lock sight (`ACV_ScanModeLockSight`, 1280x720 centered around screen middle).
    if let Some(dlg) = dialog("ACV_ScanModeLockSight") {
        let origin = corner + SCREEN * scale / 2.0;
        let root = menu::spawn(&mut commands, &hud.layout, dlg, origin, scale);
        commands.entity(root).insert((
            ScanSightRoot,
            LockSightTrack {
                center_offset: Vec2::ZERO,
                spin_sign: 0,
                ow: false,
            },
            UiTransform::IDENTITY,
            Visibility::Hidden,
        ));
    }
    // Scan Mode target bracket, scan progress gauge, and scanned enemy AP/KE/CE/TE info card.
    if let Some(dlg) = dialog("ACV_Scan_Cur_Center") {
        let root = menu::spawn(&mut commands, &hud.layout, dlg, Vec2::ZERO, scale);
        commands
            .entity(root)
            .insert((ScanCursorCenter, Visibility::Hidden));
    }
    if let Some(dlg) = dialog("ACV_Scan_Cur_Scaning") {
        let root = menu::spawn(&mut commands, &hud.layout, dlg, Vec2::ZERO, scale);
        commands
            .entity(root)
            .insert((ScanProgressHud, Visibility::Hidden));
    }
    if let Some(dlg) = dialog("ACV_Scan_Cur_EnemyParamInfo") {
        let root = menu::spawn(&mut commands, &hud.layout, dlg, Vec2::ZERO, scale);
        commands
            .entity(root)
            .insert((ScanEnemyParamHud, Visibility::Hidden));
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
        let origin = anchor_origin(rect);
        let root = menu::spawn(&mut commands, &hud.layout, weapon, origin, scale);
        commands.entity(root).insert(Panel(hand));
    }
    // Recon ammo (`FE_EspialMachineInfo`) and recon-detected enemy count (`FE_ReconRaderMode`)
    // at their `ACV_FE_Normal` anchors (`0x825171a0` / `0x82517b38`).
    if let (Some(anchor), Some(dlg)) = (
        hud.layout.drb.dialogs[normal]
            .objects
            .iter()
            .find(|o| o.name == "EspialMachineInfo"),
        dialog("FE_EspialMachineInfo"),
    ) {
        let rect = anchor.shape.rect();
        let origin = anchor_origin(rect);
        let root = menu::spawn(&mut commands, &hud.layout, dlg, origin, scale);
        commands
            .entity(root)
            .insert((ReconAmmoHud, Visibility::Hidden));
    }
    if let (Some(anchor), Some(dlg)) = (
        hud.layout.drb.dialogs[normal]
            .objects
            .iter()
            .find(|o| o.name == "ReconRadarMode"),
        dialog("FE_ReconRaderMode"),
    ) {
        let rect = anchor.shape.rect();
        let origin = anchor_origin(rect);
        let root = menu::spawn(&mut commands, &hud.layout, dlg, origin, scale);
        commands
            .entity(root)
            .insert((ReconRadarHud, Visibility::Hidden));
    }
    // Stagger warning panel (`ACV_FE_Normal::Stagger` -> `ErrorLog_Stagger` dialog 208,
    // rect [158, 126, 346, 196]; `0x825aefe0`).
    if let (Some(anchor), Some(dlg)) = (
        hud.layout.drb.dialogs[normal]
            .objects
            .iter()
            .find(|o| o.name == "Stagger"),
        dialog("ErrorLog_Stagger"),
    ) {
        let rect = anchor.shape.rect();
        let origin = anchor_origin(rect);
        let root = menu::spawn(&mut commands, &hud.layout, dlg, origin, scale);
        commands
            .entity(root)
            .insert((StaggerHud, Visibility::Hidden));
    }
    // Overed Weapon HUD dialogs (`0x829d4e40` `ACV_OWLockSight` / `ACV_OWLockSightCenter` centered
    // at screen center, and `0x825aefe0` `ACV_OW_ErrorLog` at screen corner).
    for name in ["ACV_OWLockSight", "ACV_OWLockSightCenter"] {
        if let Some(dlg) = dialog(name) {
            let root = menu::spawn(&mut commands, &hud.layout, dlg, size / 2.0, scale);
            commands.entity(root).insert((
                OwSightHud,
                LockSightTrack {
                    center_offset: Vec2::ZERO,
                    spin_sign: 0,
                    ow: true,
                },
                UiTransform::IDENTITY,
                Visibility::Hidden,
            ));
        }
    }
    if let Some(dlg) = dialog("ACV_OW_ErrorLog") {
        let root = menu::spawn(&mut commands, &hud.layout, dlg, corner, scale);
        commands
            .entity(root)
            .insert((OwErrorHud, Visibility::Hidden));
    }
    // World-projected Recon markers (`ACV_MapScan_EspialMachine*`, `ACV_MapScan_ReconInfo*`)
    // and recon-detected enemy markers (`ACV_MapScan_SubTargetS`).
    for (name, in_enemy) in [
        ("ACV_MapScan_EspialMachine", false),
        ("ACV_MapScan_EspialMachineInEnemy", true),
    ] {
        if let Some(dlg) = dialog(name) {
            for slot in 0..MAX_RECON_SLOTS {
                let root = menu::spawn(&mut commands, &hud.layout, dlg, Vec2::ZERO, scale);
                commands
                    .entity(root)
                    .insert((ReconIconHud { slot, in_enemy }, Visibility::Hidden));
            }
        }
    }
    for (name, in_enemy) in [
        ("ACV_MapScan_ReconInfo", false),
        ("ACV_MapScan_ReconInfoInEnemy", true),
    ] {
        if let Some(dlg) = dialog(name) {
            for slot in 0..MAX_RECON_SLOTS {
                let root = menu::spawn(&mut commands, &hud.layout, dlg, Vec2::ZERO, scale);
                commands
                    .entity(root)
                    .insert((ReconInfoHud { slot, in_enemy }, Visibility::Hidden));
            }
        }
    }
    if let Some(dlg) = dialog("ACV_MapScan_SubTargetS") {
        for slot in 0..MAX_RECON_ENEMY_MARKERS {
            let root = menu::spawn(&mut commands, &hud.layout, dlg, Vec2::ZERO, scale);
            commands
                .entity(root)
                .insert((ReconEnemyMarker(slot), Visibility::Hidden));
        }
    }
    // Spawn the actual in-game `sortie.drb` target lock-on cursor wings:
    // `_E` (bottom-right: enemy name/AP), `_R` (top-left: right arm), `_L` (top-right: left arm),
    // and 6x `_T` (bottom-left: per-target `MissileLock1..6`, `0x829d1910` / `0x829d1a10`).
    for (name, wing) in [
        ("ACV_OutLine_LockCursor_E", LockCursorWing::Enemy),
        ("ACV_OutLine_LockCursor_R", LockCursorWing::RightArm),
        ("ACV_OutLine_LockCursor_L", LockCursorWing::LeftArm),
    ] {
        if let Some(dlg) = dialog(name) {
            let root = menu::spawn(&mut commands, &hud.layout, dlg, Vec2::ZERO, scale);
            commands.entity(root).insert((wing, Visibility::Hidden));
        }
    }
    if let Some(msl_dlg) = dialog("ACV_OutLine_LockCursor_T") {
        for slot in 0..6 {
            let root = menu::spawn(&mut commands, &hud.layout, msl_dlg, Vec2::ZERO, scale);
            commands
                .entity(root)
                .insert((MissileLockDialog(slot), Visibility::Hidden));
        }
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
    arms.map(|a| if a.weapon_arm { a.hands[0] } else { a.hands[hand] })
        .filter(|g| g.kind != Kind::Skip)
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
            let f = s.energy_fraction();
            (
                (f * 10000.0) as u32,
                f,
                s.en_max > 0.0 && f < tuning(323),
            )
        }),
        Readout::LeftArm | Readout::RightArm => gun(arms, which).map(|g| {
            if g.kind == Kind::Blade && g.magazine == 0 {
                (0, 1.0, false)
            } else {
                let f = fraction(g.remaining as f32, g.magazine as f32);
                (g.remaining as u32, f, g.magazine > 0 && f < tuning(324))
            }
        }),
        Readout::Shoulder => arms
            .map(|a| a.shoulder)
            .filter(|g| g.kind != Kind::Skip)
            .map(|g| {
                let f = fraction(g.remaining as f32, g.magazine as f32);
                (g.remaining as u32, f, g.magazine > 0 && f < tuning(324))
            }),
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
    if let Some(s) = status {
        hud.ap_effect.step(s.ap as f32, s.ap_max as f32, dt);
    }
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
    scan_q: Query<&ScanState>,
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
    let in_scan = scan_q.single().is_ok_and(|s| s.active);
    let arms_for_hud = (!in_scan).then_some(arms).flatten();
    let low = low_color();
    for (object, mut visibility) in &mut plates {
        let Some(which) = chrome(&object.0) else {
            continue;
        };
        visibility.set_if_neq(if reading(which, status, arms_for_hud).is_some() {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
    }
    for (e, object, mut sprite, mut visibility, authored) in &mut digits {
        if let Some(which) = chrome(&object.0) {
            visibility.set_if_neq(if reading(which, status, arms_for_hud).is_some() {
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
        let value = reading(which, status, arms_for_hud);
        visibility.set_if_neq(if value.is_some() {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
        let Some((value, ..)) = value else { continue };
        // `0x829d0bd4` -> `0x825979e8`: AP digits count down smoothly using `lag_ap` and tint
        // toward `ap_damage_r..a` while damaged (`0x829d0d28` -> `0x829d00c8` / `0x829d09e8`).
        let (shown_value, base_color) = if which == Readout::Ap {
            let lag_val = (hud.ap_effect.lag_ap.round().max(0.0) as u32).min(999_999);
            (
                lag_val,
                mix(color, ap_damage_color(), hud.ap_effect.damage_blend),
            )
        } else {
            (value, color)
        };
        let color = mix(base_color, low, hud.low[slot(which)]);
        if sprite.color != color {
            sprite.color = color;
        }
        if place == 0 {
            continue;
        }
        let digit = (shown_value / place % 10) as f32;
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
        let value = reading(which, status, arms_for_hud);
        if object.0 == "Gauge_APEffect" {
            // `0x829d0c6c..0x829d0d24`: trailing damage bar fills to `lag_ap / ap_max * show`
            // and blinks at `ap_damage_blink_hz` when `(lag_ap - cur_ap) / ap_max >= ap_double_gauge_threshold`.
            let alpha = hud.ap_effect.effect_alpha;
            let shown = value.is_some() && alpha > 0.0;
            visibility.set_if_neq(if shown {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            });
            if shown {
                let ap_max = status.map_or(0.0, |s| s.ap_max as f32);
                let fill = hud.ap_effect.lag_fraction(ap_max) * hud.show;
                let base = color.to_srgba();
                let blink_color = Color::srgba(base.red, base.green, base.blue, alpha);
                if g.fill != fill || g.color != blink_color {
                    g.fill = fill;
                    g.color = blink_color;
                }
            }
            continue;
        }
        visibility.set_if_neq(if value.is_some() {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
        let Some((_, fraction, _)) = value else {
            continue;
        };
        let fill = fraction.clamp(0.0, 1.0) * hud.show;
        let color = mix(color, low, hud.low[slot(which)]);
        if g.fill != fill || g.color != color {
            g.fill = fill;
            g.color = color;
        }
    }
}

/// Marker for the `LIMIT OF AREA` / `OUTSIDE OF AREA` warning banner (`manual.fmg` 20500,
/// `AreaOverLine` `0x825b3678`).
#[derive(Component)]
pub struct AreaWarningHud;

#[derive(Component)]
pub struct AreaWarningText;

/// Shows `LIMIT OF AREA` (amber/orange) or `OUTSIDE OF AREA` (red) when the AC leaves the
/// map caution/warning/operation bounds (`POINT_PARAM_ST` kinds `100`–`102`).
pub fn area_warning_hud(
    time: Res<Time>,
    pilots: Query<&crate::control::Pilot>,
    mut commands: Commands,
    mut banner: Query<(&mut Visibility, &mut BackgroundColor), With<AreaWarningHud>>,
    mut label: Query<(&mut Text, &mut TextColor), With<AreaWarningText>>,
) {
    use crate::map::AreaZone;
    let zone = pilots.single().map_or(AreaZone::Inside, |p| p.area_zone);
    if banner.is_empty() {
        commands
            .spawn((
                AreaWarningHud,
                Node {
                    position_type: PositionType::Absolute,
                    top: px(88.0),
                    left: Val::Percent(50.0),
                    margin: UiRect::left(px(-150.0)),
                    width: px(300.0),
                    padding: UiRect::axes(px(14.0), px(6.0)),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },
                BackgroundColor(Color::srgba(0.18, 0.06, 0.02, 0.82)),
                Visibility::Hidden,
            ))
            .with_children(|c| {
                c.spawn((
                    AreaWarningText,
                    Text::new("LIMIT OF AREA"),
                    TextFont {
                        font_size: FontSize::Px(20.0),
                        ..default()
                    },
                    TextColor(Color::srgb(1.0, 0.72, 0.15)),
                ));
            });
        return;
    }

    let pulse = ((time.elapsed_secs() * 8.0).sin() * 0.5 + 0.5).clamp(0.25, 1.0);
    let (show, msg, fg, bg) = match zone {
        AreaZone::Inside => (
            false,
            "",
            Color::WHITE,
            Color::srgba(0.0, 0.0, 0.0, 0.0),
        ),
        AreaZone::Caution => (
            true,
            "LIMIT OF AREA",
            Color::srgba(1.0, 0.78, 0.2, pulse),
            Color::srgba(0.22, 0.12, 0.02, 0.78 * pulse),
        ),
        AreaZone::Warning => (
            true,
            "LIMIT OF AREA",
            Color::srgba(1.0, 0.42, 0.12, pulse),
            Color::srgba(0.32, 0.08, 0.02, 0.85 * pulse),
        ),
        AreaZone::Outside => (
            true,
            "OUTSIDE OF AREA",
            Color::srgba(1.0, 0.18, 0.15, pulse),
            Color::srgba(0.4, 0.03, 0.03, 0.9 * pulse),
        ),
    };
    for (mut vis, mut bg_col) in &mut banner {
        vis.set_if_neq(if show {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
        *bg_col = BackgroundColor(bg);
    }
    if show {
        for (mut text, mut color) in &mut label {
            if text.0 != msg {
                text.0 = msg.to_string();
            }
            *color = TextColor(fg);
        }
    }
}

/// Shows `ACV_FE_Normal::Stagger` (`ErrorLog_Stagger`, dialog 208) while the player AC is
/// staggered (`PlayerHealth::is_staggered()`).
pub fn stagger_hud(
    health: Query<&PlayerHealth>,
    mut panel: Query<&mut Visibility, With<StaggerHud>>,
) {
    let staggered = health.single().is_ok_and(PlayerHealth::is_staggered);
    for mut vis in &mut panel {
        vis.set_if_neq(if staggered {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
    }
}

/// Shows Overed Weapon HUD dialogs (`ACV_OWLockSight` / `ACV_OWLockSightCenter` `0x829d4e40`,
/// `ACV_OW_ErrorLog` `0x825aefe0`) and updates OW charge/limit gauges while an OW is active or
/// in post-purge lockout.
#[allow(clippy::type_complexity)]
pub fn ow_hud(
    status: Query<&Status>,
    mut sight: Query<(Entity, &mut Visibility), (With<OwSightHud>, Without<OwErrorHud>)>,
    mut err: Query<(Entity, &mut Visibility), (With<OwErrorHud>, Without<OwSightHud>)>,
    children: Query<&Children>,
    mut gauges: Query<(&MenuObject, &mut MenuGauge)>,
    mut texts: Query<(&MenuObject, &mut MenuText)>,
    mut sub_vis: Query<(&MenuObject, &mut Visibility), (Without<OwSightHud>, Without<OwErrorHud>)>,
) {
    let st = status.single().ok();
    let active = st.is_some_and(|s| {
        matches!(
            s.ow_phase,
            OwPhase::Arming
                | OwPhase::Armed
                | OwPhase::Charging
                | OwPhase::Charged
                | OwPhase::Attacking
        )
    });
    let show_err = active || st.is_some_and(|s| s.ow_phase == OwPhase::Lockout);
    for (root, mut vis) in &mut sight {
        vis.set_if_neq(if active {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
        if active {
            if let Some(s) = st {
                for child in children.iter_descendants(root) {
                    if let Ok((obj, mut g)) = gauges.get_mut(child) {
                        let name = obj.0.to_lowercase();
                        g.fill = if name.contains("charge") {
                            s.ow_charge
                        } else {
                            s.ow_limit
                        };
                    }
                    if let Ok((obj, mut v)) = sub_vis.get_mut(child) {
                        if matches!(
                            obj.0.as_str(),
                            "Hit" | "Ineffective" | "Danger" | "Booster"
                                | "Status_Hit" | "Status_Ineffective" | "Status_Danger" | "Status_Booster"
                        ) {
                            v.set_if_neq(Visibility::Hidden);
                        }
                    }
                }
            }
        }
    }
    for (root, mut vis) in &mut err {
        vis.set_if_neq(if show_err {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
        if show_err {
            if let Some(s) = st {
                let limit_pct = format!("{:06.2}%", s.ow_limit * 100.0);
                for child in children.iter_descendants(root) {
                    if let Ok((_, mut g)) = gauges.get_mut(child) {
                        g.fill = s.ow_limit;
                    }
                    if let Ok((obj, mut t)) = texts.get_mut(child) {
                        let name = obj.0.to_lowercase();
                        if name.contains("limit") || t.text.contains('%') || t.text.contains("000.00") {
                            if t.text != limit_pct {
                                t.text = limit_pct.clone();
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Fills the side panels' `CurAmmo` and shows `Empty` at zero (`0x825970f0`).
/// Hidden while in Scan Mode (`AC + 0xdb0 != 0`).
pub fn panels(
    arms: Query<&Armament>,
    scan_q: Query<&ScanState>,
    panels: Query<(Entity, &Panel)>,
    children: Query<&Children>,
    mut objects: Query<(&MenuObject, &mut Visibility, Option<&mut MenuText>)>,
) {
    let in_scan = scan_q.single().is_ok_and(|s| s.active);
    let arms = (!in_scan).then(|| arms.single().ok()).flatten();
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

/// Switches between Combat Mode (`ACV_LockSight_base`) and Scan Mode (`ACV_ScanModeLockSight`),
/// and updates `ACV_Scan_Cur_Center`, `ACV_Scan_Cur_Scaning`, and `ACV_Scan_Cur_EnemyParamInfo`
/// (`0x825a26a0`, `0x825a5978`, `0x825a4460`).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn scan_hud(
    hud: Option<Res<Sortie>>,
    cameras: Query<(&Camera, &GlobalTransform), (With<Camera3d>, Without<crate::env::WaterReflectionCamera>)>,
    scan_q: Query<&ScanState>,
    mut combat_sight: Query<
        &mut Visibility,
        (
            With<CombatSightRoot>,
            Without<ScanSightRoot>,
            Without<ScanCursorCenter>,
            Without<ScanProgressHud>,
            Without<ScanEnemyParamHud>,
            Without<MenuObject>,
        ),
    >,
    mut scan_sight: Query<
        &mut Visibility,
        (
            With<ScanSightRoot>,
            Without<CombatSightRoot>,
            Without<ScanCursorCenter>,
            Without<ScanProgressHud>,
            Without<ScanEnemyParamHud>,
            Without<MenuObject>,
        ),
    >,
    mut center_q: Query<
        (Entity, &mut Node, &mut Visibility),
        (
            With<ScanCursorCenter>,
            Without<CombatSightRoot>,
            Without<ScanSightRoot>,
            Without<ScanProgressHud>,
            Without<ScanEnemyParamHud>,
            Without<MenuObject>,
        ),
    >,
    mut progress_q: Query<
        (Entity, &mut Node, &mut Visibility),
        (
            With<ScanProgressHud>,
            Without<CombatSightRoot>,
            Without<ScanSightRoot>,
            Without<ScanCursorCenter>,
            Without<ScanEnemyParamHud>,
            Without<MenuObject>,
        ),
    >,
    mut param_q: Query<
        (Entity, &mut Node, &mut Visibility),
        (
            With<ScanEnemyParamHud>,
            Without<CombatSightRoot>,
            Without<ScanSightRoot>,
            Without<ScanCursorCenter>,
            Without<ScanProgressHud>,
            Without<MenuObject>,
        ),
    >,
    children: Query<&Children>,
    mut objects: Query<
        (
            &MenuObject,
            &mut Node,
            &mut Visibility,
            Option<&mut MenuText>,
            Option<&mut MenuGauge>,
        ),
        (
            Without<CombatSightRoot>,
            Without<ScanSightRoot>,
            Without<ScanCursorCenter>,
            Without<ScanProgressHud>,
            Without<ScanEnemyParamHud>,
        ),
    >,
) {
    let Some(hud) = hud else { return };
    let scale = hud.scale;
    let size = hud.size;
    let scan = scan_q.single().ok();
    let active = scan.is_some_and(|s| s.active);
    let cam_pair = cameras.single().ok();

    for mut vis in &mut combat_sight {
        vis.set_if_neq(if active {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        });
    }
    for mut vis in &mut scan_sight {
        vis.set_if_neq(if active {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
    }

    // 1. Target bracket (`ACV_Scan_Cur_Center`, centered at CenterFrame [20, 20]).
    let cand_vp = scan
        .filter(|s| s.active)
        .and_then(|s| s.candidate.as_ref())
        .and_then(|cand| {
            let (cam, cam_gt) = cam_pair?;
            let vp = cam.world_to_viewport(cam_gt, cand.world_pos).ok()?;
            Some((cand, vp))
        });
    for (root, mut node, mut vis) in &mut center_q {
        let Some((cand, vp)) = cand_vp else {
            vis.set_if_neq(Visibility::Hidden);
            continue;
        };
        vis.set_if_neq(Visibility::Inherited);
        node.left = px(vp.x - 20.0 * scale);
        node.top = px(vp.y - 20.0 * scale);
        for child in children.iter_descendants(root) {
            let Ok((obj, _, _, text, _)) = objects.get_mut(child) else {
                continue;
            };
            match obj.0.as_str() {
                "dist" => {
                    if let Some(mut t) = text {
                        let s = format!("{:.0}", cand.distance);
                        if t.text != s {
                            t.text = s;
                        }
                    }
                }
                "EnemyText" => {
                    if let Some(mut t) = text {
                        if t.text != cand.model {
                            t.text = cand.model.clone();
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // 2. Scan progress bar (`ACV_Scan_Cur_Scaning`, below center of screen while scanning).
    let show_progress = cand_vp.filter(|(cand, _)| {
        let already_shown = scan
            .and_then(|s| s.scanned.as_ref())
            .is_some_and(|sc| sc.entity == cand.entity);
        !already_shown || !cand.complete()
    });
    for (root, mut node, mut vis) in &mut progress_q {
        let Some((cand, _)) = show_progress else {
            vis.set_if_neq(Visibility::Hidden);
            continue;
        };
        vis.set_if_neq(Visibility::Inherited);
        node.left = px(size.x * 0.5);
        node.top = px(size.y * 0.62);
        let prog = cand.progress();
        for child in children.iter_descendants(root) {
            let Ok((obj, mut cnode, _, text, _)) = objects.get_mut(child) else {
                continue;
            };
            match obj.0.as_str() {
                "ScanProgressText" => {
                    if let Some(mut t) = text {
                        let s = format!("{}%", (prog * 100.0).round() as u32);
                        if t.text != s {
                            t.text = s;
                        }
                    }
                }
                "ScanProgressBar" => {
                    // Authored rect [-125, 32, 129, 34] -> full width 254 px.
                    cnode.width = px(254.0 * prog * scale);
                }
                _ => {}
            }
        }
    }

    // 3. Scanned enemy AP and KE/CE/TE defense stats (`ACV_Scan_Cur_EnemyParamInfo`, `0x825a4460`).
    let scanned_vp = scan
        .filter(|s| s.active)
        .and_then(|s| s.scanned.as_ref())
        .and_then(|info| {
            let (cam, cam_gt) = cam_pair?;
            let vp = cam.world_to_viewport(cam_gt, info.world_pos).ok()?;
            Some((info, vp))
        });
    for (root, mut node, mut vis) in &mut param_q {
        let Some((info, vp)) = scanned_vp else {
            vis.set_if_neq(Visibility::Hidden);
            continue;
        };
        vis.set_if_neq(Visibility::Inherited);
        // `CursorLine` sits at [247, 154, 300, 187]; clamp so EnemyName (-129..240) and
        // EnemyParam (127..249) stay fully readable inside the window.
        let min_x = 140.0 * scale;
        let max_x = (size.x - 310.0 * scale).max(min_x);
        let min_y = 16.0 * scale;
        let max_y = (size.y - 200.0 * scale).max(min_y);
        node.left = px((vp.x - 305.0 * scale).clamp(min_x, max_x));
        node.top = px((vp.y - 205.0 * scale).clamp(min_y, max_y));
        for child in children.iter_descendants(root) {
            let Ok((obj, _, mut cvis, text, gauge)) = objects.get_mut(child) else {
                continue;
            };
            match obj.0.as_str() {
                "EnemyName" => {
                    if let Some(mut t) = text {
                        if t.text != info.model {
                            t.text = info.model.clone();
                        }
                    }
                }
                "EnemyAP" => {
                    if let Some(mut t) = text {
                        let s = format!("{:.0}", info.ap);
                        if t.text != s {
                            t.text = s;
                        }
                    }
                }
                "EnemyKE" => {
                    if let Some(mut t) = text {
                        let s = format!("{}", info.ke_def);
                        if t.text != s {
                            t.text = s;
                        }
                    }
                }
                "EnemyCE" => {
                    if let Some(mut t) = text {
                        let s = format!("{}", info.ce_def);
                        if t.text != s {
                            t.text = s;
                        }
                    }
                }
                "EnemyTE" => {
                    if let Some(mut t) = text {
                        let s = format!("{}", info.te_def);
                        if t.text != s {
                            t.text = s;
                        }
                    }
                }
                "CursorLine" => {
                    if let Some(mut g) = gauge {
                        g.fill = 1.0;
                    }
                }
                // Runtime texture slot 14 is a blank placeholder without dynamic portrait rendering.
                "EmblemPic" | "EnemyPic" => {
                    if text.is_none() && gauge.is_none() {
                        // Only hide the inner Sprite (not the outer EnemyPic Dialog container).
                        // The inner Sprite has no children, whereas the Dialog container does.
                        if children.get(child).is_err() {
                            cvis.set_if_neq(Visibility::Hidden);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// Updates the `sortie.drb` Recon HUD dialogs:
/// - `FE_EspialMachineInfo` (remaining recon ammo)
/// - `FE_ReconRaderMode` (total enemies detected by active recons)
/// - `ACV_MapScan_EspialMachine` / `_InEnemy` & `ACV_MapScan_ReconInfo` / `_InEnemy` (per-recon markers)
/// - `ACV_MapScan_SubTargetS` (recon-detected enemy markers in Scan Mode).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn recon_hud(
    hud: Option<Res<Sortie>>,
    cameras: Query<(&Camera, &GlobalTransform), (With<Camera3d>, Without<crate::env::WaterReflectionCamera>)>,
    scan_q: Query<&ScanState>,
    recons: Query<(&ReconUnit, &Transform)>,
    mut ammo_q: Query<
        (Entity, &mut Visibility),
        (
            With<ReconAmmoHud>,
            Without<ReconRadarHud>,
            Without<ReconIconHud>,
            Without<ReconInfoHud>,
            Without<ReconEnemyMarker>,
            Without<MenuObject>,
        ),
    >,
    mut radar_q: Query<
        (Entity, &mut Visibility),
        (
            With<ReconRadarHud>,
            Without<ReconAmmoHud>,
            Without<ReconIconHud>,
            Without<ReconInfoHud>,
            Without<ReconEnemyMarker>,
            Without<MenuObject>,
        ),
    >,
    mut icon_q: Query<
        (&ReconIconHud, &mut Node, &mut Visibility),
        (
            Without<ReconAmmoHud>,
            Without<ReconRadarHud>,
            Without<ReconInfoHud>,
            Without<ReconEnemyMarker>,
            Without<MenuObject>,
        ),
    >,
    mut info_q: Query<
        (Entity, &ReconInfoHud, &mut Node, &mut Visibility),
        (
            Without<ReconAmmoHud>,
            Without<ReconRadarHud>,
            Without<ReconIconHud>,
            Without<ReconEnemyMarker>,
            Without<MenuObject>,
        ),
    >,
    mut enemy_q: Query<
        (&ReconEnemyMarker, &mut Node, &mut Visibility),
        (
            Without<ReconAmmoHud>,
            Without<ReconRadarHud>,
            Without<ReconIconHud>,
            Without<ReconInfoHud>,
            Without<MenuObject>,
        ),
    >,
    children: Query<&Children>,
    mut objects: Query<
        (&MenuObject, Option<&mut MenuText>),
        (
            Without<ReconAmmoHud>,
            Without<ReconRadarHud>,
            Without<ReconIconHud>,
            Without<ReconInfoHud>,
            Without<ReconEnemyMarker>,
        ),
    >,
) {
    let Some(hud) = hud else { return };
    let scale = hud.scale;
    let scan = scan_q.single().ok();
    let cam_pair = cameras.single().ok();

    let mut active_recons: Vec<(&ReconUnit, Vec3)> =
        recons.iter().map(|(r, tf)| (r, tf.translation)).collect();
    active_recons.sort_by_key(|(r, _)| r.index);

    // 1. Remaining recon count (`FE_EspialMachineInfo`).
    for (root, mut vis) in &mut ammo_q {
        let Some(s) = scan.filter(|s| s.recon_spec.equipped()) else {
            vis.set_if_neq(Visibility::Hidden);
            continue;
        };
        vis.set_if_neq(Visibility::Inherited);
        for child in children.iter_descendants(root) {
            if let Ok((obj, Some(mut t))) = objects.get_mut(child) {
                if obj.0 == "EspialMachineNum" {
                    let txt = format!("{}", s.recon_remaining);
                    if t.text != txt {
                        t.text = txt;
                    }
                }
            }
        }
    }

    // 2. Total enemies detected by active recons (`FE_ReconRaderMode`).
    let detected_total = scan.map_or(0, |s| s.recon_detected_targets.len());
    for (root, mut vis) in &mut radar_q {
        if active_recons.is_empty() {
            vis.set_if_neq(Visibility::Hidden);
            continue;
        }
        vis.set_if_neq(Visibility::Inherited);
        for child in children.iter_descendants(root) {
            if let Ok((obj, Some(mut t))) = objects.get_mut(child) {
                if obj.0 == "ReconRaderEnemeyNum" {
                    let txt = format!("{detected_total}");
                    if t.text != txt {
                        t.text = txt;
                    }
                }
            }
        }
    }

    // 3. Per-recon world-projected icons & info tags (`ACV_MapScan_EspialMachine*`, `_ReconInfo*`).
    let projected: Vec<Option<(Vec2, u32, usize)>> = (0..MAX_RECON_SLOTS)
        .map(|slot| {
            let &(recon, pos) = active_recons.get(slot)?;
            let (cam, cam_gt) = cam_pair?;
            let vp = cam.world_to_viewport(cam_gt, pos).ok()?;
            Some((vp, recon.index, recon.detected_count))
        })
        .collect();

    for (icon, mut node, mut vis) in &mut icon_q {
        let Some(&(vp, _, count)) = projected.get(icon.slot).and_then(Option::as_ref) else {
            vis.set_if_neq(Visibility::Hidden);
            continue;
        };
        let has_enemy = count > 0;
        if icon.in_enemy != has_enemy {
            vis.set_if_neq(Visibility::Hidden);
            continue;
        }
        vis.set_if_neq(Visibility::Inherited);
        // `EspialMachine` sprite rect is [154, 134, 167, 160] -> center ~(160, 147).
        node.left = px(vp.x - 160.0 * scale);
        node.top = px(vp.y - 147.0 * scale);
    }

    for (root, info, mut node, mut vis) in &mut info_q {
        let Some(&(vp, idx, count)) = projected.get(info.slot).and_then(Option::as_ref) else {
            vis.set_if_neq(Visibility::Hidden);
            continue;
        };
        let has_enemy = count > 0;
        if info.in_enemy != has_enemy {
            vis.set_if_neq(Visibility::Hidden);
            continue;
        }
        vis.set_if_neq(Visibility::Inherited);
        node.left = px(vp.x + 8.0 * scale);
        node.top = px(vp.y - 36.0 * scale);
        for child in children.iter_descendants(root) {
            let Ok((obj, Some(mut t))) = objects.get_mut(child) else {
                continue;
            };
            match obj.0.as_str() {
                "index_text" => {
                    let txt = format!("{idx}");
                    if t.text != txt {
                        t.text = txt;
                    }
                }
                "enemyNum_text" => {
                    let txt = format!("{count}");
                    if t.text != txt {
                        t.text = txt;
                    }
                }
                _ => {}
            }
        }
    }

    // 4. Recon-detected enemy markers (`ACV_MapScan_SubTargetS`) in Scan Mode.
    let in_scan = scan.is_some_and(|s| s.active);
    let enemy_vps: Vec<Vec2> = if in_scan {
        scan.into_iter()
            .flat_map(|s| s.recon_detected_targets.iter())
            .filter_map(|&(_, pos)| {
                let (cam, cam_gt) = cam_pair?;
                cam.world_to_viewport(cam_gt, pos).ok()
            })
            .take(MAX_RECON_ENEMY_MARKERS)
            .collect()
    } else {
        Vec::new()
    };
    for (marker, mut node, mut vis) in &mut enemy_q {
        let Some(&vp) = enemy_vps.get(marker.0) else {
            vis.set_if_neq(Visibility::Hidden);
            continue;
        };
        vis.set_if_neq(Visibility::Inherited);
        // `SubTarget_S` rect is [137, 137, 183, 183] -> center (160, 160).
        node.left = px(vp.x - 160.0 * scale);
        node.top = px(vp.y - 160.0 * scale);
    }
}

/// Projects the FCS-locked target(s) onto the viewport and updates the actual `sortie.drb`
/// lock-on dialogs:
/// - `ACV_OutLine_LockCursor_E` (`EnemyName`, `EnemyAP`, `CursorLine`, `0x829d1b40`)
/// - `ACV_OutLine_LockCursor_R` / `_L` (per-arm `LockOnText`, `ReloadBar`, `CursorLine`)
/// - `ACV_OutLine_LockCursor_T` (per-target `MissileLock1..6`, `LockOnText`, `CursorLine`,
///   `0x829d1910` / `0x829d1a10`).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn lock_cursor(
    hud: Option<Res<Sortie>>,
    cameras: Query<(&Camera, &GlobalTransform), (With<Camera3d>, Without<crate::env::WaterReflectionCamera>)>,
    fcs_q: Query<&Fcs>,
    mut wings: Query<
        (Entity, &LockCursorWing, &mut Node, &mut Visibility),
        (Without<MissileLockDialog>, Without<MenuObject>),
    >,
    mut msl_dialogs: Query<
        (Entity, &MissileLockDialog, &mut Node, &mut Visibility),
        (Without<LockCursorWing>, Without<MenuObject>),
    >,
    children: Query<&Children>,
    mut objects: Query<
        (
            &MenuObject,
            &mut Visibility,
            Option<&mut MenuText>,
            Option<&mut MenuGauge>,
        ),
        (Without<LockCursorWing>, Without<MissileLockDialog>),
    >,
) {
    let Some(hud) = hud else { return };
    let scale = hud.scale;
    let fcs = fcs_q.single().ok();
    let cam_pair = cameras.single().ok();
    let locked = fcs.and_then(|f| f.locked.as_ref());
    let projected = locked.and_then(|lock| {
        let (cam, cam_gt) = cam_pair?;
        let vp = cam.world_to_viewport(cam_gt, lock.world_pos).ok()?;
        Some((lock, vp))
    });

    for (root, &wing, mut node, mut vis) in &mut wings {
        let Some((lock, vp)) = projected else {
            vis.set_if_neq(Visibility::Hidden);
            continue;
        };
        vis.set_if_neq(Visibility::Inherited);
        match wing {
            LockCursorWing::Enemy => {
                // `CursorLine` at (14,37)-(128,64) -> bottom-right wing below target center.
                node.left = px(vp.x - 14.0 * scale);
                node.top = px(vp.y - 24.0 * scale);
                let col = lock_color(lock.any_secondary());
                for child in children.iter_descendants(root) {
                    let Ok((obj, mut cvis, text, gauge)) = objects.get_mut(child) else {
                        continue;
                    };
                    let show = match obj.0.as_str() {
                        "CursorLine" => {
                            if let Some(mut g) = gauge {
                                g.fill = 1.0;
                                g.color = col;
                            }
                            true
                        }
                        "EnemyName" => {
                            if let Some(mut t) = text {
                                if t.text != lock.model {
                                    t.text = lock.model.clone();
                                }
                            }
                            true
                        }
                        "EnemyAP" => {
                            if let Some(mut t) = text {
                                let s = format!("{:.0}", lock.ap);
                                if t.text != s {
                                    t.text = s;
                                }
                            }
                            true
                        }
                        "EnemySquadNumber" => false,
                        _ => true,
                    };
                    cvis.set_if_neq(if show {
                        Visibility::Inherited
                    } else {
                        Visibility::Hidden
                    });
                }
            }
            LockCursorWing::RightArm | LockCursorWing::LeftArm => {
                let hand = usize::from(wing == LockCursorWing::LeftArm);
                if wing == LockCursorWing::RightArm {
                    // `CursorLine` at (0,0)-(104,27) -> top-left wing above target center.
                    node.left = px(vp.x - 104.0 * scale);
                    node.top = px(vp.y - 42.0 * scale);
                } else {
                    // `CursorLine` at (24,0)-(128,27) -> top-right wing above target center.
                    node.left = px(vp.x - 24.0 * scale);
                    node.top = px(vp.y - 42.0 * scale);
                }
                let prog = lock.progress(hand);
                let ready = lock.secondary[hand];
                let col = lock_color(ready);
                for child in children.iter_descendants(root) {
                    let Ok((obj, mut cvis, text, gauge)) = objects.get_mut(child) else {
                        continue;
                    };
                    let show = match obj.0.as_str() {
                        "CursorLine" => {
                            if let Some(mut g) = gauge {
                                g.fill = prog.max(0.25);
                                g.color = col;
                            }
                            true
                        }
                        "LockOnText" => {
                            if let Some(mut t) = text {
                                let s = if ready {
                                    "LOCK ON".to_string()
                                } else {
                                    format!("{}%", (prog * 100.0).round() as u32)
                                };
                                if t.text != s {
                                    t.text = s;
                                }
                            }
                            true
                        }
                        "ReloadBar" => !ready,
                        name if name.starts_with("MissileLock") => false,
                        _ => true,
                    };
                    cvis.set_if_neq(if show {
                        Visibility::Inherited
                    } else {
                        Visibility::Hidden
                    });
                }
            }
        }
    }

    // Update per-target `ACV_OutLine_LockCursor_T` dialogs (`MissileLock1..6`, `0x829d1910` / `0x829d1a10`).
    let msl_targets: Vec<(Vec2, u8)> = fcs
        .into_iter()
        .flat_map(Fcs::missile_targets)
        .filter_map(|(_, pos, count)| {
            let (cam, cam_gt) = cam_pair?;
            let vp = cam.world_to_viewport(cam_gt, pos).ok()?;
            Some((vp, count))
        })
        .take(6)
        .collect();
    let msl_col = lock_color(true);
    for (root, dlg, mut node, mut vis) in &mut msl_dialogs {
        let Some(&(vp, count)) = msl_targets.get(dlg.0) else {
            vis.set_if_neq(Visibility::Hidden);
            continue;
        };
        vis.set_if_neq(Visibility::Inherited);
        // `CursorLine` at (0,37)-(104,64) -> bottom-left wing below target center.
        node.left = px(vp.x - 104.0 * scale);
        node.top = px(vp.y - 24.0 * scale);
        for child in children.iter_descendants(root) {
            let Ok((obj, mut cvis, text, gauge)) = objects.get_mut(child) else {
                continue;
            };
            let show = match obj.0.as_str() {
                "CursorLine" => {
                    if let Some(mut g) = gauge {
                        g.fill = 1.0;
                        g.color = msl_col;
                    }
                    true
                }
                "LockOnText" => {
                    if let Some(mut t) = text {
                        let s = format!("MSL x{count}");
                        if t.text != s {
                            t.text = s;
                        }
                    }
                    true
                }
                "ReloadBar" => false,
                name => {
                    if let Some(n) = name
                        .strip_prefix("MissileLock")
                        .and_then(|s| s.parse::<u8>().ok())
                    {
                        n <= count
                    } else {
                        true
                    }
                }
            };
            cvis.set_if_neq(if show {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            });
        }
    }
}

/// Projects the Lock-Sight billboard 700 m along the AC's aim direction (`0x829d2120` via
/// `0x829d3a08` / `0x829d3a98`, `StaticDrawParam` 51/53) and rotates `ACV_FE_LockSightRotation` /
/// `ACV_FE_LockSightRotationInv` by `±(yaw + pitch)` (`0x829d1df0` mode 3) plus follow-camera roll.
#[allow(clippy::type_complexity)]
pub fn track_lock_sight(
    hud: Option<Res<Sortie>>,
    mark: Option<Res<ShotMark>>,
    cameras: Query<
        (&Camera, &Transform),
        (With<Camera3d>, Without<crate::env::WaterReflectionCamera>),
    >,
    pilots: Query<(&Pilot, &Transform), Without<Camera3d>>,
    mut roots: Query<(&LockSightTrack, &mut Node, &mut UiTransform)>,
    mut shot_logged: Local<bool>,
) {
    let Some(hud) = hud else { return };
    let (Ok((cam, cam_tf)), Ok((pilot, ac_tf))) = (cameras.single(), pilots.single()) else {
        return;
    };
    let cam_gt = GlobalTransform::from(*cam_tf);
    let origin = pilot.sight_origin(ac_tf.translation);
    let aim_dir = pilot.sight_direction();
    let (yaw, pitch) = pilot.aim_angles();
    let roll = pilot.camera_roll();
    let ring_rot = sight_ring_rotation(yaw, pitch);
    let normal_dist = tuning(51).max(1.0);
    let ow_dist = tuning(53).max(1.0);
    let normal_vp = cam
        .world_to_viewport(&cam_gt, sight_world_pos(origin, aim_dir, normal_dist))
        .ok();
    let ow_vp = if (ow_dist - normal_dist).abs() < 1e-3 {
        normal_vp
    } else {
        cam.world_to_viewport(&cam_gt, sight_world_pos(origin, aim_dir, ow_dist))
            .ok()
    };

    for (track, mut node, mut ui_tf) in &mut roots {
        let Some(vp) = (if track.ow { ow_vp } else { normal_vp }) else {
            continue;
        };
        let pos = sight_root_origin(vp, track.center_offset);
        node.left = px(pos.x);
        node.top = px(pos.y);
        if track.center_offset == Vec2::ZERO {
            // Centered DRB dialogs (e.g. `ACV_FE_LockSightCenter`) have `size = [315, 315]` while
            // their child sprites are authored around `(0, 0)`. Zeroing the root box puts Bevy's
            // `(0.5, 0.5)` `UiTransform` pivot right on `(0, 0)` = `vp`.
            node.width = px(0.0);
            node.height = px(0.0);
        }
        ui_tf.rotation = Rot2::radians(roll + f32::from(track.spin_sign) * ring_rot);
    }

    if mark.is_some_and(|m| m.path.is_some()) && !*shot_logged {
        *shot_logged = true;
        if let Some(vp) = normal_vp {
            let off = vp - hud.size * 0.5;
            eprintln!(
                "SHOT hud_sight vp=({:.1},{:.1}) offset=({:+.1},{:+.1}) yaw_deg={:.1} pitch_deg={:.1} roll_deg={:.1} ring_deg={:.1}",
                vp.x,
                vp.y,
                off.x,
                off.y,
                yaw.to_degrees(),
                pitch.to_degrees(),
                roll.to_degrees(),
                ring_rot.to_degrees(),
            );
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
        assert!((tuning(51) - 700.0).abs() < 1e-4 && (tuning(53) - 700.0).abs() < 1e-4);
        assert!((tuning(309) - 0.53).abs() < 1e-4);
        assert!((tuning(298) - 0.3).abs() < 1e-6 && (tuning(322) - 0.3).abs() < 1e-6);
        assert!((tuning(300) - 5.0).abs() < 1e-6 && (tuning(301) - 0.02).abs() < 1e-6);
        assert_eq!(low_color().to_srgba().to_u8_array(), [255, 0, 0, 255]);
        assert_eq!(ap_damage_color().to_srgba().to_u8_array(), [255, 80, 0, 255]);
    }

    #[test]
    fn lock_sight_projection_and_ring_rotation() {
        let origin = Vec3::new(10.0, 5.0, -20.0);
        let aim = Vec3::new(0.0, 0.0, -2.0);
        let world = sight_world_pos(origin, aim, 700.0);
        assert!((world - Vec3::new(10.0, 5.0, -720.0)).length() < 1e-4);

        // Right turn in Bevy decreases yaw -> positive (clockwise) UI rotation.
        assert!(sight_ring_rotation(-0.5, 0.0) > 0.0);
        // Pitching up increases pitch -> negative (counter-clockwise) UI rotation.
        assert!(sight_ring_rotation(0.0, 0.5) < 0.0);

        let vp = Vec2::new(700.0, 380.0);
        assert_eq!(sight_root_origin(vp, Vec2::ZERO), vp);
        assert_eq!(
            sight_root_origin(vp, SCREEN * 0.5),
            Vec2::new(60.0, 20.0)
        );
    }

    #[test]
    fn locksight_rotation_subdialogs_exist() {
        let iso = acvd_formats::vfs::default_disc(std::path::Path::new("../.."));
        if !iso.exists() {
            return;
        }
        let disc = Disc::open(&iso).unwrap();
        let d = acvd_formats::drb::read(&disc.asset(LAYOUT).unwrap()).unwrap();
        assert_eq!(d.dialog("rocksight_out_rotation").unwrap().size, [402, 402]);
        assert_eq!(
            d.dialog("rocksight_out_rotationInv").unwrap().size,
            [402, 402]
        );
    }

    #[test]
    fn ap_effect_lag_bar_blinks_and_decays_over_120_ticks() {
        let mut fx = ApEffectState::default();
        // Initial sync at full AP 30000: no lag, alpha 0.
        fx.step(30000.0, 30000.0, 1.0 / 60.0);
        assert_eq!(fx.lag_ap, 30000.0);
        assert_eq!(fx.effect_alpha, 0.0);

        // Small chip damage (< 2% threshold = 600 AP): lag decays, but Gauge_APEffect stays hidden.
        fx.step(29700.0, 30000.0, 1.0 / 60.0);
        assert_eq!(fx.effect_alpha, 0.0);

        // Heavy hit (-6000 AP = 20% > 2% threshold): double gauge activates and blinks at 5 Hz.
        fx.step(24000.0, 30000.0, 0.1);
        assert!(fx.lag_ap < 30000.0 && fx.lag_ap > 24000.0);
        assert!(fx.effect_alpha > 0.5, "at t=0.1 s, sin(0.5*pi)=1 -> alpha=1.0, got {}", fx.effect_alpha);
        assert!(fx.damage_blend > 0.0);

        // After 2.5 s total decay (> 120 ticks = 2.0 s), lag catches up to cur_ap and blink turns off.
        for _ in 0..150 {
            fx.step(24000.0, 30000.0, 1.0 / 60.0);
        }
        assert!((fx.lag_ap - 24000.0).abs() < 1e-3);
        assert_eq!(fx.effect_alpha, 0.0);

        // Respawn snap-up: lag jumps immediately back to full AP.
        fx.step(30000.0, 30000.0, 1.0 / 60.0);
        assert_eq!(fx.lag_ap, 30000.0);
        assert_eq!(fx.effect_alpha, 0.0);
    }

    #[test]
    fn design_5001_ap() {
        let d = acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 5001)
            .unwrap();
        let s = Status::from_design(&d.data);
        assert_eq!(s.ap, 34695);
        assert!(s.en_max > 0.0 && s.en == s.en_max);
        assert_eq!(s.energy_fraction(), 1.0);
    }

    #[test]
    fn status_en_and_red_zone() {
        let mut s = Status {
            ap: 30000,
            ap_max: 30000,
            en: 58630.0,
            en_max: 58630.0,
            ow_phase: OwPhase::Disarmed,
            ow_charge: 0.0,
            ow_limit: 0.0,
        };
        let (digits, fill, low) = reading(Readout::Energy, Some(&s), None).unwrap();
        assert_eq!(digits, 10000);
        assert_eq!(fill, 1.0);
        assert!(!low);

        s.en = s.en_max * 0.2;
        let (digits, fill, low) = reading(Readout::Energy, Some(&s), None).unwrap();
        assert_eq!(digits, 2000);
        assert!((fill - 0.2).abs() < 1e-6);
        assert!(low, "20% EN is below en_red_zone");
    }

    #[test]
    fn shoulder_readout_shows_ammo_and_red_zone() {
        let d = acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 5001)
            .unwrap();
        let mut arms = Armament::from_design(&d.data);
        let (digits, fill, low) = reading(Readout::Shoulder, None, Some(&arms)).unwrap();
        assert_eq!(digits, 72);
        assert_eq!(fill, 1.0);
        assert!(!low);

        arms.shoulder.remaining = 10;
        let (digits, fill, low) = reading(Readout::Shoulder, None, Some(&arms)).unwrap();
        assert_eq!(digits, 10);
        assert!((fill - 10.0 / 72.0).abs() < 1e-5);
        assert!(low, "10/72 shoulder ammo is below ammo_red_zone");
    }
}
