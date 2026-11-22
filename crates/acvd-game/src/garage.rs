//! 3D ACVD Garage (`--garage` or `F1` from AC TEST):
//! - Loads any of the 13 retail 3D garage workshops from `param/customgarage.bin`
//!   (`PARAM_CUSTOMGARAGE_BIN`, `id` 1..=13, default `id = 5` → `menu1000` Underpass):
//!   `model/menu/menu{id:04}/menu{id:04}_m.bnd.dcx|menu{id:04}.flv` + `menu{id:04}.tpf.dcx`.
//! - Lights the bay with `model/map/ingamegarage/ingamegaragemenu{id:04}_env.msb` (360
//!   `0x82306860` / `0x82580080`) and `param/garagelighting.bin` (`PARAM_GARAGELIGHTING_BIN`:
//!   overhead spot light + up to 6 colored point lights).
//! - Frames the assembled AC with `param/acsisdisplay.bin` (`PARAM_ACSISDISPLAY_BIN`, 360
//!   `0x8257c988` / `0x8257cde8` / `0x8257ce50` / `0x82586400` / `0x82684958`) and
//!   `param/acsisdisplaycommon.bin` (`40.0°` FOV), smoothly sweeping to each part slot on hover
//!   while allowing free mouse-drag orbit and scroll zoom within `CustomGarageSt` bounds.
//! - Interactive Assemble (all 14 ACVD part slots from `param/acvparts.bin` + English names from
//!   `lang/en/text/partsname_en.fmg`), live `AcCtrlParam` spec telemetry with green/red preview
//!   deltas and Overweight / EN Shortage warnings, Paint (200+ `param/accolor/colorXXXX.bin`
//!   presets + live 7-channel customizer + `ca0001..ca0011` camouflage), Preset Designs, Garage
//!   Custom (all 13 workshops live), View Mode, and seamless AC TEST sortie transition with real
//!   `sound/acv2_se_menu.fev` cues and `acv2_bgm_menu_bgm_menu.fev` `grg` BGM.

use std::collections::BTreeSet;
use std::f32::consts::PI;

use acvd_data::generated::ac_unit::AcAssemblyDesignSt;
use acvd_data::generated::assembly::AC_PARTS;
use acvd_data::generated::ctrl::AcCtrlParam;
use acvd_data::generated::menu::{AcsisDisplaySt, PARAM_ACSISDISPLAY_BIN};
use acvd_data::generated::progression::{CustomGarageSt, PARAM_CUSTOMGARAGE_BIN};
use acvd_data::generated::render::{GarageLightingParamSt, PARAM_GARAGELIGHTING_BIN};
use acvd_data::{ac_part, find};
use acvd_formats::accolor::{
    AcColor, CH_AID, CH_AID2, CH_BASE, CH_JOINT, CH_KOJIMA, CH_OPTIONAL, GROUP_COUNT,
};
use acvd_formats::fmg::Fmg;
use acvd_formats::vfs::Disc;
use acvd_render::Packs;
use bevy::audio::AudioSource;
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use bevy::ui::px;

use crate::env;
use crate::paint;
use crate::sound::{self, Cues};

/// Default ACVD garage workshop row ID in `param/customgarage.bin` (`id: 5` = Underpass `menu1000`).
pub const DEFAULT_GARAGE_ID: u32 = 5;

/// `AcsisDisplaySt` preset IDs (`param/acsisdisplay.bin`, 360 `0x82684958` / `0x82684970`).
pub const DISPLAY_ACSIS_TOP: u32 = 10000;
pub const DISPLAY_PAINT_TOP: u32 = 10001;
pub const DISPLAY_COLORING_TOP: u32 = 10002;
pub const DISPLAY_DESIGN_DETAIL: u32 = 10004;

/// `sound/acv2_se_menu.fev` cues (`fevdump acv2_se_menu`).
pub const CUE_OK: &str = "menu_0000";
pub const CUE_CANCEL: &str = "menu_0001";
pub const CUE_ERROR: &str = "menu_0002";
pub const CUE_MOVE: &str = "menu_0003";
pub const CUE_GARAGE_IN: &str = "menu_0021";
pub const CUE_GARAGE_OUT: &str = "menu_0022";
pub const CUE_EQUIP: &str = "menu_0024";
pub const CUE_UNEQUIP: &str = "menu_0025";
pub const CUE_CATEGORY: &str = "menu_0030";
pub const CUE_VIEW_MODE: &str = "menu_0039";

/// Maximum rows shown at once in the scrollable left list panel.
const LIST_WINDOW: usize = 13;

/// Active top-level runtime mode (`Mode::Garage` vs `Mode::Sortie`).
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Garage,
    Sortie,
}

/// Queued mode switch executed cleanly by `main::process_mode_switch`.
#[derive(Resource, Default, Debug, Clone)]
pub struct ModeSwitch {
    pub target: Option<Mode>,
    pub sortie_map: Option<String>,
    pub sortie_test: Option<String>,
}

/// Marker for all 3D garage room geometry and garage-specific lights.
#[derive(Component)]
pub struct GarageWorld;

/// Marker for the garage UI root hierarchy.
#[derive(Component)]
pub struct GarageUi;

/// Interactive row button tagged with its logical row index in the active screen.
#[derive(Component, Clone, Copy)]
pub struct UiRow(pub usize);

/// Interactive action button in the paint editor / sortie screen.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub enum UiAction {
    Back,
    ToggleViewMode,
    PaintTab(PaintTab),
    PaintSwatch(usize),
    PaintChannel(usize),
    PaintRgbDelta(usize, i16),
    PaintCamoDelta(i8),
    SortieMapStep(i8),
    SortieTargetStep(i8),
    LaunchSortie,
}

/// Active garage screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GarageScreen {
    Top,
    AssembleSlots,
    AssembleParts { slot: AssembleSlot },
    Paint,
    Design,
    GarageCustom,
    SortieConfig,
    ViewMode,
}

impl GarageScreen {
    pub fn from_cli(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "top" => Some(Self::Top),
            "assemble" | "slots" => Some(Self::AssembleSlots),
            "head" => Some(Self::AssembleParts {
                slot: AssembleSlot::Head,
            }),
            "core" => Some(Self::AssembleParts {
                slot: AssembleSlot::Core,
            }),
            "arms" => Some(Self::AssembleParts {
                slot: AssembleSlot::Arms,
            }),
            "legs" => Some(Self::AssembleParts {
                slot: AssembleSlot::Legs,
            }),
            "weapon_r" | "arm_r" => Some(Self::AssembleParts {
                slot: AssembleSlot::ArmUnitR,
            }),
            "paint" => Some(Self::Paint),
            "design" => Some(Self::Design),
            "custom" | "workshops" => Some(Self::GarageCustom),
            "sortie" | "actest" => Some(Self::SortieConfig),
            "view" => Some(Self::ViewMode),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaintTab {
    Presets,
    Custom,
}

/// Top menu choices (order matches `private/lua360/script/garage/garagescene.lua`).
const TOP_ITEMS: &[(&str, &str)] = &[
    ("ASSEMBLE", "Customize AC frame, internals, and weapons"),
    ("PAINT", "Apply preset color schemes, RGB paint, and camo"),
    ("AC DESIGN", "Load any preset AC blueprint as a base"),
    ("GARAGE CUSTOM", "Switch between all 13 3D ACVD workshops"),
    ("AC TEST", "Configure targets & sortie into combat test"),
    ("VIEW MODE", "Hide HUD and inspect AC freely (V)"),
];

/// AC TEST target layer presets (`m4000_actest.msb`, `LAYER_PARAM_ST`).
pub const SORTIE_TARGETS: &[(&str, &str)] = &[
    ("actest2", "Light Targets (Tanks & Helis)"),
    ("actest3", "MT Squad (Light & Heavy MTs)"),
    ("actest4", "Test AC: Light Biped"),
    ("actest5", "Test AC: Midweight Biped"),
    ("actest6", "Test AC: Heavyweight"),
    ("actest7", "Test AC: Quadruped"),
    ("all", "All AC TEST Targets"),
    ("actest1", "Empty Range (Free Movement)"),
];

/// Sortie maps selectable from the Garage AC TEST launcher.
pub const SORTIE_MAPS: &[(&str, &str)] = &[
    ("m4000", "AC TEST Facility (m4000)"),
    ("m3200", "Contaminated Area (m3200)"),
    ("m0000", "Urban Sector (m0000)"),
    ("m1000", "Industrial Zone (m1000)"),
    ("m2000", "Dam Complex (m2000)"),
    ("m3000", "Alpine Basin (m3000)"),
];

/// Curated tactical color swatches for quick painting in Custom Paint mode.
const PAINT_SWATCHES: &[(&str, [u8; 4])] = &[
    ("MATTE BLACK", [34, 36, 40, 255]),
    ("GUNMETAL", [84, 90, 98, 255]),
    ("STEEL GREY", [148, 154, 162, 255]),
    ("IVORY WHITE", [228, 224, 214, 255]),
    ("OLIVE DRAB", [92, 102, 72, 255]),
    ("DESERT TAN", [176, 152, 114, 255]),
    ("NAVY BLUE", [46, 68, 104, 255]),
    ("CRIMSON RED", [176, 42, 38, 255]),
    ("RUST ORANGE", [204, 98, 36, 255]),
    ("AMBER GOLD", [214, 168, 52, 255]),
    ("TEAL CYAN", [44, 156, 178, 255]),
    ("EMERALD", [48, 138, 84, 255]),
];

const PAINT_CHANNEL_NAMES: &[&str] = &[
    "MAIN (BASE)",
    "SUB 1 (AID)",
    "SUB 2 (AID2)",
    "OPTIONAL",
    "JOINT",
    "GLOW / KOJIMA",
    "EYE / SENSOR",
];

/// All 14 authentic ACVD assembly slots (`garageselectscene.lua` & `AC_ASSEMBLY_DESIGN_ST_SLOTS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssembleSlot {
    Head,
    Core,
    Arms,
    Legs,
    Fcs,
    Generator,
    Booster,
    Recon,
    ArmUnitR,
    ArmUnitL,
    Shoulder,
    HangerR,
    HangerL,
    OveredWeapon,
}

impl AssembleSlot {
    pub const ALL: [Self; 14] = [
        Self::Head,
        Self::Core,
        Self::Arms,
        Self::Legs,
        Self::Fcs,
        Self::Generator,
        Self::Booster,
        Self::Recon,
        Self::ArmUnitR,
        Self::ArmUnitL,
        Self::Shoulder,
        Self::HangerR,
        Self::HangerL,
        Self::OveredWeapon,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Head => "HEAD",
            Self::Core => "CORE",
            Self::Arms => "ARMS",
            Self::Legs => "LEGS",
            Self::Fcs => "FCS",
            Self::Generator => "GENERATOR",
            Self::Booster => "BOOSTER",
            Self::Recon => "RECON UNIT",
            Self::ArmUnitR => "R ARM UNIT",
            Self::ArmUnitL => "L ARM UNIT",
            Self::Shoulder => "SHOULDER UNIT",
            Self::HangerR => "R BAY UNIT",
            Self::HangerL => "L BAY UNIT",
            Self::OveredWeapon => "OVERED WEAPON",
        }
    }

    pub fn group_header(self) -> Option<&'static str> {
        match self {
            Self::Head => Some("FRAME"),
            Self::Fcs => Some("INNER"),
            Self::ArmUnitR => Some("WEAPONS"),
            _ => None,
        }
    }

    /// `param/acvparts.bin` category index.
    pub fn category(self) -> u8 {
        match self {
            Self::Head => 0,
            Self::Core => 1,
            Self::Arms => 2,
            Self::Legs => 3,
            Self::Fcs => 4,
            Self::Generator => 5,
            Self::Booster => 6,
            Self::Recon => 13,
            Self::ArmUnitR | Self::ArmUnitL | Self::HangerR | Self::HangerL => 10,
            Self::Shoulder => 12,
            Self::OveredWeapon => 11,
        }
    }

    /// Model prefix in `AcPart::models` (empty for internal non-drawn slots).
    pub fn model_prefix(self) -> Option<&'static str> {
        match self {
            Self::Head => Some("hd"),
            Self::Core => Some("cr"),
            Self::Arms => Some("am"),
            Self::Legs => Some("lg"),
            Self::Booster => Some("bs"),
            Self::ArmUnitR | Self::HangerR => Some("hr"),
            Self::ArmUnitL | Self::HangerL => Some("hl"),
            Self::OveredWeapon => Some("ow"),
            Self::Fcs | Self::Generator | Self::Recon | Self::Shoulder => None,
        }
    }

    /// `param/acsisdisplay.bin` focus camera preset ID (`PARAM_ACSISDISPLAY_BIN`).
    pub fn display_id(self) -> u32 {
        match self {
            Self::Head => 0,
            Self::Core => 1,
            Self::Arms => 2,
            Self::Legs => 3,
            Self::Fcs => 4,
            Self::Generator => 5,
            Self::Booster => 6,
            Self::Recon => 9,
            Self::ArmUnitR => 10,
            Self::ArmUnitL => 11,
            Self::OveredWeapon => 12,
            Self::Shoulder => 14,
            Self::HangerR => 15,
            Self::HangerL => 16,
        }
    }

    /// True when the slot may be cleared to `0` (`(EMPTY)`).
    pub fn allows_empty(self) -> bool {
        matches!(
            self,
            Self::ArmUnitR
                | Self::ArmUnitL
                | Self::Shoulder
                | Self::HangerR
                | Self::HangerL
                | Self::OveredWeapon
        )
    }

    /// True when equipping Weapon Arms (`arms.is_weapon_arm != 0`) disables this slot
    /// (360 `0x824793c8` / `0x82457400` / `0x8288e218`, matching `assemble::assemble`).
    pub fn blocked_by_weapon_arm(self) -> bool {
        matches!(
            self,
            Self::ArmUnitR | Self::ArmUnitL | Self::Shoulder | Self::HangerR | Self::HangerL
        )
    }

    pub fn get(self, d: &AcAssemblyDesignSt) -> u16 {
        let v = match self {
            Self::Head => d.head,
            Self::Core => d.core,
            Self::Arms => d.arms,
            Self::Legs => d.legs,
            Self::Fcs => d.fcs,
            Self::Generator => d.gene,
            Self::Booster => d.booster,
            Self::Recon => d.ob,
            Self::ArmUnitR => d.armwep_r,
            Self::ArmUnitL => d.armwep_l,
            Self::Shoulder => d.shoulder,
            Self::HangerR => d.hanger_r,
            Self::HangerL => d.hanger_l,
            Self::OveredWeapon => d.back_r,
        };
        v.max(0) as u16
    }

    pub fn set(self, d: &mut AcAssemblyDesignSt, id: u16) {
        let v = id as i16;
        match self {
            Self::Head => d.head = v,
            Self::Core => d.core = v,
            Self::Arms => d.arms = v,
            Self::Legs => d.legs = v,
            Self::Fcs => d.fcs = v,
            Self::Generator => d.gene = v,
            Self::Booster => d.booster = v,
            Self::Recon => d.ob = v,
            Self::ArmUnitR => d.armwep_r = v,
            Self::ArmUnitL => d.armwep_l = v,
            Self::Shoulder => d.shoulder = v,
            Self::HangerR => d.hanger_r = v,
            Self::HangerL => d.hanger_l = v,
            Self::OveredWeapon => d.back_r = v,
        }
    }
}

/// True when `design`'s equipped arms are Weapon Arms (`acvparts` cat-2 `+0x13c == 1`).
pub fn has_weapon_arms(design: &AcAssemblyDesignSt) -> bool {
    acvd_data::part_field(design.arms as i64, 2, "is_weapon_arm") != 0.0
}

/// Returns all selectable part IDs (`0` = empty when allowed) for `slot`.
pub fn parts_for_slot(slot: AssembleSlot) -> Vec<u16> {
    let mut out = Vec::new();
    if slot.allows_empty() {
        out.push(0);
    }
    let cat = slot.category();
    let prefix = slot.model_prefix();
    let mut seen = BTreeSet::new();
    for p in AC_PARTS {
        if p.category != cat || p.id == 0 {
            continue;
        }
        // Skip parts that cannot be drawn when the slot expects a 3D model.
        if let Some(pref) = prefix {
            if p.model(pref).is_none() {
                continue;
            }
        }
        // Skip ready-position weapons on hanger bays (bay weapons cannot be ready-position
        // cannons — 360 bay-shift rules, see `bay.rs`).
        if matches!(slot, AssembleSlot::HangerR | AssembleSlot::HangerL)
            && p.field("ready_position").unwrap_or(0.0) != 0.0
        {
            continue;
        }
        seen.insert(p.id);
    }
    out.extend(seen);
    out
}

/// Looks up the English name of a part in `lang/en/text/partsname_en.fmg`, falling back to its ID.
///
/// `partsname_en.fmg` keys every category at `category * 10_000 + part_id` (category 0 Head at
/// `0..`, category 1 Core at `10000..`, category 2 Arms at `20000..`, ..., category 10 Weapons at
/// `100000..`, category 11 Overed Weapons at `110000..`, category 12 Shoulders at `120000..`,
/// category 13 Recon at `130000..`).
pub fn part_name(parts_fmg: Option<&Fmg>, category: u8, id: u16) -> String {
    if id == 0 {
        return "(EMPTY)".to_string();
    }
    if let Some(fmg) = parts_fmg {
        let fmg_id = i32::from(category) * 10_000 + i32::from(id);
        if let Some(name) = fmg.get(fmg_id) {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    let model = ac_part(category, id).map(|p| p.model_id).unwrap_or(0);
    format!("PART #{id} (m{model:04})")
}

/// Formats an `AC_ASSEMBLY_DESIGN_ST` name as clean ASCII for Bevy's built-in font (avoiding
/// tofu boxes on Japanese dev design labels in `param/acassemblydesign.bin`).
pub fn display_design_name(id: u32, raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_ascii_graphic() { c } else { ' ' })
        .collect();
    let trimmed = cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let without_id = trimmed
        .strip_prefix(&format!("{id}_"))
        .or_else(|| trimmed.strip_prefix(&format!("{id} ")))
        .or_else(|| trimmed.strip_prefix(&format!("{id}")))
        .unwrap_or(&trimmed)
        .trim_matches(|c: char| c == '_' || c == '-' || c.is_whitespace());
    if without_id.chars().any(|c| c.is_ascii_alphanumeric()) {
        format!("{id} {without_id}")
    } else {
        format!("UNIT #{id}")
    }
}

/// One of the 13 retail 3D garage workshops from `param/customgarage.bin`.
#[derive(Debug, Clone)]
pub struct GarageWorkshop {
    pub id: u32,
    pub model_id: u16,
    pub title: String,
    pub theme: &'static str,
    pub row: CustomGarageSt,
}

/// Human-readable English theme labels matching the Japanese names in `param/customgarage.bin`.
fn workshop_theme(model_id: u16) -> &'static str {
    match model_id {
        310 => "Outdoor Yard",
        300 => "Warehouse",
        330 => "Carrier Deck",
        340 => "Vertical Shaft",
        1000 => "Underpass (Default)",
        1020 => "Desert Base",
        1010 => "Sci-Fi Facility",
        1030 => "Modeler's Room",
        1040 => "Corporate Bay",
        1050 => "WWII Hangar",
        1060 => "8-Bit Retro",
        1070 => "AC Museum I",
        1080 => "AC Museum II",
        _ => "Workshop",
    }
}

/// Lists all retail 3D workshops whose FLVER model exists in `acvd_data`.
pub fn load_workshops(menu_fmg: Option<&Fmg>) -> Vec<GarageWorkshop> {
    let mut out = Vec::new();
    for r in PARAM_CUSTOMGARAGE_BIN {
        if r.id == 0 || r.id > 13 {
            continue;
        }
        let Ok(model_id) = u16::try_from(r.data.model_id) else {
            continue;
        };
        let path = garage_model_path(model_id);
        if acvd_data::model(&path).is_none() {
            continue;
        }
        let title = menu_fmg
            .and_then(|f| f.get(r.data.name_id))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("Workshop {}", r.id));
        out.push(GarageWorkshop {
            id: r.id,
            model_id,
            title,
            theme: workshop_theme(model_id),
            row: r.data,
        });
    }
    out
}

pub fn garage_model_path(model_id: u16) -> String {
    format!("model/menu/menu{model_id:04}/menu{model_id:04}_m.bnd.dcx|menu{model_id:04}.flv")
}

pub fn garage_tpf_path(model_id: u16) -> String {
    format!("model/menu/menu{model_id:04}/menu{model_id:04}.tpf.dcx")
}

pub fn garage_env_path(disc: &Disc, model_id: u16) -> String {
    let specific = format!("model/map/ingamegarage/ingamegaragemenu{model_id:04}_env.msb");
    if disc.exists(&specific) {
        specific
    } else {
        "model/map/ingamegarage/ingamegarage_env.msb".to_string()
    }
}

/// Enumerates all `param/accolor/colorXXXX.bin` preset IDs on the disc.
pub fn discover_color_presets(disc: &Disc) -> Vec<u16> {
    let mut ids = BTreeSet::new();
    for path in disc.list("param/accolor") {
        let low = path.to_ascii_lowercase();
        if let Some(rest) = low.strip_prefix("param/accolor/color") {
            if let Some(num) = rest.strip_suffix(".bin") {
                if let Ok(id) = num.parse::<u16>() {
                    if id > 0 {
                        ids.insert(id);
                    }
                }
            }
        }
    }
    ids.into_iter().collect()
}

/// Live computed AC specifications (`AcCtrlParamCalc.lua` + `param/acvparts.bin`).
#[derive(Debug, Clone, Copy)]
pub struct AcSpecs {
    pub ap: f32,
    pub ke_def: f32,
    pub ce_def: f32,
    pub te_def: f32,
    pub total_weight: f32,
    /// Non-leg build weight carried by the legs (`AcCtrlParam::leg_capability`, `AcCtrlParamCalc.lua` line 867).
    pub carried_load: f32,
    /// Maximum leg payload (`param/acvparts.bin` cat-3 `+0x142`, `AcCtrlParamCalc.lua` line 871).
    pub leg_payload: f32,
    pub overweight: bool,
    pub en_output: f32,
    pub en_drain: f32,
    pub en_recovery: f32,
    pub en_shortage: bool,
    pub en_capacity: f32,
    pub boost_speed: f32,
    pub hb_speed: f32,
    pub glide_speed: f32,
    pub turn_speed: f32,
    pub lock_range: f32,
    pub scan_secs: f32,
    pub stagger_thr: f32,
    pub buildable: bool,
}

impl AcSpecs {
    pub fn calculate(design: &AcAssemblyDesignSt) -> Self {
        let ap = [(design.head, 0u8), (design.core, 1), (design.arms, 2), (design.legs, 3)]
            .into_iter()
            .map(|(id, cat)| acvd_data::part_field(id as i64, cat, "ap"))
            .sum::<f32>();
        let buildable = AcCtrlParam::buildable(design);
        if !buildable {
            return Self {
                ap,
                ke_def: 0.0,
                ce_def: 0.0,
                te_def: 0.0,
                total_weight: 0.0,
                carried_load: 0.0,
                leg_payload: 0.0,
                overweight: false,
                en_output: 0.0,
                en_drain: 0.0,
                en_recovery: 0.0,
                en_shortage: false,
                en_capacity: 0.0,
                boost_speed: 0.0,
                hb_speed: 0.0,
                glide_speed: 0.0,
                turn_speed: 0.0,
                lock_range: 0.0,
                scan_secs: 0.0,
                stagger_thr: 0.0,
                buildable: false,
            };
        }
        let c = AcCtrlParam::calculate(design);
        let en_recovery = c.en_output - c.passive_en_drain;
        let leg_payload = acvd_data::part_field(design.legs as i64, 3, "payload");
        Self {
            ap,
            ke_def: c.ke_def_thr,
            ce_def: c.ce_def_thr,
            te_def: c.te_def_thr,
            total_weight: c.total_weight,
            carried_load: c.leg_capability,
            leg_payload,
            overweight: c.weight_ratio > 1.0,
            en_output: c.en_output,
            en_drain: c.passive_en_drain,
            en_recovery,
            en_shortage: en_recovery <= 0.0,
            en_capacity: c.en_capacity,
            boost_speed: c.boost_max_speed,
            hb_speed: c.quick_boost_max_speed,
            glide_speed: c.glide_max_speed,
            turn_speed: c.turn_speed,
            lock_range: c.first_lock_range,
            scan_secs: c.scan_complete_secs,
            stagger_thr: c.stagger_penalty_threshold,
            buildable: true,
        }
    }
}

/// Target pose for the garage camera (`PARAM_ACSISDISPLAY_BIN` + `PARAM_CUSTOMGARAGE_BIN`).
#[derive(Debug, Clone, Copy)]
pub struct GarageCamTarget {
    pub focus: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub dist: f32,
    pub fov_deg: f32,
}

/// Computes the Bevy camera target from an `AcsisDisplaySt` preset (`param/acsisdisplay.bin`,
/// 360 `0x82586400`), AC bounding box, and active `CustomGarageSt` bounds.
///
/// The AC stands at `(0, 0, 0)` rotated by `PI` so it faces `+Z` (away from the workshop's `-Z`
/// backdrop/emblem wall toward the open bay). `bevy_yaw = -eye_angle_y` puts `eye_angle_y = 0°`
/// at `+Z` looking at the AC's front with the workshop backdrop behind it, `+69°` (`ArmUnitR`) on
/// the AC's right (`-X`), `-59°` (`ArmUnitL`) on the AC's left (`+X`), and `±170°` (`Booster` /
/// `OveredUnit`) behind the AC (`-Z`).
pub fn acsis_camera_pose(
    preset_id: u32,
    bounds: (Vec3, Vec3),
    workshop: &CustomGarageSt,
    view_mode: bool,
) -> GarageCamTarget {
    let row: &AcsisDisplaySt = find(PARAM_ACSISDISPLAY_BIN, preset_id)
        .or_else(|| find(PARAM_ACSISDISPLAY_BIN, DISPLAY_ACSIS_TOP))
        .map(|r| &r.data)
        .expect("PARAM_ACSISDISPLAY_BIN has AcsisTop");

    let ac_height = (bounds.1.y - bounds.0.y).clamp(4.5, 9.5);
    let is_full_view = preset_id >= DISPLAY_ACSIS_TOP || view_mode;
    let base_y = if is_full_view {
        // Aim just below torso mid so feet sit cleanly on the turntable and bay weapons fit above.
        bounds.0.y + ac_height * 0.44
    } else {
        match preset_id {
            0 => bounds.0.y + ac_height * 0.78, // Head
            1 | 4 | 5 | 9 | 14 => bounds.0.y + ac_height * 0.58, // Core / Inner / Shoulder
            2 | 10 | 11 => bounds.0.y + ac_height * 0.50, // Arms / Hand weapons
            3 => bounds.0.y + ac_height * 0.24, // Legs
            6 | 12 | 15 | 16 => bounds.0.y + ac_height * 0.58, // Booster / OW / Hangers
            _ => bounds.0.y + ac_height * 0.46,
        }
    };

    let yaw = -row.eye_angle_y.to_radians();
    // Positive `eye_angle_x` looks downward from above; clamp gently so close-ups frame the
    // part at a flattering 3/4 workshop angle instead of straight top-down.
    let pitch = if is_full_view {
        (row.eye_angle_x.to_radians() * 0.45 + 0.08).clamp(-0.18, 0.30)
    } else {
        (row.eye_angle_x.to_radians() * 0.48 + 0.08).clamp(-0.22, 0.48)
    };
    let max_dist = (workshop.camera_dist_max as f32).max(16.0);
    let dist = if is_full_view {
        (row.eye_dist * 1.48).max(ac_height * 1.85).clamp(11.5, max_dist)
    } else {
        (row.eye_dist * 1.15).clamp(5.2, max_dist)
    };

    // Keep the AC centered in the viewport between the left menu and right specs card.
    let mut focus = Vec3::new(row.at_offset_x * 0.35, base_y, -row.at_offset_z * 0.25);
    if !view_mode {
        let cam_right = Vec3::new(yaw.cos(), 0.0, -yaw.sin());
        focus -= cam_right * 0.20;
    }

    let fov_deg = if workshop.camera_fov > 10 {
        workshop.camera_fov as f32
    } else {
        40.0
    };
    GarageCamTarget {
        focus,
        yaw,
        pitch,
        dist,
        fov_deg,
    }
}

/// Persistent Garage controller state.
#[derive(Resource)]
pub struct GarageState {
    pub screen: GarageScreen,
    pub cursor: usize,
    pub scroll: usize,
    /// Saved slot cursor when diving into `AssembleParts`.
    pub slot_cursor: usize,
    /// Design snapshot before previewing parts/designs, restored on Cancel.
    pub saved_design: Option<AcAssemblyDesignSt>,
    pub saved_color: Option<Option<AcColor>>,
    pub saved_name: Option<String>,
    /// Loaded retail workshops (`PARAM_CUSTOMGARAGE_BIN` 1..=13).
    pub workshops: Vec<GarageWorkshop>,
    pub workshop_idx: usize,
    /// Model ID currently spawned in `GarageWorld` (`None` when not spawned).
    pub spawned_workshop: Option<u16>,
    /// Available `param/accolor/colorXXXX.bin` IDs.
    pub color_presets: Vec<u16>,
    pub paint_tab: PaintTab,
    pub paint_channel: usize,
    /// Sortie launcher indices.
    pub sortie_map_idx: usize,
    pub sortie_target_idx: usize,
    /// Localized text bank (`lang/en/text/partsname_en.fmg`).
    pub parts_fmg: Option<Fmg>,
    /// Active camera interpolation state.
    pub cam_yaw: f32,
    pub cam_pitch: f32,
    pub cam_dist: f32,
    pub cam_focus: Vec3,
    pub cam_initialized: bool,
    pub cam_manual_orbit: bool,
    /// Transient status banner shown inside the UI (e.g. "OVERWEIGHT", "EQUIPPED ...").
    pub banner: Option<(String, Color, f32)>,
    /// True when the UI tree needs rebuilding.
    pub ui_dirty: bool,
}

impl GarageState {
    pub fn init(
        disc: &Disc,
        initial_garage_id: Option<u32>,
        initial_screen: GarageScreen,
        initial_map: &str,
        initial_test: &str,
    ) -> Self {
        let menu_fmg = disc
            .asset("lang/en/text/menu/menu.fmg")
            .ok()
            .and_then(|b| acvd_formats::fmg::read(&b).ok());
        let parts_fmg = disc
            .asset("lang/en/text/partsname_en.fmg")
            .ok()
            .and_then(|b| acvd_formats::fmg::read(&b).ok());
        let workshops = load_workshops(menu_fmg.as_ref());
        let want = initial_garage_id.unwrap_or(DEFAULT_GARAGE_ID);
        let workshop_idx = workshops
            .iter()
            .position(|w| w.id == want || u32::from(w.model_id) == want)
            .or_else(|| workshops.iter().position(|w| w.id == DEFAULT_GARAGE_ID))
            .unwrap_or(0);
        let color_presets = discover_color_presets(disc);
        let sortie_map_idx = SORTIE_MAPS
            .iter()
            .position(|(id, _)| *id == initial_map)
            .unwrap_or(0);
        let sortie_target_idx = SORTIE_TARGETS
            .iter()
            .position(|(id, _)| *id == initial_test)
            .unwrap_or(0);
        Self {
            screen: initial_screen,
            cursor: 0,
            scroll: 0,
            slot_cursor: 0,
            saved_design: None,
            saved_color: None,
            saved_name: None,
            workshops,
            workshop_idx,
            spawned_workshop: None,
            color_presets,
            paint_tab: PaintTab::Presets,
            paint_channel: CH_BASE,
            sortie_map_idx,
            sortie_target_idx,
            parts_fmg,
            cam_yaw: -19.0f32.to_radians(),
            cam_pitch: 0.14,
            cam_dist: 9.8,
            cam_focus: Vec3::new(0.0, 3.2, 0.0),
            cam_initialized: false,
            cam_manual_orbit: false,
            banner: None,
            ui_dirty: true,
        }
    }

    pub fn active_workshop(&self) -> Option<&GarageWorkshop> {
        self.workshops.get(self.workshop_idx)
    }

    pub fn display_preset_id(&self) -> u32 {
        match self.screen {
            GarageScreen::Top | GarageScreen::GarageCustom | GarageScreen::SortieConfig => {
                DISPLAY_ACSIS_TOP
            }
            GarageScreen::ViewMode => DISPLAY_ACSIS_TOP,
            GarageScreen::AssembleSlots => AssembleSlot::ALL
                .get(self.cursor)
                .map(|s| s.display_id())
                .unwrap_or(DISPLAY_ACSIS_TOP),
            GarageScreen::AssembleParts { slot } => slot.display_id(),
            GarageScreen::Paint => match self.paint_tab {
                PaintTab::Presets => DISPLAY_PAINT_TOP,
                PaintTab::Custom => DISPLAY_COLORING_TOP,
            },
            GarageScreen::Design => DISPLAY_DESIGN_DETAIL,
        }
    }

    pub fn set_banner(&mut self, msg: impl Into<String>, color: Color) {
        self.banner = Some((msg.into(), color, 3.0));
        self.ui_dirty = true;
    }
}

/// Ensures the active 3D garage workshop model, env lighting, and `PARAM_GARAGELIGHTING_BIN`
/// light rig are spawned whenever `Mode::Garage` is active or the workshop changes.
#[allow(clippy::too_many_arguments)]
pub fn sync_room(
    mut commands: Commands,
    mode: Res<Mode>,
    mut state: ResMut<GarageState>,
    garage: Res<crate::Garage>,
    world: Query<Entity, With<GarageWorld>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut dir_lights: Query<(&mut DirectionalLight, &mut Transform)>,
    cameras: Query<Entity, With<Camera3d>>,
) {
    if *mode != Mode::Garage {
        if state.spawned_workshop.is_some() {
            for e in &world {
                commands.entity(e).despawn();
            }
            state.spawned_workshop = None;
        }
        return;
    }

    let Some(ws) = state.active_workshop().cloned() else {
        return;
    };
    if state.spawned_workshop == Some(ws.model_id) {
        return;
    }

    for e in &world {
        commands.entity(e).despawn();
    }
    state.spawned_workshop = Some(ws.model_id);

    // 1. Apply the workshop's real 360 env MSB (`ingamegaragemenuXXXX_env.msb` or fallback).
    let env_file = garage_env_path(&garage.disc, ws.model_id);
    env::apply_env_file(&mut commands, &garage.disc, &env_file, &mut dir_lights, &cameras);

    // 2. Spawn the real 3D garage FLVER (`menuXXXX.flv`) with per-garage TPF texture priority.
    let flv_path = garage_model_path(ws.model_id);
    let primary_tpf = garage_tpf_path(ws.model_id);
    let fallback_tpf = "model/map/ingamegarage/ingamegarage.tpf.dcx";
    let mut packs = Packs::default();
    let mut drawn = 0usize;
    let mut missing = 0usize;

    match acvd_render::model(&garage.disc, &flv_path) {
        Ok(loaded_meshes) => {
            let root = commands
                .spawn((
                    GarageWorld,
                    Name::new(format!("menu{:04}", ws.model_id)),
                    Transform::IDENTITY,
                    Visibility::default(),
                ))
                .id();

            for part in loaded_meshes {
                let mtd_low = part.mtd.to_ascii_lowercase();
                let diff_low = part
                    .diffuse
                    .as_deref()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                let is_sky = mtd_low.contains("sky")
                    || diff_low.contains("sky")
                    || diff_low.contains("cloud")
                    || diff_low.contains("kumo");
                let is_cutout = diff_low.contains("wire")
                    || diff_low.contains("net")
                    || diff_low.contains("fence")
                    || diff_low.contains("decal")
                    || diff_low.contains("sign")
                    || diff_low.contains("smoke")
                    || diff_low.contains("light")
                    || mtd_low.contains("alpha")
                    || mtd_low.contains("decal");
                let is_glass = diff_low.contains("glass") || mtd_low.contains("glass");

                let diffuse = part.diffuse.as_deref().filter(|_| !garage.flat).and_then(|name| {
                    load_garage_tex(&garage.disc, &mut packs, &primary_tpf, fallback_tpf, name, false)
                        .map(|img| images.add(img))
                        .or_else(|| {
                            missing += 1;
                            None
                        })
                });
                let normal = part.bump.as_deref().filter(|_| !garage.flat && !is_sky).and_then(|name| {
                    load_garage_tex(&garage.disc, &mut packs, &primary_tpf, fallback_tpf, name, true)
                        .map(|img| images.add(img))
                });

                let alpha_mode = if is_glass {
                    AlphaMode::Blend
                } else if is_cutout {
                    AlphaMode::Mask(0.45)
                } else {
                    AlphaMode::Opaque
                };

                let mat = StandardMaterial {
                    base_color: if is_glass {
                        Color::srgba(0.85, 0.92, 1.0, 0.35)
                    } else if diffuse.is_some() {
                        Color::WHITE
                    } else {
                        Color::srgb(0.32, 0.34, 0.38)
                    },
                    base_color_texture: diffuse,
                    normal_map_texture: normal,
                    flip_normal_map_y: false,
                    perceptual_roughness: if is_glass { 0.15 } else { 0.78 },
                    metallic: 0.12,
                    unlit: is_sky,
                    cull_mode: if is_sky || is_cutout { None } else { Some(bevy::render::render_resource::Face::Back) },
                    alpha_mode,
                    ..default()
                };
                commands.spawn((
                    Mesh3d(meshes.add(part.mesh)),
                    MeshMaterial3d(materials.add(mat)),
                    Transform::IDENTITY,
                    ChildOf(root),
                ));
                drawn += 1;
            }
            info!(
                "garage menu{:04} ({}): {} submeshes drawn ({} textures missing)",
                ws.model_id, ws.theme, drawn, missing
            );
        }
        Err(e) => warn!("garage {flv_path}: {e:#}"),
    }

    // 3. Spawn `param/garagelighting.bin` spotlight + 6 colored point lights.
    let lighting: &GarageLightingParamSt = find(PARAM_GARAGELIGHTING_BIN, u32::from(ws.model_id))
        .or_else(|| find(PARAM_GARAGELIGHTING_BIN, 0))
        .map(|r| &r.data)
        .expect("PARAM_GARAGELIGHTING_BIN has default row 0");

    let spot_pos = Vec3::new(
        -lighting.spot_light_pos_x,
        lighting.spot_light_pos_y,
        lighting.spot_light_pos_z,
    );
    let spot_at = Vec3::new(
        -lighting.spot_light_look_at_x,
        lighting.spot_light_look_at_y.max(2.5),
        lighting.spot_light_look_at_z,
    );
    if spot_pos.length_squared() > 0.25 {
        commands.spawn((
            GarageWorld,
            SpotLight {
                color: Color::srgb(1.0, 0.96, 0.90),
                intensity: 18_000_000.0,
                range: 85.0,
                outer_angle: 0.75,
                inner_angle: 0.35,
                shadow_maps_enabled: true,
                ..default()
            },
            Transform::from_translation(spot_pos).looking_at(spot_at, Vec3::Y),
        ));
    }

    let point_lights = [
        (
            lighting.point_light0_pos_x,
            lighting.point_light0_pos_y,
            lighting.point_light0_pos_z,
            lighting.point_light0_renge,
            lighting.point_light0_color_r,
            lighting.point_light0_color_g,
            lighting.point_light0_color_b,
            lighting.point_light0_color_a,
        ),
        (
            lighting.point_light1_pos_x,
            lighting.point_light1_pos_y,
            lighting.point_light1_pos_z,
            lighting.point_light1_renge,
            lighting.point_light1_color_r,
            lighting.point_light1_color_g,
            lighting.point_light1_color_b,
            lighting.point_light1_color_a,
        ),
        (
            lighting.point_light2_pos_x,
            lighting.point_light2_pos_y,
            lighting.point_light2_pos_z,
            lighting.point_light2_renge,
            lighting.point_light2_color_r,
            lighting.point_light2_color_g,
            lighting.point_light2_color_b,
            lighting.point_light2_color_a,
        ),
        (
            lighting.point_light3_pos_x,
            lighting.point_light3_pos_y,
            lighting.point_light3_pos_z,
            lighting.point_light3_renge,
            lighting.point_light3_color_r,
            lighting.point_light3_color_g,
            lighting.point_light3_color_b,
            lighting.point_light3_color_a,
        ),
        (
            lighting.point_light4_pos_x,
            lighting.point_light4_pos_y,
            lighting.point_light4_pos_z,
            lighting.point_light4_renge,
            lighting.point_light4_color_r,
            lighting.point_light4_color_g,
            lighting.point_light4_color_b,
            lighting.point_light4_color_a,
        ),
        (
            lighting.point_light5_pos_x,
            lighting.point_light5_pos_y,
            lighting.point_light5_pos_z,
            lighting.point_light5_renge,
            lighting.point_light5_color_r,
            lighting.point_light5_color_g,
            lighting.point_light5_color_b,
            lighting.point_light5_color_a,
        ),
    ];
    for (x, y, z, range, r, g, b, a) in point_lights {
        if range <= 0.1 || a <= 0.005 {
            continue;
        }
        let peak = r.max(g).max(b).max(1e-4);
        let intensity = (peak * a * range * range * 4800.0).clamp(15_000.0, 24_000_000.0);
        commands.spawn((
            GarageWorld,
            PointLight {
                color: Color::srgb((r / peak).clamp(0.0, 1.0), (g / peak).clamp(0.0, 1.0), (b / peak).clamp(0.0, 1.0)),
                intensity,
                range: range.max(6.0) * 1.4,
                shadow_maps_enabled: false,
                ..default()
            },
            Transform::from_xyz(-x, y, z),
        ));
    }

    // Subtle rim/key fill right near the AC bay so dark armor plates always read crisply.
    commands.spawn((
        GarageWorld,
        PointLight {
            color: Color::srgb(0.86, 0.92, 1.0),
            intensity: 480_000.0,
            range: 24.0,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_xyz(3.5, 5.5, -6.0),
    ));
}

fn load_garage_tex(
    disc: &Disc,
    packs: &mut Packs,
    primary_tpf: &str,
    fallback_tpf: &str,
    name: &str,
    linear: bool,
) -> Option<Image> {
    for pack in [primary_tpf, fallback_tpf] {
        if let Ok(Some(idx)) = packs.find(disc, pack, name) {
            let img = if linear {
                packs.linear_texture_at(disc, pack, idx, name).ok()
            } else {
                packs.texture_at(disc, pack, idx, name).ok()
            };
            if img.is_some() {
                return img;
            }
        }
    }
    if linear {
        packs.linear_image(disc, name).ok()
    } else {
        packs.image(disc, name).ok()
    }
}

/// Drives the smooth `PARAM_ACSISDISPLAY_BIN` garage camera + free mouse orbit/zoom.
#[allow(clippy::too_many_arguments)]
pub fn update_camera(
    mode: Res<Mode>,
    time: Res<Time>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    garage: Res<crate::Garage>,
    mut state: ResMut<GarageState>,
    mut cams: Query<(&mut Transform, &mut Projection), With<Camera3d>>,
) {
    if *mode != Mode::Garage {
        state.cam_initialized = false;
        return;
    }
    let Ok((mut xf, mut proj)) = cams.single_mut() else {
        return;
    };
    let Some(ws_row) = state.active_workshop().map(|w| w.row) else {
        return;
    };

    let target = acsis_camera_pose(
        state.display_preset_id(),
        garage.bounds,
        &ws_row,
        state.screen == GarageScreen::ViewMode,
    );

    if let Projection::Perspective(ref mut p) = *proj {
        p.fov = target.fov_deg.to_radians();
    }

    let allow_left_drag = state.screen == GarageScreen::ViewMode;
    let dragging = buttons.pressed(MouseButton::Right)
        || (allow_left_drag && buttons.pressed(MouseButton::Left));
    if dragging && motion.delta.length_squared() > 0.0 {
        state.cam_manual_orbit = true;
        state.cam_yaw -= motion.delta.x * 0.006;
        state.cam_pitch = (state.cam_pitch + motion.delta.y * 0.005).clamp(-0.35, 1.15);
    }
    if scroll.delta.y.abs() > 0.0 {
        state.cam_manual_orbit = true;
        let max_d = (ws_row.camera_dist_max as f32).max(18.0);
        state.cam_dist = (state.cam_dist * (1.0 - scroll.delta.y * 0.09)).clamp(3.2, max_d);
    }

    if !state.cam_initialized {
        state.cam_yaw = target.yaw;
        state.cam_pitch = target.pitch;
        state.cam_dist = target.dist;
        state.cam_focus = target.focus;
        state.cam_initialized = true;
    } else if !state.cam_manual_orbit {
        let t = 1.0 - (-8.0 * time.delta_secs()).exp();
        let dyaw = shortest_angle(state.cam_yaw, target.yaw);
        state.cam_yaw += dyaw * t;
        state.cam_pitch += (target.pitch - state.cam_pitch) * t;
        state.cam_dist += (target.dist - state.cam_dist) * t;
        state.cam_focus = state.cam_focus.lerp(target.focus, t);
    } else {
        // Still smoothly align focus Y if the AC height changed.
        let t = 1.0 - (-6.0 * time.delta_secs()).exp();
        state.cam_focus = state.cam_focus.lerp(target.focus, t);
    }

    let min_y = (ws_row.camera_min_height as f32 * 0.01).clamp(0.8, 4.5);
    let offset = Vec3::new(
        state.cam_yaw.sin() * state.cam_pitch.cos(),
        state.cam_pitch.sin(),
        state.cam_yaw.cos() * state.cam_pitch.cos(),
    ) * state.cam_dist;
    let mut eye = state.cam_focus + offset;
    if eye.y < min_y {
        eye.y = min_y;
    }
    *xf = Transform::from_translation(eye).looking_at(state.cam_focus, Vec3::Y);
}

fn shortest_angle(from: f32, to: f32) -> f32 {
    let mut d = (to - from) % (2.0 * PI);
    if d > PI {
        d -= 2.0 * PI;
    } else if d < -PI {
        d += 2.0 * PI;
    }
    d
}

/// Handles keyboard, gamepad, and mouse-button interaction in the 3D Garage.
#[allow(clippy::too_many_arguments)]
pub fn handle_input(
    mut commands: Commands,
    mode: Res<Mode>,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    gamepads: Query<&Gamepad>,
    mut state: ResMut<GarageState>,
    mut garage: ResMut<crate::Garage>,
    mut cues: ResMut<Cues>,
    mut audio_assets: ResMut<Assets<AudioSource>>,
    mut mode_switch: ResMut<ModeSwitch>,
    row_buttons: Query<(&Interaction, &UiRow), Changed<Interaction>>,
    action_buttons: Query<(&Interaction, &UiAction), Changed<Interaction>>,
) {
    if *mode != Mode::Garage {
        return;
    }

    if let Some((_, _, ref mut ttl)) = state.banner {
        *ttl -= time.delta_secs();
        if *ttl <= 0.0 {
            state.banner = None;
            state.ui_dirty = true;
        }
    }

    let mut gp_up = false;
    let mut gp_down = false;
    let mut gp_left = false;
    let mut gp_right = false;
    let mut gp_confirm = false;
    let mut gp_cancel = false;
    let mut gp_view = false;
    for gp in &gamepads {
        gp_up |= gp.just_pressed(GamepadButton::DPadUp);
        gp_down |= gp.just_pressed(GamepadButton::DPadDown);
        gp_left |= gp.just_pressed(GamepadButton::DPadLeft);
        gp_right |= gp.just_pressed(GamepadButton::DPadRight);
        gp_confirm |= gp.just_pressed(GamepadButton::South);
        gp_cancel |= gp.just_pressed(GamepadButton::East);
        gp_view |= gp.just_pressed(GamepadButton::North);
    }

    // View mode toggle (`V` or Gamepad North).
    if keys.just_pressed(KeyCode::KeyV) || gp_view {
        sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_VIEW_MODE);
        if state.screen == GarageScreen::ViewMode {
            state.screen = GarageScreen::Top;
            state.cam_manual_orbit = false;
        } else {
            revert_preview(&mut state, &mut garage);
            state.screen = GarageScreen::ViewMode;
        }
        state.ui_dirty = true;
        return;
    }

    // `R` resets manual camera orbit back to the active `AcsisDisplaySt` preset.
    if keys.just_pressed(KeyCode::KeyR) {
        state.cam_manual_orbit = false;
    }

    // Mouse button clicks on UI action widgets.
    for (interaction, action) in &action_buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match *action {
            UiAction::Back => {
                trigger_cancel(&mut commands, &mut state, &mut garage, &mut cues, &mut audio_assets);
                return;
            }
            UiAction::ToggleViewMode => {
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_VIEW_MODE);
                state.screen = if state.screen == GarageScreen::ViewMode {
                    GarageScreen::Top
                } else {
                    revert_preview(&mut state, &mut garage);
                    GarageScreen::ViewMode
                };
                state.cam_manual_orbit = false;
                state.ui_dirty = true;
                return;
            }
            UiAction::PaintTab(tab) => {
                if state.paint_tab != tab {
                    state.paint_tab = tab;
                    state.cursor = 0;
                    state.scroll = 0;
                    state.cam_manual_orbit = false;
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_CATEGORY);
                    state.ui_dirty = true;
                }
                return;
            }
            UiAction::PaintChannel(ch) => {
                state.paint_channel = ch.min(6);
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                state.ui_dirty = true;
                return;
            }
            UiAction::PaintSwatch(idx) => {
                if let Some(&(_, rgba)) = PAINT_SWATCHES.get(idx) {
                    apply_custom_channel_color(&mut garage, state.paint_channel, rgba);
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_EQUIP);
                    state.ui_dirty = true;
                }
                return;
            }
            UiAction::PaintRgbDelta(comp, delta) => {
                step_custom_channel_rgb(&mut garage, state.paint_channel, comp, delta);
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                state.ui_dirty = true;
                return;
            }
            UiAction::PaintCamoDelta(delta) => {
                step_custom_camo(&mut garage, delta);
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_EQUIP);
                state.ui_dirty = true;
                return;
            }
            UiAction::SortieMapStep(delta) => {
                let n = SORTIE_MAPS.len() as isize;
                state.sortie_map_idx = (state.sortie_map_idx as isize + delta as isize).rem_euclid(n) as usize;
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                state.ui_dirty = true;
                return;
            }
            UiAction::SortieTargetStep(delta) => {
                let n = SORTIE_TARGETS.len() as isize;
                state.sortie_target_idx = (state.sortie_target_idx as isize + delta as isize).rem_euclid(n) as usize;
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                state.ui_dirty = true;
                return;
            }
            UiAction::LaunchSortie => {
                trigger_launch_sortie(
                    &mut commands,
                    &mut state,
                    &garage,
                    &mut cues,
                    &mut audio_assets,
                    &mut mode_switch,
                );
                return;
            }
        }
    }

    // Mouse clicks on list rows.
    for (interaction, UiRow(idx)) in &row_buttons {
        if *interaction == Interaction::Pressed {
            state.cursor = *idx;
            apply_cursor_preview(&mut state, &mut garage);
            trigger_confirm(
                &mut commands,
                &mut state,
                &mut garage,
                &mut cues,
                &mut audio_assets,
                &mut mode_switch,
            );
            return;
        }
    }

    if state.screen == GarageScreen::ViewMode {
        if keys.just_pressed(KeyCode::Escape) || keys.just_pressed(KeyCode::Backspace) || gp_cancel {
            sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_CANCEL);
            state.screen = GarageScreen::Top;
            state.cam_manual_orbit = false;
            state.ui_dirty = true;
        }
        return;
    }

    // Cancel / Back (`Esc` or `Backspace` or Gamepad East).
    if keys.just_pressed(KeyCode::Escape) || keys.just_pressed(KeyCode::Backspace) || gp_cancel {
        trigger_cancel(&mut commands, &mut state, &mut garage, &mut cues, &mut audio_assets);
        return;
    }

    // Paint tab switching via `Tab` / `Q` / `E`.
    if state.screen == GarageScreen::Paint
        && (keys.just_pressed(KeyCode::Tab)
            || keys.just_pressed(KeyCode::KeyQ)
            || keys.just_pressed(KeyCode::KeyE))
    {
        state.paint_tab = match state.paint_tab {
            PaintTab::Presets => PaintTab::Custom,
            PaintTab::Custom => PaintTab::Presets,
        };
        state.cursor = 0;
        state.scroll = 0;
        state.cam_manual_orbit = false;
        sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_CATEGORY);
        state.ui_dirty = true;
        return;
    }

    let up = keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::KeyW) || gp_up;
    let down = keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::KeyS) || gp_down;
    let left = keys.just_pressed(KeyCode::ArrowLeft) || keys.just_pressed(KeyCode::KeyA) || gp_left;
    let right = keys.just_pressed(KeyCode::ArrowRight) || keys.just_pressed(KeyCode::KeyD) || gp_right;
    let page_up = keys.just_pressed(KeyCode::PageUp);
    let page_down = keys.just_pressed(KeyCode::PageDown);
    let confirm =
        keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space) || gp_confirm;

    // Sortie screen Left/Right steps map or target directly.
    if state.screen == GarageScreen::SortieConfig && (left || right) {
        let delta: isize = if right { 1 } else { -1 };
        match state.cursor {
            0 => {
                let n = SORTIE_TARGETS.len() as isize;
                state.sortie_target_idx = (state.sortie_target_idx as isize + delta).rem_euclid(n) as usize;
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                state.ui_dirty = true;
                return;
            }
            1 => {
                let n = SORTIE_MAPS.len() as isize;
                state.sortie_map_idx = (state.sortie_map_idx as isize + delta).rem_euclid(n) as usize;
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                state.ui_dirty = true;
                return;
            }
            _ => {}
        }
    }

    // Custom Paint screen Left/Right adjusts swatches or camo directly.
    if state.screen == GarageScreen::Paint && state.paint_tab == PaintTab::Custom && (left || right) {
        let delta: i8 = if right { 1 } else { -1 };
        if state.cursor < 7 {
            // Cycle swatches on Left/Right for fast keyboard painting.
            let cur = current_channel_color(&garage, state.cursor);
            let idx = PAINT_SWATCHES
                .iter()
                .position(|&(_, c)| c == cur)
                .map(|i| (i as isize + delta as isize).rem_euclid(PAINT_SWATCHES.len() as isize) as usize)
                .unwrap_or(0);
            apply_custom_channel_color(&mut garage, state.cursor, PAINT_SWATCHES[idx].1);
            sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_EQUIP);
            state.ui_dirty = true;
            return;
        } else if state.cursor == 7 {
            step_custom_camo(&mut garage, delta);
            sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_EQUIP);
            state.ui_dirty = true;
            return;
        }
    }

    let count = current_list_len(&state, &garage);
    let step: isize = if up {
        -1
    } else if down {
        1
    } else if page_up {
        -(LIST_WINDOW as isize)
    } else if page_down {
        LIST_WINDOW as isize
    } else {
        0
    };

    if step != 0 && count > 0 {
        let next_cursor = (state.cursor as isize + step).rem_euclid(count as isize) as usize;
        state.cursor = next_cursor;
        clamp_scroll(&mut state.scroll, next_cursor, count);
        state.cam_manual_orbit = false;
        if state.screen == GarageScreen::Paint && state.paint_tab == PaintTab::Custom && state.cursor < 7 {
            state.paint_channel = state.cursor;
        }
        apply_cursor_preview(&mut state, &mut garage);
        sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
        state.ui_dirty = true;
        return;
    }

    if confirm {
        trigger_confirm(
            &mut commands,
            &mut state,
            &mut garage,
            &mut cues,
            &mut audio_assets,
            &mut mode_switch,
        );
    }
}

fn current_list_len(state: &GarageState, garage: &crate::Garage) -> usize {
    match state.screen {
        GarageScreen::Top => TOP_ITEMS.len(),
        GarageScreen::AssembleSlots => AssembleSlot::ALL.len(),
        GarageScreen::AssembleParts { slot } => parts_for_slot(slot).len(),
        GarageScreen::Paint => match state.paint_tab {
            PaintTab::Presets => state.color_presets.len(),
            PaintTab::Custom => 8, // 7 channels + Camouflage
        },
        GarageScreen::Design => garage.designs.len(),
        GarageScreen::GarageCustom => state.workshops.len(),
        GarageScreen::SortieConfig => 3, // Target Set, Map, Launch
        GarageScreen::ViewMode => 0,
    }
}

fn clamp_scroll(scroll: &mut usize, cursor: usize, count: usize) {
    if count <= LIST_WINDOW {
        *scroll = 0;
        return;
    }
    if cursor < *scroll {
        *scroll = cursor;
    } else if cursor >= *scroll + LIST_WINDOW {
        *scroll = cursor + 1 - LIST_WINDOW;
    }
    *scroll = (*scroll).min(count.saturating_sub(LIST_WINDOW));
}

fn revert_preview(state: &mut GarageState, garage: &mut crate::Garage) {
    let mut changed = false;
    if let Some(d) = state.saved_design.take() {
        garage.active_design = d;
        changed = true;
    }
    if let Some(c) = state.saved_color.take() {
        garage.custom_color = c;
        changed = true;
    }
    if let Some(n) = state.saved_name.take() {
        garage.active_name = n;
    }
    if changed {
        garage.dirty = true;
    }
}

fn apply_cursor_preview(state: &mut GarageState, garage: &mut crate::Garage) {
    match state.screen {
        GarageScreen::AssembleParts { slot } => {
            let parts = parts_for_slot(slot);
            if let Some(&part_id) = parts.get(state.cursor) {
                if slot.get(&garage.active_design) != part_id {
                    slot.set(&mut garage.active_design, part_id);
                    garage.dirty = true;
                }
            }
        }
        GarageScreen::Paint if state.paint_tab == PaintTab::Presets => {
            if let Some(&color_id) = state.color_presets.get(state.cursor) {
                if let Some(col) = paint::load_accolor(&garage.disc, color_id) {
                    garage.active_design.coloring_id = color_id;
                    garage.custom_color = Some(col);
                    garage.dirty = true;
                }
            }
        }
        GarageScreen::Design => {
            if let Some(row) = garage.designs.get(state.cursor) {
                garage.active_design = row.data;
                garage.active_name = display_design_name(row.id, row.name);
                garage.custom_color = paint::load_accolor(&garage.disc, row.data.coloring_id);
                garage.dirty = true;
            }
        }
        GarageScreen::GarageCustom => {
            if state.cursor < state.workshops.len() && state.workshop_idx != state.cursor {
                state.workshop_idx = state.cursor;
            }
        }
        _ => {}
    }
}

fn trigger_cancel(
    commands: &mut Commands,
    state: &mut GarageState,
    garage: &mut crate::Garage,
    cues: &mut Cues,
    audio_assets: &mut Assets<AudioSource>,
) {
    match state.screen {
        GarageScreen::Top => {}
        GarageScreen::AssembleParts { .. } => {
            revert_preview(state, garage);
            state.screen = GarageScreen::AssembleSlots;
            state.cursor = state.slot_cursor;
            clamp_scroll(&mut state.scroll, state.cursor, AssembleSlot::ALL.len());
            state.cam_manual_orbit = false;
            sound::cue(commands, cues, audio_assets, CUE_CANCEL);
            state.ui_dirty = true;
        }
        GarageScreen::AssembleSlots
        | GarageScreen::Paint
        | GarageScreen::Design
        | GarageScreen::GarageCustom
        | GarageScreen::SortieConfig
        | GarageScreen::ViewMode => {
            revert_preview(state, garage);
            state.screen = GarageScreen::Top;
            state.cursor = 0;
            state.scroll = 0;
            state.cam_manual_orbit = false;
            sound::cue(commands, cues, audio_assets, CUE_CANCEL);
            state.ui_dirty = true;
        }
    }
}

fn trigger_confirm(
    commands: &mut Commands,
    state: &mut GarageState,
    garage: &mut crate::Garage,
    cues: &mut Cues,
    audio_assets: &mut Assets<AudioSource>,
    mode_switch: &mut ModeSwitch,
) {
    match state.screen {
        GarageScreen::Top => {
            sound::cue(commands, cues, audio_assets, CUE_OK);
            state.cam_manual_orbit = false;
            match state.cursor {
                0 => {
                    state.screen = GarageScreen::AssembleSlots;
                    state.cursor = state.slot_cursor;
                    clamp_scroll(&mut state.scroll, state.cursor, AssembleSlot::ALL.len());
                }
                1 => {
                    state.saved_design = Some(garage.active_design);
                    state.saved_color = Some(garage.custom_color);
                    state.screen = GarageScreen::Paint;
                    state.paint_tab = PaintTab::Presets;
                    state.cursor = state
                        .color_presets
                        .iter()
                        .position(|&id| id == garage.active_design.coloring_id)
                        .unwrap_or(0);
                    clamp_scroll(&mut state.scroll, state.cursor, state.color_presets.len());
                }
                2 => {
                    state.saved_design = Some(garage.active_design);
                    state.saved_color = Some(garage.custom_color);
                    state.saved_name = Some(garage.active_name.clone());
                    state.screen = GarageScreen::Design;
                    state.cursor = garage.current.min(garage.designs.len().saturating_sub(1));
                    clamp_scroll(&mut state.scroll, state.cursor, garage.designs.len());
                }
                3 => {
                    state.screen = GarageScreen::GarageCustom;
                    state.cursor = state.workshop_idx;
                    clamp_scroll(&mut state.scroll, state.cursor, state.workshops.len());
                }
                4 => {
                    state.screen = GarageScreen::SortieConfig;
                    state.cursor = 2; // Highlight LAUNCH SORTIE by default for instant sortie
                    state.scroll = 0;
                }
                5 => {
                    state.screen = GarageScreen::ViewMode;
                }
                _ => {}
            }
            state.ui_dirty = true;
        }
        GarageScreen::AssembleSlots => {
            let Some(&slot) = AssembleSlot::ALL.get(state.cursor) else {
                return;
            };
            if has_weapon_arms(&garage.active_design) && slot.blocked_by_weapon_arm() {
                sound::cue(commands, cues, audio_assets, CUE_ERROR);
                state.set_banner(
                    "LOCKED BY WEAPON ARMS - swap ARMS to equip separate weapons",
                    Color::srgb(1.0, 0.42, 0.35),
                );
                return;
            }
            state.slot_cursor = state.cursor;
            state.saved_design = Some(garage.active_design);
            let parts = parts_for_slot(slot);
            let equipped = slot.get(&garage.active_design);
            state.cursor = parts.iter().position(|&id| id == equipped).unwrap_or(0);
            clamp_scroll(&mut state.scroll, state.cursor, parts.len());
            state.screen = GarageScreen::AssembleParts { slot };
            state.cam_manual_orbit = false;
            sound::cue(commands, cues, audio_assets, CUE_CATEGORY);
            state.ui_dirty = true;
        }
        GarageScreen::AssembleParts { slot } => {
            let parts = parts_for_slot(slot);
            let Some(&part_id) = parts.get(state.cursor) else {
                return;
            };
            slot.set(&mut garage.active_design, part_id);
            garage.dirty = true;
            state.saved_design = None;
            let cue = if part_id == 0 { CUE_UNEQUIP } else { CUE_EQUIP };
            sound::cue(commands, cues, audio_assets, cue);
            let pname = part_name(state.parts_fmg.as_ref(), slot.category(), part_id);
            state.set_banner(
                format!("{}: {}", slot.label(), pname),
                Color::srgb(0.45, 0.92, 0.62),
            );
            state.screen = GarageScreen::AssembleSlots;
            state.cursor = state.slot_cursor;
            clamp_scroll(&mut state.scroll, state.cursor, AssembleSlot::ALL.len());
            state.ui_dirty = true;
        }
        GarageScreen::Paint => {
            match state.paint_tab {
                PaintTab::Presets => {
                    if let Some(&color_id) = state.color_presets.get(state.cursor) {
                        garage.active_design.coloring_id = color_id;
                        garage.custom_color = paint::load_accolor(&garage.disc, color_id);
                        garage.dirty = true;
                        state.saved_design = None;
                        state.saved_color = None;
                        sound::cue(commands, cues, audio_assets, CUE_EQUIP);
                        state.set_banner(
                            format!("APPLIED COLOR PRESET {:04}", color_id),
                            Color::srgb(0.45, 0.92, 0.62),
                        );
                    }
                }
                PaintTab::Custom => {
                    state.saved_design = None;
                    state.saved_color = None;
                    sound::cue(commands, cues, audio_assets, CUE_OK);
                    state.set_banner("CUSTOM PAINT SAVED", Color::srgb(0.45, 0.92, 0.62));
                }
            }
            state.ui_dirty = true;
        }
        GarageScreen::Design => {
            if let Some(row) = garage.designs.get(state.cursor) {
                garage.current = state.cursor;
                garage.active_design = row.data;
                garage.active_name = display_design_name(row.id, row.name);
                garage.custom_color = paint::load_accolor(&garage.disc, row.data.coloring_id);
                garage.dirty = true;
                state.saved_design = None;
                state.saved_color = None;
                state.saved_name = None;
                sound::cue(commands, cues, audio_assets, CUE_EQUIP);
                state.set_banner(
                    format!("LOADED DESIGN {}", garage.active_name),
                    Color::srgb(0.45, 0.92, 0.62),
                );
                state.screen = GarageScreen::Top;
                state.cursor = 0;
                state.scroll = 0;
                state.ui_dirty = true;
            }
        }
        GarageScreen::GarageCustom => {
            if state.cursor < state.workshops.len() {
                state.workshop_idx = state.cursor;
                sound::cue(commands, cues, audio_assets, CUE_OK);
                if let Some(ws) = state.active_workshop() {
                    let msg = format!("WORKSHOP: {} - {}", ws.title, ws.theme);
                    state.set_banner(msg, Color::srgb(0.45, 0.92, 0.62));
                }
                state.screen = GarageScreen::Top;
                state.cursor = 3;
                state.scroll = 0;
                state.ui_dirty = true;
            }
        }
        GarageScreen::SortieConfig => match state.cursor {
            0 => {
                let n = SORTIE_TARGETS.len();
                state.sortie_target_idx = (state.sortie_target_idx + 1) % n;
                sound::cue(commands, cues, audio_assets, CUE_MOVE);
                state.ui_dirty = true;
            }
            1 => {
                let n = SORTIE_MAPS.len();
                state.sortie_map_idx = (state.sortie_map_idx + 1) % n;
                sound::cue(commands, cues, audio_assets, CUE_MOVE);
                state.ui_dirty = true;
            }
            _ => {
                trigger_launch_sortie(
                    commands,
                    state,
                    garage,
                    cues,
                    audio_assets,
                    mode_switch,
                );
            }
        },
        GarageScreen::ViewMode => {}
    }
}

fn trigger_launch_sortie(
    commands: &mut Commands,
    state: &mut GarageState,
    garage: &crate::Garage,
    cues: &mut Cues,
    audio_assets: &mut Assets<AudioSource>,
    mode_switch: &mut ModeSwitch,
) {
    if !AcCtrlParam::buildable(&garage.active_design) {
        sound::cue(commands, cues, audio_assets, CUE_ERROR);
        state.set_banner(
            "CANNOT SORTIE - core frame / inner part missing",
            Color::srgb(1.0, 0.35, 0.30),
        );
        return;
    }
    sound::cue(commands, cues, audio_assets, CUE_GARAGE_OUT);
    let map = SORTIE_MAPS[state.sortie_map_idx].0.to_string();
    let test = SORTIE_TARGETS[state.sortie_target_idx].0.to_string();
    mode_switch.target = Some(Mode::Sortie);
    mode_switch.sortie_map = Some(map);
    mode_switch.sortie_test = Some(test);
}

fn ensure_custom_color(garage: &mut crate::Garage) -> &mut AcColor {
    if garage.custom_color.is_none() {
        let loaded = paint::load_accolor(&garage.disc, garage.active_design.coloring_id)
            .unwrap_or_else(default_accolor);
        garage.custom_color = Some(loaded);
    }
    garage.custom_color.as_mut().unwrap()
}

pub fn default_accolor() -> AcColor {
    let mut groups = [[[128, 128, 128, 255]; 6]; GROUP_COUNT];
    for g in &mut groups {
        g[CH_BASE] = [92, 98, 106, 255];
        g[CH_AID] = [148, 152, 158, 255];
        g[CH_AID2] = [56, 60, 68, 255];
        g[CH_OPTIONAL] = [188, 132, 44, 255];
        g[CH_JOINT] = [66, 70, 76, 255];
        g[CH_KOJIMA] = [255, 168, 48, 255];
    }
    AcColor {
        groups,
        camo: [0; GROUP_COUNT],
        eye: [255, 180, 48, 255],
    }
}

fn current_channel_color(garage: &crate::Garage, channel: usize) -> [u8; 4] {
    let col = garage
        .custom_color
        .or_else(|| paint::load_accolor(&garage.disc, garage.active_design.coloring_id))
        .unwrap_or_else(default_accolor);
    if channel >= 6 {
        col.eye
    } else {
        col.groups[0][channel]
    }
}

fn apply_custom_channel_color(garage: &mut crate::Garage, channel: usize, rgba: [u8; 4]) {
    let col = ensure_custom_color(garage);
    if channel >= 6 {
        col.eye = rgba;
    } else {
        for g in 0..GROUP_COUNT {
            col.groups[g][channel] = rgba;
        }
    }
    garage.dirty = true;
}

fn step_custom_channel_rgb(garage: &mut crate::Garage, channel: usize, comp: usize, delta: i16) {
    let mut rgba = current_channel_color(garage, channel);
    let next = (rgba[comp] as i16 + delta).clamp(0, 255) as u8;
    rgba[comp] = next;
    apply_custom_channel_color(garage, channel, rgba);
}

fn step_custom_camo(garage: &mut crate::Garage, delta: i8) {
    let col = ensure_custom_color(garage);
    let cur = col.camo[0] as i16;
    let next = (cur + delta as i16).rem_euclid(12) as u8; // 0 = NONE, 1..=11 = ca0001..ca0011
    for slot in &mut col.camo {
        *slot = next;
    }
    garage.dirty = true;
}

/// Rebuilds the interactive ACVD Garage HUD whenever `GarageState::ui_dirty` is set.
pub fn sync_ui(
    mut commands: Commands,
    mode: Res<Mode>,
    mut state: ResMut<GarageState>,
    garage: Res<crate::Garage>,
    existing: Query<Entity, With<GarageUi>>,
) {
    if *mode != Mode::Garage {
        for e in &existing {
            commands.entity(e).despawn();
        }
        return;
    }
    if !state.ui_dirty {
        return;
    }
    state.ui_dirty = false;
    for e in &existing {
        commands.entity(e).despawn();
    }

    let bg_panel = Color::srgba(0.05, 0.07, 0.10, 0.86);
    let bg_header = Color::srgba(0.08, 0.12, 0.17, 0.94);
    let bg_row = Color::srgba(0.09, 0.12, 0.16, 0.65);
    let bg_sel = Color::srgba(0.88, 0.58, 0.14, 0.92);
    let fg_text = Color::srgb(0.90, 0.93, 0.96);
    let fg_dim = Color::srgb(0.58, 0.66, 0.74);
    let fg_cyan = Color::srgb(0.38, 0.86, 0.96);
    let fg_amber = Color::srgb(0.98, 0.74, 0.24);
    let fg_dark = Color::srgb(0.06, 0.07, 0.09);
    let border_col = Color::srgba(0.32, 0.44, 0.56, 0.55);

    let root = commands
        .spawn((
            GarageUi,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
        ))
        .id();

    // View Mode shows only a minimal unobtrusive bottom bar.
    if state.screen == GarageScreen::ViewMode {
        commands.entity(root).with_children(|p| {
            p.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: px(24.0),
                    bottom: px(18.0),
                    padding: UiRect::axes(px(14.0), px(7.0)),
                    ..default()
                },
                BackgroundColor(bg_panel),
                BorderColor::all(border_col),
            ))
            .with_children(|bar| {
                bar.spawn((
                    Text::new("VIEW MODE   |   Drag Left/Right: Orbit   |   Wheel: Zoom   |   R: Reset   |   V / Esc: Return"),
                    TextFont {
                        font_size: FontSize::Px(13.0),
                        ..default()
                    },
                    TextColor(fg_cyan),
                ));
            });
        });
        return;
    }

    let ws_label = state
        .active_workshop()
        .map(|w| format!("{} - {}", w.title, w.theme))
        .unwrap_or_else(|| "Workshop 5 - Underpass".to_string());

    // Top bar: ACVD GARAGE title + active design + workshop name.
    commands.entity(root).with_children(|p| {
        p.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(20.0),
                right: px(20.0),
                top: px(14.0),
                height: px(40.0),
                padding: UiRect::axes(px(14.0), px(6.0)),
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::Center,
                border: UiRect::bottom(px(2.0)),
                ..default()
            },
            BackgroundColor(bg_header),
            BorderColor::all(fg_amber),
        ))
        .with_children(|top| {
            let screen_title = match state.screen {
                GarageScreen::Top => "GARAGE",
                GarageScreen::AssembleSlots => "ASSEMBLE",
                GarageScreen::AssembleParts { slot } => slot.label(),
                GarageScreen::Paint => "PAINT & CAMOUFLAGE",
                GarageScreen::Design => "AC DESIGN PRESETS",
                GarageScreen::GarageCustom => "GARAGE CUSTOM",
                GarageScreen::SortieConfig => "AC TEST SORTIE",
                GarageScreen::ViewMode => "VIEW MODE",
            };
            top.spawn((
                Text::new(format!("ACVD  //  {screen_title}")),
                TextFont {
                    font_size: FontSize::Px(18.0),
                    ..default()
                },
                TextColor(fg_amber),
            ));
            top.spawn((
                Text::new(format!("AC: {}   |   {}", garage.active_name, ws_label)),
                TextFont {
                    font_size: FontSize::Px(13.0),
                    ..default()
                },
                TextColor(fg_cyan),
            ));
        });

        // Transient feedback banner (top-center).
        if let Some((ref msg, col, _)) = state.banner {
            p.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: px(60.0),
                    left: Val::Percent(50.0),
                    margin: UiRect::left(px(-220.0)),
                    width: px(440.0),
                    padding: UiRect::axes(px(14.0), px(5.0)),
                    justify_content: JustifyContent::Center,
                    border: UiRect::all(px(1.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.05, 0.08, 0.12, 0.90)),
                BorderColor::all(col),
            ))
            .with_children(|b| {
                b.spawn((
                    Text::new(msg.clone()),
                    TextFont {
                        font_size: FontSize::Px(13.0),
                        ..default()
                    },
                    TextColor(col),
                ));
            });
        }

        // Left interactive menu panel.
        p.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(20.0),
                top: px(66.0),
                width: px(370.0),
                padding: UiRect::all(px(10.0)),
                flex_direction: FlexDirection::Column,
                row_gap: px(4.0),
                border: UiRect::all(px(1.0)),
                ..default()
            },
            BackgroundColor(bg_panel),
            BorderColor::all(border_col),
        ))
        .with_children(|panel| {
            match state.screen {
                GarageScreen::Top => {
                    for (idx, &(title, desc)) in TOP_ITEMS.iter().enumerate() {
                        let sel = state.cursor == idx;
                        panel
                            .spawn((
                                Button,
                                UiRow(idx),
                                Node {
                                    width: Val::Percent(100.0),
                                    padding: UiRect::axes(px(10.0), px(8.0)),
                                    flex_direction: FlexDirection::Column,
                                    row_gap: px(2.0),
                                    ..default()
                                },
                                BackgroundColor(if sel { bg_sel } else { bg_row }),
                            ))
                            .with_children(|r| {
                                r.spawn((
                                    Text::new(title),
                                    TextFont {
                                        font_size: FontSize::Px(15.0),
                                        ..default()
                                    },
                                    TextColor(if sel { fg_dark } else { fg_text }),
                                ));
                                r.spawn((
                                    Text::new(desc),
                                    TextFont {
                                        font_size: FontSize::Px(11.0),
                                        ..default()
                                    },
                                    TextColor(if sel { fg_dark } else { fg_dim }),
                                ));
                            });
                    }
                }
                GarageScreen::AssembleSlots => {
                    let wa = has_weapon_arms(&garage.active_design);
                    let start = state.scroll;
                    let end = (start + LIST_WINDOW).min(AssembleSlot::ALL.len());
                    for idx in start..end {
                        let slot = AssembleSlot::ALL[idx];
                        if let Some(hdr) = slot.group_header() {
                            panel.spawn((
                                Text::new(format!("-- {hdr} --")),
                                TextFont {
                                    font_size: FontSize::Px(11.0),
                                    ..default()
                                },
                                TextColor(fg_cyan),
                            ));
                        }
                        let sel = state.cursor == idx;
                        let locked = wa && slot.blocked_by_weapon_arm();
                        let equipped = if locked {
                            "(WEAPON ARM)".to_string()
                        } else {
                            part_name(
                                state.parts_fmg.as_ref(),
                                slot.category(),
                                slot.get(&garage.active_design),
                            )
                        };
                        panel
                            .spawn((
                                Button,
                                UiRow(idx),
                                Node {
                                    width: Val::Percent(100.0),
                                    padding: UiRect::axes(px(8.0), px(5.0)),
                                    justify_content: JustifyContent::SpaceBetween,
                                    ..default()
                                },
                                BackgroundColor(if sel { bg_sel } else { bg_row }),
                            ))
                            .with_children(|r| {
                                r.spawn((
                                    Text::new(slot.label()),
                                    TextFont {
                                        font_size: FontSize::Px(12.0),
                                        ..default()
                                    },
                                    TextColor(if sel {
                                        fg_dark
                                    } else if locked {
                                        Color::srgb(0.50, 0.45, 0.45)
                                    } else {
                                        fg_dim
                                    }),
                                ));
                                r.spawn((
                                    Text::new(truncate(&equipped, 22)),
                                    TextFont {
                                        font_size: FontSize::Px(12.0),
                                        ..default()
                                    },
                                    TextColor(if sel {
                                        fg_dark
                                    } else if locked {
                                        Color::srgb(0.85, 0.42, 0.35)
                                    } else {
                                        fg_text
                                    }),
                                ));
                            });
                    }
                    spawn_back_btn(panel, "< BACK TO TOP (Esc)", bg_row, fg_cyan);
                }
                GarageScreen::AssembleParts { slot } => {
                    let parts = parts_for_slot(slot);
                    let orig_id = state
                        .saved_design
                        .map(|d| slot.get(&d))
                        .unwrap_or_else(|| slot.get(&garage.active_design));
                    panel.spawn((
                        Text::new(format!(
                            "SELECT {} ({} / {})",
                            slot.label(),
                            (state.cursor + 1).min(parts.len()),
                            parts.len()
                        )),
                        TextFont {
                            font_size: FontSize::Px(12.0),
                            ..default()
                        },
                        TextColor(fg_cyan),
                    ));
                    let start = state.scroll;
                    let end = (start + LIST_WINDOW).min(parts.len());
                    for idx in start..end {
                        let pid = parts[idx];
                        let sel = state.cursor == idx;
                        let is_equipped = pid == orig_id;
                        let pname = part_name(state.parts_fmg.as_ref(), slot.category(), pid);
                        let wt = if pid == 0 {
                            0
                        } else {
                            acvd_data::part_field(pid as i64, slot.category(), "weight") as i32
                        };
                        let en = if pid == 0 {
                            0
                        } else {
                            acvd_data::part_field(
                                pid as i64,
                                slot.category(),
                                "en_consumption",
                            ) as i32
                        };
                        let tag = if is_equipped { " [E]" } else { "" };
                        panel
                            .spawn((
                                Button,
                                UiRow(idx),
                                Node {
                                    width: Val::Percent(100.0),
                                    padding: UiRect::axes(px(8.0), px(5.0)),
                                    justify_content: JustifyContent::SpaceBetween,
                                    ..default()
                                },
                                BackgroundColor(if sel { bg_sel } else { bg_row }),
                            ))
                            .with_children(|r| {
                                r.spawn((
                                    Text::new(format!("{}{}", truncate(&pname, 22), tag)),
                                    TextFont {
                                        font_size: FontSize::Px(12.0),
                                        ..default()
                                    },
                                    TextColor(if sel {
                                        fg_dark
                                    } else if is_equipped {
                                        fg_amber
                                    } else {
                                        fg_text
                                    }),
                                ));
                                r.spawn((
                                    Text::new(format!("W:{wt}  EN:{en}")),
                                    TextFont {
                                        font_size: FontSize::Px(11.0),
                                        ..default()
                                    },
                                    TextColor(if sel { fg_dark } else { fg_dim }),
                                ));
                            });
                    }
                    spawn_back_btn(panel, "< CANCEL / BACK (Esc)", bg_row, fg_cyan);
                }
                GarageScreen::Paint => {
                    // Tab bar: PRESETS | CUSTOM
                    panel
                        .spawn(Node {
                            width: Val::Percent(100.0),
                            column_gap: px(6.0),
                            margin: UiRect::bottom(px(4.0)),
                            ..default()
                        })
                        .with_children(|tabs| {
                            for (tab, label) in [
                                (PaintTab::Presets, "COLOR PRESETS"),
                                (PaintTab::Custom, "CUSTOM PALETTE"),
                            ] {
                                let active = state.paint_tab == tab;
                                tabs.spawn((
                                    Button,
                                    UiAction::PaintTab(tab),
                                    Node {
                                        flex_grow: 1.0,
                                        padding: UiRect::axes(px(8.0), px(5.0)),
                                        justify_content: JustifyContent::Center,
                                        ..default()
                                    },
                                    BackgroundColor(if active { bg_sel } else { bg_row }),
                                ))
                                .with_children(|b| {
                                    b.spawn((
                                        Text::new(label),
                                        TextFont {
                                            font_size: FontSize::Px(12.0),
                                            ..default()
                                        },
                                        TextColor(if active { fg_dark } else { fg_text }),
                                    ));
                                });
                            }
                        });

                    match state.paint_tab {
                        PaintTab::Presets => {
                            let start = state.scroll;
                            let end = (start + LIST_WINDOW).min(state.color_presets.len());
                            for idx in start..end {
                                let cid = state.color_presets[idx];
                                let sel = state.cursor == idx;
                                let preview = paint::load_accolor(&garage.disc, cid);
                                panel
                                    .spawn((
                                        Button,
                                        UiRow(idx),
                                        Node {
                                            width: Val::Percent(100.0),
                                            padding: UiRect::axes(px(8.0), px(5.0)),
                                            justify_content: JustifyContent::SpaceBetween,
                                            align_items: AlignItems::Center,
                                            ..default()
                                        },
                                        BackgroundColor(if sel { bg_sel } else { bg_row }),
                                    ))
                                    .with_children(|r| {
                                        r.spawn((
                                            Text::new(format!("COLOR PRESET {cid:04}")),
                                            TextFont {
                                                font_size: FontSize::Px(12.0),
                                                ..default()
                                            },
                                            TextColor(if sel { fg_dark } else { fg_text }),
                                        ));
                                        if let Some(col) = preview {
                                            r.spawn(Node {
                                                column_gap: px(3.0),
                                                ..default()
                                            })
                                            .with_children(|dots| {
                                                for ch in [CH_BASE, CH_AID, CH_OPTIONAL, CH_KOJIMA] {
                                                    let c = col.groups[0][ch];
                                                    dots.spawn((
                                                        Node {
                                                            width: px(14.0),
                                                            height: px(12.0),
                                                            ..default()
                                                        },
                                                        BackgroundColor(Color::srgb_u8(c[0], c[1], c[2])),
                                                    ));
                                                }
                                            });
                                        }
                                    });
                            }
                        }
                        PaintTab::Custom => {
                            for (ch, &name) in PAINT_CHANNEL_NAMES.iter().enumerate() {
                                let sel = state.paint_channel == ch;
                                let rgba = current_channel_color(&garage, ch);
                                panel
                                    .spawn((
                                        Button,
                                        UiAction::PaintChannel(ch),
                                        Node {
                                            width: Val::Percent(100.0),
                                            padding: UiRect::axes(px(8.0), px(5.0)),
                                            justify_content: JustifyContent::SpaceBetween,
                                            align_items: AlignItems::Center,
                                            ..default()
                                        },
                                        BackgroundColor(if sel { bg_sel } else { bg_row }),
                                    ))
                                    .with_children(|r| {
                                        r.spawn((
                                            Text::new(name),
                                            TextFont {
                                                font_size: FontSize::Px(12.0),
                                                ..default()
                                            },
                                            TextColor(if sel { fg_dark } else { fg_text }),
                                        ));
                                        r.spawn(Node {
                                            column_gap: px(8.0),
                                            align_items: AlignItems::Center,
                                            ..default()
                                        })
                                        .with_children(|right| {
                                            right.spawn((
                                                Text::new(format!("{},{},{}", rgba[0], rgba[1], rgba[2])),
                                                TextFont {
                                                    font_size: FontSize::Px(11.0),
                                                    ..default()
                                                },
                                                TextColor(if sel { fg_dark } else { fg_dim }),
                                            ));
                                            right.spawn((
                                                Node {
                                                    width: px(22.0),
                                                    height: px(14.0),
                                                    border: UiRect::all(px(1.0)),
                                                    ..default()
                                                },
                                                BackgroundColor(Color::srgb_u8(rgba[0], rgba[1], rgba[2])),
                                                BorderColor::all(Color::WHITE),
                                            ));
                                        });
                                    });
                            }

                            // RGB fine adjustment row for selected channel.
                            let rgba = current_channel_color(&garage, state.paint_channel);
                            panel
                                .spawn(Node {
                                    width: Val::Percent(100.0),
                                    column_gap: px(4.0),
                                    margin: UiRect::top(px(4.0)),
                                    ..default()
                                })
                                .with_children(|rgb| {
                                    for (comp, lbl, col) in [
                                        (0usize, "R", Color::srgb(1.0, 0.45, 0.45)),
                                        (1, "G", Color::srgb(0.45, 1.0, 0.55)),
                                        (2, "B", Color::srgb(0.45, 0.72, 1.0)),
                                    ] {
                                        rgb.spawn((
                                            Button,
                                            UiAction::PaintRgbDelta(comp, -16),
                                            Node {
                                                padding: UiRect::axes(px(7.0), px(4.0)),
                                                ..default()
                                            },
                                            BackgroundColor(bg_row),
                                        ))
                                        .with_children(|b| {
                                            b.spawn((
                                                Text::new(format!("{lbl}-")),
                                                TextFont {
                                                    font_size: FontSize::Px(11.0),
                                                    ..default()
                                                },
                                                TextColor(col),
                                            ));
                                        });
                                        rgb.spawn((
                                            Text::new(format!("{:3}", rgba[comp])),
                                            TextFont {
                                                font_size: FontSize::Px(11.0),
                                                ..default()
                                            },
                                            TextColor(fg_text),
                                        ));
                                        rgb.spawn((
                                            Button,
                                            UiAction::PaintRgbDelta(comp, 16),
                                            Node {
                                                padding: UiRect::axes(px(7.0), px(4.0)),
                                                ..default()
                                            },
                                            BackgroundColor(bg_row),
                                        ))
                                        .with_children(|b| {
                                            b.spawn((
                                                Text::new(format!("{lbl}+")),
                                                TextFont {
                                                    font_size: FontSize::Px(11.0),
                                                    ..default()
                                                },
                                                TextColor(col),
                                            ));
                                        });
                                    }
                                });

                            // 12-swatch tactical color grid.
                            panel
                                .spawn(Node {
                                    width: Val::Percent(100.0),
                                    flex_wrap: FlexWrap::Wrap,
                                    row_gap: px(4.0),
                                    column_gap: px(4.0),
                                    margin: UiRect::top(px(4.0)),
                                    ..default()
                                })
                                .with_children(|grid| {
                                    for (i, &(_, c)) in PAINT_SWATCHES.iter().enumerate() {
                                        grid.spawn((
                                            Button,
                                            UiAction::PaintSwatch(i),
                                            Node {
                                                width: px(52.0),
                                                height: px(20.0),
                                                border: UiRect::all(px(1.0)),
                                                ..default()
                                            },
                                            BackgroundColor(Color::srgb_u8(c[0], c[1], c[2])),
                                            BorderColor::all(border_col),
                                        ));
                                    }
                                });

                            // Camouflage pattern selector (`NONE` or `CA0001..CA0011`).
                            let camo_id = garage
                                .custom_color
                                .map(|c| c.camo[0])
                                .unwrap_or(0);
                            let camo_label = if camo_id == 0 {
                                "CAMO: NONE".to_string()
                            } else {
                                format!("CAMO: CA{camo_id:04}")
                            };
                            panel
                                .spawn(Node {
                                    width: Val::Percent(100.0),
                                    justify_content: JustifyContent::SpaceBetween,
                                    align_items: AlignItems::Center,
                                    margin: UiRect::top(px(4.0)),
                                    ..default()
                                })
                                .with_children(|row| {
                                    row.spawn((
                                        Button,
                                        UiAction::PaintCamoDelta(-1),
                                        Node {
                                            padding: UiRect::axes(px(10.0), px(4.0)),
                                            ..default()
                                        },
                                        BackgroundColor(bg_row),
                                    ))
                                    .with_children(|b| {
                                        b.spawn((
                                            Text::new("< PREV"),
                                            TextFont {
                                                font_size: FontSize::Px(11.0),
                                                ..default()
                                            },
                                            TextColor(fg_cyan),
                                        ));
                                    });
                                    row.spawn((
                                        Text::new(camo_label),
                                        TextFont {
                                            font_size: FontSize::Px(12.0),
                                            ..default()
                                        },
                                        TextColor(fg_amber),
                                    ));
                                    row.spawn((
                                        Button,
                                        UiAction::PaintCamoDelta(1),
                                        Node {
                                            padding: UiRect::axes(px(10.0), px(4.0)),
                                            ..default()
                                        },
                                        BackgroundColor(bg_row),
                                    ))
                                    .with_children(|b| {
                                        b.spawn((
                                            Text::new("NEXT >"),
                                            TextFont {
                                                font_size: FontSize::Px(11.0),
                                                ..default()
                                            },
                                            TextColor(fg_cyan),
                                        ));
                                    });
                                });
                        }
                    }
                    spawn_back_btn(panel, "< BACK TO TOP (Esc)", bg_row, fg_cyan);
                }
                GarageScreen::Design => {
                    panel.spawn((
                        Text::new(format!(
                            "PRESET DESIGNS ({} / {})",
                            (state.cursor + 1).min(garage.designs.len()),
                            garage.designs.len()
                        )),
                        TextFont {
                            font_size: FontSize::Px(12.0),
                            ..default()
                        },
                        TextColor(fg_cyan),
                    ));
                    let start = state.scroll;
                    let end = (start + LIST_WINDOW).min(garage.designs.len());
                    for idx in start..end {
                        let row = garage.designs[idx];
                        let sel = state.cursor == idx;
                        panel
                            .spawn((
                                Button,
                                UiRow(idx),
                                Node {
                                    width: Val::Percent(100.0),
                                    padding: UiRect::axes(px(8.0), px(5.0)),
                                    justify_content: JustifyContent::SpaceBetween,
                                    ..default()
                                },
                                BackgroundColor(if sel { bg_sel } else { bg_row }),
                            ))
                            .with_children(|r| {
                                r.spawn((
                                    Text::new(truncate(&display_design_name(row.id, row.name), 24)),
                                    TextFont {
                                        font_size: FontSize::Px(12.0),
                                        ..default()
                                    },
                                    TextColor(if sel { fg_dark } else { fg_text }),
                                ));
                                r.spawn((
                                    Text::new(format!("CLR {:04}", row.data.coloring_id)),
                                    TextFont {
                                        font_size: FontSize::Px(11.0),
                                        ..default()
                                    },
                                    TextColor(if sel { fg_dark } else { fg_dim }),
                                ));
                            });
                    }
                    spawn_back_btn(panel, "< CANCEL / BACK (Esc)", bg_row, fg_cyan);
                }
                GarageScreen::GarageCustom => {
                    panel.spawn((
                        Text::new("SELECT 3D WORKSHOP (param/customgarage.bin)"),
                        TextFont {
                            font_size: FontSize::Px(12.0),
                            ..default()
                        },
                        TextColor(fg_cyan),
                    ));
                    let start = state.scroll;
                    let end = (start + LIST_WINDOW).min(state.workshops.len());
                    for idx in start..end {
                        let ws = &state.workshops[idx];
                        let sel = state.cursor == idx;
                        panel
                            .spawn((
                                Button,
                                UiRow(idx),
                                Node {
                                    width: Val::Percent(100.0),
                                    padding: UiRect::axes(px(8.0), px(5.0)),
                                    justify_content: JustifyContent::SpaceBetween,
                                    ..default()
                                },
                                BackgroundColor(if sel { bg_sel } else { bg_row }),
                            ))
                            .with_children(|r| {
                                r.spawn((
                                    Text::new(format!("{} - {}", ws.title, ws.theme)),
                                    TextFont {
                                        font_size: FontSize::Px(12.0),
                                        ..default()
                                    },
                                    TextColor(if sel { fg_dark } else { fg_text }),
                                ));
                                r.spawn((
                                    Text::new(format!("menu{:04}", ws.model_id)),
                                    TextFont {
                                        font_size: FontSize::Px(11.0),
                                        ..default()
                                    },
                                    TextColor(if sel { fg_dark } else { fg_dim }),
                                ));
                            });
                    }
                    spawn_back_btn(panel, "< BACK TO TOP (Esc)", bg_row, fg_cyan);
                }
                GarageScreen::SortieConfig => {
                    let (_, target_label) = SORTIE_TARGETS[state.sortie_target_idx];
                    let (_, map_label) = SORTIE_MAPS[state.sortie_map_idx];

                    // Row 0: Target Set
                    let sel0 = state.cursor == 0;
                    panel
                        .spawn((
                            Button,
                            UiRow(0),
                            Node {
                                width: Val::Percent(100.0),
                                padding: UiRect::axes(px(10.0), px(8.0)),
                                flex_direction: FlexDirection::Column,
                                row_gap: px(3.0),
                                ..default()
                            },
                            BackgroundColor(if sel0 { bg_sel } else { bg_row }),
                        ))
                        .with_children(|r| {
                            r.spawn((
                                Text::new("TARGET SET (Left / Right)"),
                                TextFont {
                                    font_size: FontSize::Px(11.0),
                                    ..default()
                                },
                                TextColor(if sel0 { fg_dark } else { fg_cyan }),
                            ));
                            r.spawn((
                                Text::new(target_label),
                                TextFont {
                                    font_size: FontSize::Px(14.0),
                                    ..default()
                                },
                                TextColor(if sel0 { fg_dark } else { fg_text }),
                            ));
                        });

                    // Quick prev/next target buttons
                    panel
                        .spawn(Node {
                            width: Val::Percent(100.0),
                            column_gap: px(6.0),
                            ..default()
                        })
                        .with_children(|row| {
                            row.spawn((
                                Button,
                                UiAction::SortieTargetStep(-1),
                                Node {
                                    flex_grow: 1.0,
                                    padding: UiRect::axes(px(8.0), px(4.0)),
                                    justify_content: JustifyContent::Center,
                                    ..default()
                                },
                                BackgroundColor(bg_row),
                            ))
                            .with_children(|b| {
                                b.spawn((
                                    Text::new("< PREV TARGET"),
                                    TextFont {
                                        font_size: FontSize::Px(11.0),
                                        ..default()
                                    },
                                    TextColor(fg_cyan),
                                ));
                            });
                            row.spawn((
                                Button,
                                UiAction::SortieTargetStep(1),
                                Node {
                                    flex_grow: 1.0,
                                    padding: UiRect::axes(px(8.0), px(4.0)),
                                    justify_content: JustifyContent::Center,
                                    ..default()
                                },
                                BackgroundColor(bg_row),
                            ))
                            .with_children(|b| {
                                b.spawn((
                                    Text::new("NEXT TARGET >"),
                                    TextFont {
                                        font_size: FontSize::Px(11.0),
                                        ..default()
                                    },
                                    TextColor(fg_cyan),
                                ));
                            });
                        });

                    // Row 1: Map
                    let sel1 = state.cursor == 1;
                    panel
                        .spawn((
                            Button,
                            UiRow(1),
                            Node {
                                width: Val::Percent(100.0),
                                padding: UiRect::axes(px(10.0), px(8.0)),
                                flex_direction: FlexDirection::Column,
                                row_gap: px(3.0),
                                margin: UiRect::top(px(4.0)),
                                ..default()
                            },
                            BackgroundColor(if sel1 { bg_sel } else { bg_row }),
                        ))
                        .with_children(|r| {
                            r.spawn((
                                Text::new("SORTIE MAP (Left / Right)"),
                                TextFont {
                                    font_size: FontSize::Px(11.0),
                                    ..default()
                                },
                                TextColor(if sel1 { fg_dark } else { fg_cyan }),
                            ));
                            r.spawn((
                                Text::new(map_label),
                                TextFont {
                                    font_size: FontSize::Px(14.0),
                                    ..default()
                                },
                                TextColor(if sel1 { fg_dark } else { fg_text }),
                            ));
                        });

                    panel
                        .spawn(Node {
                            width: Val::Percent(100.0),
                            column_gap: px(6.0),
                            ..default()
                        })
                        .with_children(|row| {
                            row.spawn((
                                Button,
                                UiAction::SortieMapStep(-1),
                                Node {
                                    flex_grow: 1.0,
                                    padding: UiRect::axes(px(8.0), px(4.0)),
                                    justify_content: JustifyContent::Center,
                                    ..default()
                                },
                                BackgroundColor(bg_row),
                            ))
                            .with_children(|b| {
                                b.spawn((
                                    Text::new("< PREV MAP"),
                                    TextFont {
                                        font_size: FontSize::Px(11.0),
                                        ..default()
                                    },
                                    TextColor(fg_cyan),
                                ));
                            });
                            row.spawn((
                                Button,
                                UiAction::SortieMapStep(1),
                                Node {
                                    flex_grow: 1.0,
                                    padding: UiRect::axes(px(8.0), px(4.0)),
                                    justify_content: JustifyContent::Center,
                                    ..default()
                                },
                                BackgroundColor(bg_row),
                            ))
                            .with_children(|b| {
                                b.spawn((
                                    Text::new("NEXT MAP >"),
                                    TextFont {
                                        font_size: FontSize::Px(11.0),
                                        ..default()
                                    },
                                    TextColor(fg_cyan),
                                ));
                            });
                        });

                    // Row 2: Launch Sortie
                    let sel2 = state.cursor == 2;
                    panel
                        .spawn((
                            Button,
                            UiAction::LaunchSortie,
                            Node {
                                width: Val::Percent(100.0),
                                padding: UiRect::axes(px(12.0), px(12.0)),
                                margin: UiRect::top(px(10.0)),
                                justify_content: JustifyContent::Center,
                                border: UiRect::all(px(1.5)),
                                ..default()
                            },
                            BackgroundColor(if sel2 {
                                bg_sel
                            } else {
                                Color::srgba(0.18, 0.36, 0.22, 0.90)
                            }),
                            BorderColor::all(fg_amber),
                        ))
                        .with_children(|b| {
                            b.spawn((
                                Text::new(">>  LAUNCH AC TEST SORTIE"),
                                TextFont {
                                    font_size: FontSize::Px(15.0),
                                    ..default()
                                },
                                TextColor(if sel2 { fg_dark } else { fg_text }),
                            ));
                        });

                    spawn_back_btn(panel, "< BACK TO TOP (Esc)", bg_row, fg_cyan);
                }
                GarageScreen::ViewMode => {}
            }
        });

        // Right AC SPECS telemetry panel (live `AcCtrlParam` calculation + preview delta!).
        let cur_specs = AcSpecs::calculate(&garage.active_design);
        let base_specs = state
            .saved_design
            .as_ref()
            .map(AcSpecs::calculate)
            .unwrap_or(cur_specs);

        p.spawn((
            Node {
                position_type: PositionType::Absolute,
                right: px(20.0),
                top: px(66.0),
                width: px(280.0),
                padding: UiRect::all(px(10.0)),
                flex_direction: FlexDirection::Column,
                row_gap: px(4.0),
                border: UiRect::all(px(1.0)),
                ..default()
            },
            BackgroundColor(bg_panel),
            BorderColor::all(border_col),
        ))
        .with_children(|spec| {
            spec.spawn((
                Text::new("AC SPECS  (AcCtrlParam)"),
                TextFont {
                    font_size: FontSize::Px(13.0),
                    ..default()
                },
                TextColor(fg_cyan),
            ));

            if !cur_specs.buildable {
                spec.spawn((
                    Node {
                        width: Val::Percent(100.0),
                        padding: UiRect::axes(px(6.0), px(3.0)),
                        justify_content: JustifyContent::Center,
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.68, 0.14, 0.12, 0.90)),
                ))
                .with_children(|w| {
                    w.spawn((
                        Text::new("! UNBUILDABLE !"),
                        TextFont {
                            font_size: FontSize::Px(12.0),
                            ..default()
                        },
                        TextColor(Color::WHITE),
                    ));
                });
            }
            if cur_specs.overweight {
                spec.spawn((
                    Node {
                        width: Val::Percent(100.0),
                        padding: UiRect::axes(px(6.0), px(3.0)),
                        justify_content: JustifyContent::Center,
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.68, 0.14, 0.12, 0.90)),
                ))
                .with_children(|w| {
                    w.spawn((
                        Text::new("! OVERWEIGHT !"),
                        TextFont {
                            font_size: FontSize::Px(12.0),
                            ..default()
                        },
                        TextColor(Color::WHITE),
                    ));
                });
            }
            if cur_specs.en_shortage {
                spec.spawn((
                    Node {
                        width: Val::Percent(100.0),
                        padding: UiRect::axes(px(6.0), px(3.0)),
                        justify_content: JustifyContent::Center,
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.72, 0.34, 0.08, 0.90)),
                ))
                .with_children(|w| {
                    w.spawn((
                        Text::new("! EN SHORTAGE !"),
                        TextFont {
                            font_size: FontSize::Px(12.0),
                            ..default()
                        },
                        TextColor(Color::WHITE),
                    ));
                });
            }

            let rows: &[(&str, f32, f32, bool, &str)] = &[
                ("AP", cur_specs.ap, base_specs.ap, true, ""),
                ("KE DEF", cur_specs.ke_def, base_specs.ke_def, true, ""),
                ("CE DEF", cur_specs.ce_def, base_specs.ce_def, true, ""),
                ("TE DEF", cur_specs.te_def, base_specs.te_def, true, ""),
                ("WEIGHT", cur_specs.total_weight, base_specs.total_weight, false, ""),
                ("LOAD", cur_specs.carried_load, base_specs.carried_load, false, ""),
                ("LOAD CAP", cur_specs.leg_payload, base_specs.leg_payload, true, ""),
                ("EN OUTPUT", cur_specs.en_output, base_specs.en_output, true, ""),
                ("EN DRAIN", cur_specs.en_drain, base_specs.en_drain, false, ""),
                ("EN RECOV", cur_specs.en_recovery, base_specs.en_recovery, true, ""),
                ("EN CAP", cur_specs.en_capacity, base_specs.en_capacity, true, ""),
                ("BOOST SPD", cur_specs.boost_speed, base_specs.boost_speed, true, " km/h"),
                ("HB SPEED", cur_specs.hb_speed, base_specs.hb_speed, true, " km/h"),
                ("GLIDE SPD", cur_specs.glide_speed, base_specs.glide_speed, true, " km/h"),
                ("TURN SPD", cur_specs.turn_speed, base_specs.turn_speed, true, " deg/s"),
                ("LOCK RNG", cur_specs.lock_range, base_specs.lock_range, true, " m"),
                ("SCAN TIME", cur_specs.scan_secs, base_specs.scan_secs, false, " s"),
                ("STABILITY", cur_specs.stagger_thr, base_specs.stagger_thr, true, ""),
            ];
            for &(label, val, prev, higher_is_better, unit) in rows {
                let diff = val - prev;
                let is_secs = unit == " s";
                let has_diff = if is_secs {
                    diff.abs() >= 0.01
                } else {
                    diff.abs() >= 0.5
                };
                let val_color = if !has_diff {
                    fg_text
                } else if (diff > 0.0) == higher_is_better {
                    Color::srgb(0.40, 0.94, 0.56)
                } else {
                    Color::srgb(1.0, 0.40, 0.36)
                };
                let formatted = if is_secs {
                    if has_diff {
                        format!("{:.2}{unit} ({:+.2})", val, diff)
                    } else {
                        format!("{:.2}{unit}", val)
                    }
                } else if has_diff {
                    format!("{:.0}{unit} ({:+.0})", val, diff)
                } else {
                    format!("{:.0}{unit}", val)
                };
                spec.spawn(Node {
                    width: Val::Percent(100.0),
                    justify_content: JustifyContent::SpaceBetween,
                    ..default()
                })
                .with_children(|r| {
                    r.spawn((
                        Text::new(label),
                        TextFont {
                            font_size: FontSize::Px(11.5),
                            ..default()
                        },
                        TextColor(fg_dim),
                    ));
                    r.spawn((
                        Text::new(formatted),
                        TextFont {
                            font_size: FontSize::Px(11.5),
                            ..default()
                        },
                        TextColor(val_color),
                    ));
                });
            }
        });

        // Bottom key-guide footer.
        p.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(20.0),
                right: px(20.0),
                bottom: px(14.0),
                height: px(30.0),
                padding: UiRect::axes(px(12.0), px(4.0)),
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(bg_panel),
            BorderColor::all(border_col),
        ))
        .with_children(|foot| {
            foot.spawn((
                Text::new("W/S or Up/Dn: Navigate   |   Enter / Click: Select   |   Esc: Back   |   Right-Drag: Orbit   |   Wheel: Zoom   |   V: View Mode   |   F9: Mute BGM"),
                TextFont {
                    font_size: FontSize::Px(11.5),
                    ..default()
                },
                TextColor(fg_dim),
            ));
            foot.spawn((
                Button,
                UiAction::ToggleViewMode,
                Node {
                    padding: UiRect::axes(px(8.0), px(2.0)),
                    ..default()
                },
                BackgroundColor(bg_row),
            ))
            .with_children(|b| {
                b.spawn((
                    Text::new("VIEW MODE"),
                    TextFont {
                        font_size: FontSize::Px(11.0),
                        ..default()
                    },
                    TextColor(fg_cyan),
                ));
            });
        });
    });
}

fn spawn_back_btn(
    panel: &mut ChildSpawnerCommands,
    label: &str,
    bg: Color,
    fg: Color,
) {
    panel
        .spawn((
            Button,
            UiAction::Back,
            Node {
                width: Val::Percent(100.0),
                padding: UiRect::axes(px(8.0), px(5.0)),
                margin: UiRect::top(px(4.0)),
                ..default()
            },
            BackgroundColor(bg),
        ))
        .with_children(|b| {
            b.spawn((
                Text::new(label),
                TextFont {
                    font_size: FontSize::Px(12.0),
                    ..default()
                },
                TextColor(fg),
            ));
        });
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        out.push_str("...");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES;

    #[test]
    fn all_13_retail_workshops_have_models_and_lighting() {
        let workshops = load_workshops(None);
        assert_eq!(workshops.len(), 13, "expected 13 retail workshops in param/customgarage.bin");
        assert!(workshops.iter().any(|w| w.id == DEFAULT_GARAGE_ID && w.model_id == 1000));
        for ws in &workshops {
            let path = garage_model_path(ws.model_id);
            assert!(
                acvd_data::model(&path).is_some(),
                "workshop {} missing model {path}",
                ws.id
            );
            assert!(
                find(PARAM_GARAGELIGHTING_BIN, u32::from(ws.model_id)).is_some(),
                "workshop {} missing PARAM_GARAGELIGHTING_BIN row {}",
                ws.id,
                ws.model_id
            );
        }
    }

    #[test]
    fn every_assemble_slot_has_parts_and_valid_acsis_display_preset() {
        for slot in AssembleSlot::ALL {
            let parts = parts_for_slot(slot);
            assert!(
                parts.len() > 2,
                "slot {:?} has too few selectable parts ({})",
                slot,
                parts.len()
            );
            assert!(
                find(PARAM_ACSISDISPLAY_BIN, slot.display_id()).is_some(),
                "slot {:?} missing PARAM_ACSISDISPLAY_BIN preset {}",
                slot,
                slot.display_id()
            );
            if slot.allows_empty() {
                assert_eq!(parts[0], 0, "optional slot {:?} should start with (EMPTY)", slot);
            } else {
                assert_ne!(parts[0], 0, "required slot {:?} must not allow 0", slot);
            }
        }
    }

    #[test]
    fn live_part_swap_updates_specs_and_detects_overweight() {
        let (_, rows) = AC_ASSEMBLY_DESIGN_ST_FILES[0];
        let base_row = rows
            .iter()
            .find(|r| r.id == 5001)
            .or_else(|| rows.iter().find(|r| AcCtrlParam::buildable(&r.data)))
            .expect("buildable design");
        let base_specs = AcSpecs::calculate(&base_row.data);
        assert!(base_specs.buildable);
        assert!(!base_specs.overweight, "design 5001 should not be overweight");
        assert!(base_specs.leg_payload > base_specs.carried_load);
        assert!(base_specs.ap > 10_000.0);
        assert!(base_specs.boost_speed > 20.0);

        let mut modified = base_row.data;
        let heads = parts_for_slot(AssembleSlot::Head);
        let alt_head = heads
            .into_iter()
            .find(|&id| id != AssembleSlot::Head.get(&modified))
            .expect("alternate head");
        AssembleSlot::Head.set(&mut modified, alt_head);
        assert_eq!(AssembleSlot::Head.get(&modified), alt_head);
        let alt_specs = AcSpecs::calculate(&modified);
        assert!(alt_specs.buildable);
        assert!(
            (alt_specs.ap - base_specs.ap).abs() > 0.0
                || (alt_specs.total_weight - base_specs.total_weight).abs() > 0.0
        );
    }

    #[test]
    fn partsname_fmg_resolves_english_names_for_every_slot() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let Ok(disc) = Disc::open(&acvd_formats::vfs::default_disc(&root)) else {
            return;
        };
        let bytes = disc
            .asset("lang/en/text/partsname_en.fmg")
            .expect("partsname_en.fmg");
        let fmg = acvd_formats::fmg::read(&bytes).expect("read partsname_en.fmg");
        assert_eq!(part_name(Some(&fmg), 0, 110), "HA-202");
        assert_eq!(part_name(Some(&fmg), 1, 110), "CB-209");
        assert_eq!(part_name(Some(&fmg), 2, 110), "AA-135");
        assert_eq!(part_name(Some(&fmg), 3, 110), "L2MA-131");
        assert_eq!(part_name(Some(&fmg), 4, 110), "FA-108");
        assert_eq!(part_name(Some(&fmg), 5, 110), "Ge-D-G23");
        assert_eq!(part_name(Some(&fmg), 6, 110), "BA-124");
        assert_eq!(part_name(Some(&fmg), 10, 110), "Au-R-F03");
        assert_eq!(part_name(Some(&fmg), 11, 110), "Grind Blade");
        assert_eq!(part_name(Some(&fmg), 12, 110), "TOMOSHIBI mdl.1");
        assert_eq!(part_name(Some(&fmg), 13, 110), "RA-103");
        for slot in AssembleSlot::ALL {
            let parts = parts_for_slot(slot);
            let named = parts
                .iter()
                .filter(|&&id| id > 0 && !part_name(Some(&fmg), slot.category(), id).starts_with("PART #"))
                .count();
            assert!(
                named > 2,
                "slot {:?} should resolve English names in partsname_en.fmg (got {named})",
                slot
            );
        }
    }

    #[test]
    fn acsis_camera_poses_frame_front_sides_and_rear_correctly() {
        let ws = &find(PARAM_CUSTOMGARAGE_BIN, DEFAULT_GARAGE_ID)
            .expect("default workshop")
            .data;
        let bounds = (Vec3::new(-2.0, 0.0, -2.0), Vec3::new(2.0, 6.2, 2.0));
        let top = acsis_camera_pose(DISPLAY_ACSIS_TOP, bounds, ws, false);
        let head = acsis_camera_pose(AssembleSlot::Head.display_id(), bounds, ws, false);
        let legs = acsis_camera_pose(AssembleSlot::Legs.display_id(), bounds, ws, false);
        let rear = acsis_camera_pose(AssembleSlot::OveredWeapon.display_id(), bounds, ws, false);

        assert!(head.focus.y > legs.focus.y, "head camera should focus higher than legs");
        assert!(top.dist <= ws.camera_dist_max as f32);
        // Front camera (`AcsisTop`) sits at +Z (cos(yaw) > 0) facing the AC's front and the `-Z`
        // workshop backdrop; rear camera (`OveredWeapon`) sits at -Z (cos(yaw) < 0).
        assert!(top.yaw.cos() > 0.0, "AcsisTop should look from +Z at front of AC");
        assert!(rear.yaw.cos() < 0.0, "OveredWeapon should look from -Z at rear of AC");
    }
}
