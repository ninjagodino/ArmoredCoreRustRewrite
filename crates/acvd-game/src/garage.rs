//! 3D ACVD Garage (`--garage` or `F1` from AC TEST):
//! - Loads any of the 13 retail 3D garage workshops from `param/customgarage.bin`
//!   (`PARAM_CUSTOMGARAGE_BIN`, `id` 1..=13, default `id = 5` → `menu1000` Underpass):
//!   `model/menu/menu{id:04}/menu{id:04}_m.bnd.dcx|menu{id:04}.flv` + `menu{id:04}.tpf.dcx`.
//! - Lights the bay with `model/map/ingamegarage/ingamegaragemenu{id:04}_env.msb` (360
//!   `0x82306860` / `0x82580080` / `0x825bc318` / `0x825d33b0`): workshop world meshes use light
//!   set `1` (`SET_MAP`) + fog + clustered dynamic `POINT_PARAM_ST` point (`1300`) and spot
//!   (`1310`) lights (`0x82800170` -> `0x82be8218`), sky/clouds use `SkyMaterial`, and the entire
//!   HDR frame passes once through `m_ToneMapFilter` (`0x82c18628`).
//! - Frames the assembled AC with `param/acsisdisplay.bin` (`PARAM_ACSISDISPLAY_BIN`, 360
//!   `0x8257c988` / `0x8257cde8` / `0x8257ce50` / `0x82586400` / `0x82684958`) and
//!   `param/acsisdisplaycommon.bin` (`40.0°` FOV), smoothly sweeping to each part slot on hover
//!   while allowing free mouse-drag orbit and scroll zoom within `CustomGarageSt` bounds.
//! - Interactive Assemble (all 14 ACVD part slots from `param/acvparts.bin` + English names from
//!   `lang/en/text/partsname_en.fmg`), live `AcCtrlParam` spec telemetry with green/red preview
//!   deltas and Overweight / EN Shortage warnings, Paint (200+ `param/accolor/colorXXXX.bin`
//!   presets + live 7-channel customizer + `ca0001..ca0011` camouflage), Preset Designs, Garage
//!   Custom (all 13 workshops live), Options (fullscreen + master volume via `settings.json`),
//!   View Mode, and seamless AC TEST sortie transition with real `sound/acv2_se_menu.fev` cues
//!   and `acv2_bgm_menu_bgm_menu.fev` `grg` BGM.

use std::collections::BTreeSet;
use std::f32::consts::PI;

use acvd_data::generated::ac_unit::AcAssemblyDesignSt;
use acvd_data::generated::assembly::AC_PARTS;
use acvd_data::generated::ctrl::AcCtrlParam;
use acvd_data::generated::menu::{AcsisDisplaySt, PARAM_ACSISDISPLAY_BIN};
use acvd_data::generated::progression::{CustomGarageSt, PARAM_CUSTOMGARAGE_BIN};
#[cfg(test)]
use acvd_data::generated::render::PARAM_GARAGELIGHTING_BIN;
use acvd_data::{ac_part, find};
use acvd_formats::accolor::{
    AcColor, CH_AID, CH_AID2, CH_BASE, CH_JOINT, CH_KOJIMA, CH_OPTIONAL, GROUP_COUNT,
};
use acvd_formats::fmg::Fmg;
use acvd_formats::vfs::Disc;
use acvd_render::ui_kit::{
    palette, AcvdUiKit, ArrowDir, ButtonState, FontRole, PanelStyle, StatDelta, TraceStyle,
};
use acvd_render::Packs;
use bevy::audio::{AudioSink, AudioSource, GlobalVolume};
use bevy::camera::Exposure;
use bevy::ecs::system::SystemParam;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::ui::{px, BackgroundGradient, ColorStop, LinearGradient};

use crate::decal::{self, DecalLibraryTab, DecalState, DECAL_TINTS};
use crate::env;
use crate::paint;
use crate::settings::{self, AudioChannel, GameSettings};
use crate::sound::{self, AudioBus, BgmPlayer, BgmState, Cues};

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

/// Number of decal thumbnails displayed per page in the `PAINT -> DECALS` grid (4 cols × 3 rows).
const DECAL_GRID_PAGE_SIZE: usize = 12;

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

/// Marker for garage workshop meshes that display the player's live AC Emblem
/// (`0x825d3978`: `mtd="emblem"` / diffuse `"im999"` / `"im9999"`). Lets
/// `decal::sync_bakes` rebind `DecalState::emblem_texture` after custom uploads
/// without relying on handle-identity scans of every `EnvMaterial`.
#[derive(Component)]
pub struct GarageEmblemBanner;

/// Marker for the garage UI root hierarchy.
#[derive(Component)]
pub struct GarageUi;

/// Interactive row button tagged with its logical row index in the active screen.
#[derive(Component, Clone, Copy)]
pub struct UiRow(pub usize);

/// Hover-revealed corner `X` delete button on a custom decal/emblem library thumbnail cell.
#[derive(Component, Clone, Copy)]
pub struct CustomThumbDeleteBtn {
    pub cell: Entity,
}

/// Interactive action button in the paint/decal editor and sortie screen.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub enum UiAction {
    Back,
    ListPage(i8),
    ToggleViewMode,
    PaintTab(PaintTab),
    PaintPickerMode(PaintPickerMode),
    PaintSwatch(usize),
    PaintChannel(usize),
    PaintRgbDelta(usize, i16),
    PaintCamoDelta(i8),
    PaintCamoSelect(u8),
    PaintCamoScaleStep(i8),
    PaintCamoUpload,
    PaintCustomCamoSelect(usize),
    PaintCustomCamoDelete(usize),
    PaintExitSave,
    PaintExitDiscard,
    DecalUpload,
    DecalLibTab(DecalLibraryTab),
    DecalLibPage(i8),
    DecalSelectBrush(usize),
    DecalDeleteCustom(usize),
    DecalToggleNewMode,
    DecalSelectLayer(u64),
    DecalLayerMove(u64, i8),
    DecalLayerDelete(u64),
    DecalScaleStep(i8),
    DecalRotateStep(i8),
    DecalOpacityStep(i8),
    DecalToggleMirror,
    DecalToggleFlip,
    DecalTint(usize),
    DecalDuplicate,
    DecalClearAll,
    EmblemSelect(usize),
    EmblemResetDefault,
    EmblemClear,
    SortieMapStep(i8),
    SortieTargetStep(i8),
    LaunchSortie,
    AssembleSelectSlot(AssembleSlot),
    AssembleCatStep(i8),
    AssemblePartStep(i8),
    AssembleSubcat(Option<u8>),
    AssembleDmgFilter(Option<u8>),
    AssembleToggleDetail,
    ToggleFullscreen,
    SelectOptionRow(usize),
    UiScaleStep(i8),
    HudSwayStep(i8),
    VolumeStep(AudioChannel, i8),
    QuitToDesktop,
}

/// Adjustable garage UI control that responds to mouse-wheel scrolling while hovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelAdjust {
    UiScale,
    HudSway,
    Volume(AudioChannel),
    PaintRgb(usize),
    PaintCamo,
    PaintCamoScale,
    DecalScale,
    DecalRotate,
    DecalOpacity,
    SortieMap,
    SortieTarget,
}

impl UiAction {
    /// Maps a hovered `UiAction` widget to its mouse-wheel adjustable target, if any.
    pub fn wheel_adjust(self) -> Option<WheelAdjust> {
        match self {
            Self::SelectOptionRow(1) | Self::UiScaleStep(_) => Some(WheelAdjust::UiScale),
            Self::SelectOptionRow(2) | Self::HudSwayStep(_) => Some(WheelAdjust::HudSway),
            Self::SelectOptionRow(row) => GARAGE_OPTION_CHANNELS
                .iter()
                .find(|&&(r, _)| r == row)
                .map(|&(_, ch)| WheelAdjust::Volume(ch)),
            Self::VolumeStep(ch, _) => Some(WheelAdjust::Volume(ch)),
            Self::PaintRgbDelta(comp, _) => Some(WheelAdjust::PaintRgb(comp)),
            Self::PaintCamoDelta(_) => Some(WheelAdjust::PaintCamo),
            Self::PaintCamoScaleStep(_) => Some(WheelAdjust::PaintCamoScale),
            Self::DecalScaleStep(_) => Some(WheelAdjust::DecalScale),
            Self::DecalRotateStep(_) => Some(WheelAdjust::DecalRotate),
            Self::DecalOpacityStep(_) => Some(WheelAdjust::DecalOpacity),
            Self::SortieMapStep(_) => Some(WheelAdjust::SortieMap),
            Self::SortieTargetStep(_) => Some(WheelAdjust::SortieTarget),
            _ => None,
        }
    }
}

/// Converts a vertical mouse-wheel delta (Bevy: positive Y = scroll up) into a ±1 step direction.
pub fn wheel_step_direction(delta_y: f32) -> i8 {
    if delta_y > 0.0 {
        1
    } else if delta_y < 0.0 {
        -1
    } else {
        0
    }
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
    Options,
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
            "options" | "settings" => Some(Self::Options),
            "view" => Some(Self::ViewMode),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaintTab {
    Presets,
    Custom,
    Decals,
    Emblem,
}

/// Interactive color-picker mode inside `PAINT -> PALETTE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PaintPickerMode {
    #[default]
    Hsv,
    Rgb,
}

/// Click-drag interactive surface inside `PAINT -> PALETTE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaintDragTarget {
    /// 2D Saturation (X) × Value (1 - Y) square.
    HsvSv,
    /// Horizontal Hue (0..360 deg) rainbow bar.
    HsvHue,
    /// Horizontal Red (0..255) gradient bar.
    RgbR,
    /// Horizontal Green (0..255) gradient bar.
    RgbG,
    /// Horizontal Blue (0..255) gradient bar.
    RgbB,
}

/// Tags a UI node as a click-and-drag color picker track.
#[derive(Component, Clone, Copy, Debug)]
pub struct ColorPickerTrack(pub PaintDragTarget);

/// Target value controlled by clicking or dragging on a horizontal slider gauge bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SliderTrackTarget {
    UiScale,
    HudSway,
    Volume(AudioChannel),
    PaintCamoScale,
    DecalScale,
    DecalRotate,
    DecalOpacity,
}

/// Tags a slider row's gauge track node so clicking or dragging anywhere on the bar sets the value.
#[derive(Component, Clone, Copy, Debug)]
pub struct SliderTrack(pub SliderTrackTarget);

/// Cached GPU textures for the HSV 2D SV square, 1D Hue spectrum bar, and 11 retail camo swatches.
#[derive(Resource, Default)]
pub struct PaintPickerAssets {
    pub sv_image: Option<Handle<Image>>,
    pub sv_hue_cached: f32,
    pub hue_image: Option<Handle<Image>>,
    /// Retail camouflage thumbnails indexed by `camo_id` (`1..=11`; index `0` is `None`).
    pub camo_thumbs: [Option<Handle<Image>>; 12],
    pub camo_thumbs_loaded: bool,
}

impl PaintPickerAssets {
    /// Lazily decodes retail camouflage textures `ca0001..ca0011` once for the Palette camo grid.
    pub fn ensure_camo_thumbs(
        &mut self,
        disc: &acvd_formats::vfs::Disc,
        images: &mut Assets<Image>,
    ) {
        if self.camo_thumbs_loaded {
            return;
        }
        self.camo_thumbs_loaded = true;
        let mut packs = acvd_render::Packs::default();
        for camo_id in 1u8..=11 {
            if let Some(handle) = paint::load_camo_map_handle(disc, &mut packs, images, camo_id) {
                self.camo_thumbs[camo_id as usize] = Some(handle);
            }
        }
    }
    // Matches the 354×104 SV box aspect ratio so Bevy's `ImageNode` fills 100% edge-to-edge at all UI scales.
    const SV_W: u32 = 354;
    const SV_H: u32 = 104;
    const HUE_W: u32 = 256;
    const HUE_H: u32 = 12;

    /// Ensures both HSV picker textures exist and refreshes the SV square when `hue_deg` changes.
    pub fn ensure_loaded(&mut self, hue_deg: f32, images: &mut Assets<Image>) {
        let hue = hue_deg.rem_euclid(360.0);
        if self.hue_image.is_none() {
            let mut data = Vec::with_capacity((Self::HUE_W * Self::HUE_H * 4) as usize);
            for _y in 0..Self::HUE_H {
                for x in 0..Self::HUE_W {
                    let h = (x as f32 / (Self::HUE_W - 1) as f32) * 360.0;
                    let c = paint::hsv_to_rgb(h, 1.0, 1.0);
                    data.extend_from_slice(&c);
                }
            }
            let mut img = Image::new(
                Extent3d {
                    width: Self::HUE_W,
                    height: Self::HUE_H,
                    depth_or_array_layers: 1,
                },
                TextureDimension::D2,
                data,
                TextureFormat::Rgba8UnormSrgb,
                default(),
            );
            img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                address_mode_u: ImageAddressMode::ClampToEdge,
                address_mode_v: ImageAddressMode::ClampToEdge,
                mag_filter: ImageFilterMode::Linear,
                min_filter: ImageFilterMode::Linear,
                ..default()
            });
            self.hue_image = Some(images.add(img));
        }

        if self.sv_image.is_none() {
            let mut img = Image::new(
                Extent3d {
                    width: Self::SV_W,
                    height: Self::SV_H,
                    depth_or_array_layers: 1,
                },
                TextureDimension::D2,
                vec![0u8; (Self::SV_W * Self::SV_H * 4) as usize],
                TextureFormat::Rgba8UnormSrgb,
                default(),
            );
            img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                address_mode_u: ImageAddressMode::ClampToEdge,
                address_mode_v: ImageAddressMode::ClampToEdge,
                mag_filter: ImageFilterMode::Linear,
                min_filter: ImageFilterMode::Linear,
                ..default()
            });
            self.sv_image = Some(images.add(img));
            self.sv_hue_cached = -999.0;
        }

        if (hue - self.sv_hue_cached).abs() > 0.2 {
            if let Some(handle) = &self.sv_image {
                if let Some(mut img) = images.get_mut(handle) {
                    let data = img.data.get_or_insert_with(|| {
                        vec![0u8; (Self::SV_W * Self::SV_H * 4) as usize]
                    });
                    data.resize((Self::SV_W * Self::SV_H * 4) as usize, 255);
                    let mut idx = 0;
                    for y in 0..Self::SV_H {
                        let v = 1.0 - (y as f32 / (Self::SV_H - 1) as f32);
                        for x in 0..Self::SV_W {
                            let s = x as f32 / (Self::SV_W - 1) as f32;
                            let c = paint::hsv_to_rgb(hue, s, v);
                            data[idx..idx + 4].copy_from_slice(&c);
                            idx += 4;
                        }
                    }
                    self.sv_hue_cached = hue;
                }
            }
        }
    }
}

/// Top menu choices (order matches `private/lua360/script/garage/garagescene.lua`).
/// Tuple: `(label, right_tag, helpline_description)`.
const TOP_ITEMS: &[(&str, &str, &str)] = &[
    ("ASSEMBLE", "PARTS", "Customize AC frame, internals,\nand weapons."),
    ("PAINT", "COLOR", "Presets, RGB paint, camouflage,\ndecals, and the AC emblem."),
    ("AC DESIGN", "PRESET", "Load any preset AC blueprint\nas a starting base."),
    ("GARAGE CUSTOM", "BAY", "Switch between all 13 retail\n3D ACVD workshops."),
    ("AC TEST", "SORTIE", "Configure targets and sortie\ninto live combat test."),
    ("VIEW MODE", "V", "Hide HUD and inspect your AC\nfreely from any angle."),
    ("OPTIONS", "ESC", "Toggle fullscreen display and\nadjust master volume."),
];

/// Helpline descriptions for [`GarageScreen::Options`] rows.
const OPTIONS_ROW_DESCS: [&str; 9] = [
    "Toggle fullscreen or windowed\ndisplay mode.",
    "Scale HUD and menu size\n(75% - 150%, default 100%).",
    "Adjust crosshair HUD sway\nintensity (0% - 100%).",
    "Adjust master audio volume\nacross all sound channels.",
    "Adjust background music volume\nin Garage and Sortie.",
    "Adjust weapon, booster, and\ncombat sound effects volume.",
    "Adjust menu navigation and\nUI feedback sound volume.",
    "Return to the Garage main menu.",
    "Exit the game and return\nto the desktop.",
];

const GARAGE_OPTION_CHANNELS: [(usize, AudioChannel); 4] = [
    (3, AudioChannel::Master),
    (4, AudioChannel::Music),
    (5, AudioChannel::Sfx),
    (6, AudioChannel::Ui),
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
    /// Active subcategory filter tab in `AssembleParts` (`None` = ALL).
    pub assemble_subcat: Option<u8>,
    /// Active damage-type filter in `AssembleParts` (`None` = ALL, `0` = KE, `6` = CE, `1` = TE).
    pub assemble_dmg_filter: Option<u8>,
    /// `Tab` toggle in `AssembleParts`: `false` = `@PartsNormal` + 3D preview, `true` = `@PartsDetail` + `@SpecDetail`.
    pub assemble_detail_view: bool,
    /// Horizontal scroll offset for `@PartsCategoryList` (7 visible slot tabs).
    pub assemble_cat_scroll: usize,
    /// Debounce timer (seconds) before triggering a full 3D AC rebuild (`garage.dirty = true`)
    /// while scrolling through visible parts in `AssembleParts`.
    pub ac_preview_debounce: Option<f32>,
    /// True once a hovered preview actually triggered a 3D AC rebuild since entering the slot.
    pub ac_preview_rebuilt: bool,
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
    pub paint_picker_mode: PaintPickerMode,
    /// Persistent `[hue_deg (0..360), sat (0..1), val (0..1)]` for the active channel so dragging
    /// `S=0` or `V=0` does not lose the current Hue angle.
    pub paint_hsv: [f32; 3],
    /// Channel index for which `paint_hsv` was last synced (`None` forces a sync from AC color).
    pub paint_hsv_synced_channel: Option<usize>,
    /// Active mouse click-drag target in `PAINT -> PALETTE`.
    pub paint_drag: Option<PaintDragTarget>,
    /// Active click/drag target on any horizontal slider gauge bar (`SliderTrack`), along with
    /// the track's captured `(center_phys, size_phys)` rect at drag start so UI rebuilds never
    /// read a zero-sized un-laid-out `ComputedNode` or jitter while resizing (`UI SCALE`).
    pub slider_drag: Option<(SliderTrackTarget, Vec2, Vec2)>,
    /// Session-scoped undo/redo stack for the Paint menu (cleared when leaving `GarageScreen::Paint`).
    pub paint_history: PaintUndoHistory,
    /// Set to `true` on frames where a garage UI slider/strip consumed the mouse wheel so
    /// `update_camera` (which runs after `sync_ui`) never also zooms the 3D mech camera.
    pub wheel_consumed: bool,
    /// True when the "Save paint changes before exit?" modal prompt is open.
    pub paint_exit_prompt: bool,
    /// Selected choice in the paint exit prompt (`true` = YES / Save, `false` = NO / Discard).
    pub paint_exit_prompt_choice: bool,
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
    /// Persistent vertical pan focus Y in `GarageScreen::Paint` (held until leaving Paint,
    /// switching Paint tabs, or pressing `R`).
    pub cam_pan_y: Option<f32>,
    pub cam_pan_tab: Option<PaintTab>,
    /// Transient status banner shown inside the UI (e.g. "OVERWEIGHT", "EQUIPPED ...").
    pub banner: Option<(String, Color, f32)>,
    /// Last window logical size used to lay out the UI (triggers rebuild on resize).
    pub last_window_size: Vec2,
    /// Last `GameSettings::ui_scale` used to lay out the UI (triggers rebuild on scale change).
    pub last_ui_scale: f32,
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
            assemble_subcat: None,
            assemble_dmg_filter: None,
            assemble_detail_view: false,
            assemble_cat_scroll: 0,
            ac_preview_debounce: None,
            ac_preview_rebuilt: false,
            saved_design: None,
            saved_color: None,
            saved_name: None,
            workshops,
            workshop_idx,
            spawned_workshop: None,
            color_presets,
            paint_tab: PaintTab::Presets,
            paint_channel: CH_BASE,
            paint_picker_mode: PaintPickerMode::Hsv,
            paint_hsv: [0.0, 0.0, 0.5],
            paint_hsv_synced_channel: None,
            paint_drag: None,
            slider_drag: None,
            paint_history: PaintUndoHistory::default(),
            wheel_consumed: false,
            paint_exit_prompt: false,
            paint_exit_prompt_choice: true,
            sortie_map_idx,
            sortie_target_idx,
            parts_fmg,
            cam_yaw: -19.0f32.to_radians(),
            cam_pitch: 0.14,
            cam_dist: 9.8,
            cam_focus: Vec3::new(0.0, 3.2, 0.0),
            cam_initialized: false,
            cam_manual_orbit: false,
            cam_pan_y: None,
            cam_pan_tab: None,
            banner: None,
            last_window_size: Vec2::ZERO,
            last_ui_scale: 1.0,
            ui_dirty: true,
        }
    }

    pub fn active_workshop(&self) -> Option<&GarageWorkshop> {
        self.workshops.get(self.workshop_idx)
    }

    pub fn display_preset_id(&self) -> u32 {
        match self.screen {
            GarageScreen::Top
            | GarageScreen::GarageCustom
            | GarageScreen::SortieConfig
            | GarageScreen::Options => DISPLAY_ACSIS_TOP,
            GarageScreen::ViewMode => DISPLAY_ACSIS_TOP,
            GarageScreen::AssembleSlots => AssembleSlot::ALL
                .get(self.cursor)
                .map(|s| s.display_id())
                .unwrap_or(DISPLAY_ACSIS_TOP),
            GarageScreen::AssembleParts { .. } => DISPLAY_ACSIS_TOP,
            GarageScreen::Paint => match self.paint_tab {
                PaintTab::Presets => DISPLAY_PAINT_TOP,
                PaintTab::Custom | PaintTab::Decals | PaintTab::Emblem => DISPLAY_COLORING_TOP,
            },
            GarageScreen::Design => DISPLAY_DESIGN_DETAIL,
        }
    }

    pub fn set_banner(&mut self, msg: impl Into<String>, color: Color) {
        self.banner = Some((msg.into(), color, 3.0));
        self.ui_dirty = true;
    }
}

/// Ensures the active 3D garage workshop model, `_env.msb` light sets (`SET_AC` + `SET_MAP`),
/// and `POINT_PARAM_ST` dynamic point/spot lights (`0x82be8218`) are spawned whenever
/// `Mode::Garage` is active or the workshop changes.
#[allow(clippy::too_many_arguments)]
pub fn sync_room(
    mut commands: Commands,
    mode: Res<Mode>,
    mut state: ResMut<GarageState>,
    mut decals: ResMut<DecalState>,
    garage: Res<crate::Garage>,
    world: Query<Entity, With<GarageWorld>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut env_materials: ResMut<Assets<env::EnvMaterial>>,
    mut sky_materials: ResMut<Assets<env::SkyMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut dir_lights: Query<(&mut DirectionalLight, &mut Transform)>,
    cameras: Query<
        Entity,
        (
            With<Camera3d>,
            Without<crate::env::WaterReflectionCamera>,
            Without<crate::garage_assemble::PartPreviewCamera>,
        ),
    >,
) {
    decals.init_assets(&mut images);
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
    // Sets ClearColor, SET_AC (slot 0) on the AC lights, HDR Bloom, and fullscreen m_ToneMapFilter.
    let env_file = garage_env_path(&garage.disc, ws.model_id);
    let loaded_env =
        env::apply_env_file(&mut commands, &garage.disc, &env_file, &mut dir_lights, &cameras);
    // 360 `0x825d33b0` -> `0x825d3108(this, 1)` assigns light/fog set 1 (`SET_MAP`) to all
    // garage workshop submeshes.
    let map_light_set = loaded_env
        .as_ref()
        .and_then(|e| e.light(env::SET_MAP).or_else(|| e.light(env::SET_AC)));
    let white_srgb = env::white_srgb_pixel(&mut images);

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
                let mtd_stem = part
                    .mtd
                    .rsplit(['\\', '/'])
                    .next()
                    .unwrap_or(&part.mtd)
                    .trim_end_matches(".mtd")
                    .to_ascii_lowercase();
                let diff_low = part
                    .diffuse
                    .as_deref()
                    .unwrap_or("")
                    .to_ascii_lowercase();

                let is_sky_cloud = mtd_stem.contains("skymulti")
                    || diff_low.contains("cloud")
                    || diff_low.contains("kumo");
                let is_nolight_add = mtd_stem == "map_nolightnofog_add";
                let is_sky = !is_sky_cloud
                    && !is_nolight_add
                    && (mtd_stem.contains("sky")
                        || mtd_stem == "map_nolightnofog"
                        || diff_low.contains("sky"));
                let is_add = !is_nolight_add && mtd_stem.ends_with("_add");
                let is_glow = mtd_stem.contains("_grow")
                    || mtd_stem.contains("_glow")
                    || mtd_stem.contains("carlight");
                let is_alpha_mask = mtd_stem.contains("alphamask")
                    || diff_low.contains("wire")
                    || diff_low.contains("net")
                    || diff_low.contains("fence");
                let is_alpha_blend = !is_alpha_mask
                    && (mtd_stem == "emblem"
                        || mtd_stem.ends_with("_alpha")
                        || mtd_stem.contains("_alpha_")
                        || diff_low.contains("glass")
                        || mtd_stem.contains("glass"));
                let is_specular = mtd_stem.contains("specul") || mtd_stem.contains("env");

                let is_garage_emblem = mtd_stem == "emblem"
                    || diff_low == "emblem"
                    || diff_low == "im999"
                    || diff_low == "im9999";
                let diffuse = if is_garage_emblem && !garage.flat {
                    // 360 `0x825d3978`: overrides `"emblem"`, `"im999"`, `"im9999"` in the garage
                    // FLVER with the player's live AC Emblem texture.
                    Some(decals.emblem_texture.clone())
                } else {
                    part.diffuse
                        .as_deref()
                        .filter(|_| !garage.flat)
                        .and_then(|name| {
                            load_garage_tex(
                                &garage.disc,
                                &mut packs,
                                &primary_tpf,
                                fallback_tpf,
                                name,
                                false,
                            )
                            .map(|img| images.add(img))
                            .or_else(|| {
                                missing += 1;
                                None
                            })
                        })
                };
                let normal = part
                    .bump
                    .as_deref()
                    .filter(|_| !garage.flat && !is_sky && !is_sky_cloud && !is_add && !is_nolight_add)
                    .and_then(|name| {
                        load_garage_tex(
                            &garage.disc,
                            &mut packs,
                            &primary_tpf,
                            fallback_tpf,
                            name,
                            true,
                        )
                        .map(|img| images.add(img))
                    });

                if is_sky || is_sky_cloud || is_nolight_add {
                    let tex = diffuse.unwrap_or_else(|| white_srgb.clone());
                    let (alpha_mode, use_tex_alpha) = if is_nolight_add {
                        (AlphaMode::Add, true)
                    } else if is_sky_cloud {
                        (AlphaMode::Blend, true)
                    } else {
                        (AlphaMode::Opaque, false)
                    };
                    let sky_mat =
                        env::make_sky_material(tex, alpha_mode, false, use_tex_alpha, 1.0);
                    commands.spawn((
                        Mesh3d(meshes.add(part.mesh)),
                        MeshMaterial3d(sky_materials.add(sky_mat)),
                        Transform::IDENTITY,
                        ChildOf(root),
                    ));
                } else if is_add {
                    // Additive volumetric light shafts (`menu1000` 差し込む光 `Ene_Diffuse_Add`)
                    // and additive floor/wall light decals (`Map_Diffuse_Add`).
                    let mat = StandardMaterial {
                        base_color: Color::linear_rgba(0.65, 0.65, 0.65, 1.0),
                        base_color_texture: diffuse,
                        unlit: true,
                        cull_mode: None,
                        alpha_mode: AlphaMode::Add,
                        ..default()
                    };
                    commands.spawn((
                        Mesh3d(meshes.add(part.mesh)),
                        MeshMaterial3d(materials.add(mat)),
                        Transform::IDENTITY,
                        ChildOf(root),
                    ));
                } else if let Some(set) = map_light_set {
                    let is_glass = diff_low.contains("glass") || mtd_stem.contains("glass");
                    let alpha_mode = if is_alpha_blend {
                        AlphaMode::Blend
                    } else if is_alpha_mask {
                        AlphaMode::Mask(0.45)
                    } else {
                        AlphaMode::Opaque
                    };
                    let base = StandardMaterial {
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
                        perceptual_roughness: if is_glass {
                            0.18
                        } else if is_specular {
                            0.52
                        } else {
                            0.82
                        },
                        metallic: if is_specular { 0.18 } else { 0.05 },
                        cull_mode: if is_alpha_mask || is_alpha_blend {
                            None
                        } else {
                            Some(bevy::render::render_resource::Face::Back)
                        },
                        alpha_mode,
                        ..default()
                    };
                    let emissive_boost = if is_glow { 1.6 } else { 0.0 };
                    let env_mat = env::make_env_material(
                        base,
                        set,
                        &white_srgb,
                        true,
                        emissive_boost,
                    );
                    let mut entity = commands.spawn((
                        Mesh3d(meshes.add(part.mesh)),
                        MeshMaterial3d(env_materials.add(env_mat)),
                        Transform::IDENTITY,
                        ChildOf(root),
                    ));
                    if is_garage_emblem {
                        entity.insert(GarageEmblemBanner);
                    }
                } else {
                    let mat = StandardMaterial {
                        base_color: if diffuse.is_some() {
                            Color::WHITE
                        } else {
                            Color::srgb(0.32, 0.34, 0.38)
                        },
                        base_color_texture: diffuse,
                        normal_map_texture: normal,
                        flip_normal_map_y: false,
                        perceptual_roughness: 0.78,
                        metallic: 0.12,
                        ..default()
                    };
                    let mut entity = commands.spawn((
                        Mesh3d(meshes.add(part.mesh)),
                        MeshMaterial3d(materials.add(mat)),
                        Transform::IDENTITY,
                        ChildOf(root),
                    ));
                    if is_garage_emblem {
                        entity.insert(GarageEmblemBanner);
                    }
                }
                drawn += 1;
            }
            info!(
                "garage menu{:04} ({}): {} submeshes drawn ({} textures missing)",
                ws.model_id, ws.theme, drawn, missing
            );
        }
        Err(e) => warn!("garage {flv_path}: {e:#}"),
    }

    // 3. Spawn the workshop's real 360 `POINT_PARAM_ST` PointLights (`1300`) and SpotLights
    // (`1310`) from `ingamegaragemenuXXXX_env.msb` (`0x82800170` -> `0x82be8218`).
    let inv_exposure = 1.0 / Exposure::default().exposure();
    let mut spawned_dyn = 0usize;

    if let Some(ref e) = loaded_env {
        for sp in &e.spot_lights {
            if !sp.enabled || sp.far <= 0.1 {
                continue;
            }
            let c = env::conv(sp.diffuse) * sp.scale.max(0.0);
            let peak = c.max_element();
            if peak <= 1e-4 {
                continue;
            }
            let pos = Vec3::new(-sp.position[0], sp.position[1], sp.position[2]);
            let d = sp.direction();
            let dir = Vec3::new(-d[0], d[1], d[2]).normalize_or(Vec3::NEG_Y);
            let up = if dir.y.abs() > 0.98 { Vec3::Z } else { Vec3::Y };
            let outer = (sp.fov_y_deg.max(sp.fov_x_deg) * 0.5)
                .to_radians()
                .clamp(0.18, std::f32::consts::FRAC_PI_2 - 0.05);
            let inner = (sp.fov_y_deg.min(sp.fov_x_deg) * 0.32)
                .to_radians()
                .min(outer * 0.85);
            // Match 360 linear-range attenuation `(1 - d/far)^falloff` at the AC working distance
            // (~55% of `far`) to Bevy's inverse-square lumens.
            let atten_ref = 0.25_f32.powf(sp.falloff.clamp(0.25, 4.0));
            let d_ref = (sp.far * 0.55).max(3.0);
            let intensity =
                (peak * atten_ref * 4.0 * PI * PI * d_ref * d_ref * inv_exposure).min(60_000_000.0);
            commands.spawn((
                GarageWorld,
                SpotLight {
                    color: Color::linear_rgb(c.x / peak, c.y / peak, c.z / peak),
                    intensity,
                    range: (sp.far * 1.5).max(16.0),
                    outer_angle: outer,
                    inner_angle: inner,
                    shadow_maps_enabled: true,
                    ..default()
                },
                Transform::from_translation(pos).looking_to(dir, up),
            ));
            spawned_dyn += 1;
        }

        for pt in &e.point_lights {
            if !pt.enabled || pt.radius <= 0.1 {
                continue;
            }
            let c = env::conv(pt.diffuse) * pt.scale.max(0.0);
            let peak = c.max_element();
            if peak <= 1e-4 {
                continue;
            }
            // Match 360 normalized-radius falloff `(1 - d/R)^falloff` without letting Bevy's
            // `1/d^2` inverse-square law blow out surfaces close to the emitter.
            let atten_ref = 0.65_f32.powf(pt.falloff.clamp(0.25, 4.0));
            let d_ref = (pt.radius * 0.22).clamp(1.5, 8.0);
            let intensity =
                (peak * atten_ref * 4.0 * PI * PI * d_ref * d_ref * inv_exposure).min(12_000_000.0);
            let mut pos = Vec3::new(-pt.position[0], pt.position[1], pt.position[2]);
            // 360's `(1 - d/R)^p` point lights have no `1/d^2` singularity, so artists sometimes
            // place bay fill lights inside the AC's waist (e.g. `menu0310` at `(0.42, 3.02, 0.95)`).
            // Nudge emitters that fall inside the AC torso radius out to a safe stand-off along
            // the same direction so they illuminate the bay smoothly without a close-contact flare.
            let ac_center = Vec3::new(0.0, 4.2, 0.0);
            let from_ac = pos - ac_center;
            if from_ac.length() < 2.6 {
                let dir = from_ac.normalize_or(Vec3::new(0.0, 0.2, 1.0));
                pos = ac_center + dir * 2.8;
            }
            commands.spawn((
                GarageWorld,
                PointLight {
                    color: Color::linear_rgb(c.x / peak, c.y / peak, c.z / peak),
                    intensity,
                    range: (pt.radius * 1.15).max(4.0),
                    radius: (pt.radius * 0.12).clamp(0.6, 2.5),
                    shadow_maps_enabled: false,
                    ..default()
                },
                Transform::from_translation(pos),
            ));
            spawned_dyn += 1;
        }
    }

    if spawned_dyn == 0 {
        // Fallback overhead bay key light only when an env MSB defines no `POINT_PARAM_ST` lights.
        commands.spawn((
            GarageWorld,
            SpotLight {
                color: Color::srgb(0.95, 0.97, 1.0),
                intensity: 14_000_000.0,
                range: 50.0,
                outer_angle: 0.72,
                inner_angle: 0.35,
                shadow_maps_enabled: true,
                ..default()
            },
            Transform::from_xyz(0.0, 18.0, 2.0).looking_at(Vec3::new(0.0, 4.0, 0.0), Vec3::Y),
        ));
    }
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

/// True when the cursor sits over `@PartsCategoryList`, `@PartsList`, or the `PARTS VIEW`
/// preview panel so the mouse wheel drives those controls instead of zooming the garage camera.
fn assemble_ui_owns_wheel(state: &GarageState, window: Option<&Window>) -> bool {
    if !matches!(state.screen, GarageScreen::AssembleParts { .. }) {
        return false;
    }
    let Some(window) = window else {
        return false;
    };
    let Some(cursor) = window.cursor_position() else {
        return false;
    };
    let win_size = Vec2::new(window.width(), window.height());
    crate::garage_assemble::hovered_scroll_strip(win_size, cursor).is_some()
        || (!state.assemble_detail_view
            && crate::garage_assemble::cursor_in_part_preview_panel(win_size, cursor))
}

/// Drives the smooth `PARAM_ACSISDISPLAY_BIN` garage camera + free mouse/gamepad orbit & zoom.
#[allow(clippy::too_many_arguments)]
pub fn update_camera(
    mode: Res<Mode>,
    time: Res<Time>,
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    gamepads: Query<&Gamepad>,
    garage: Res<crate::Garage>,
    mut state: ResMut<GarageState>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    action_buttons: Query<(&Interaction, &UiAction)>,
    mut cams: Query<
        (&mut Transform, &mut Projection),
        (
            With<Camera3d>,
            Without<crate::env::WaterReflectionCamera>,
            Without<crate::garage_assemble::PartPreviewCamera>,
        ),
    >,
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

    // In `AssembleParts`, the main garage scene is the blurred background while mouse/stick
    // orbit and zoom drive the right-side `ASSEMBLY VIEW` preview AC (or left `PARTS VIEW`).
    let in_assemble = matches!(state.screen, GarageScreen::AssembleParts { .. });
    let allow_left_drag = state.screen == GarageScreen::ViewMode;
    let dragging = !in_assemble
        && (buttons.pressed(MouseButton::Right)
            || (allow_left_drag && buttons.pressed(MouseButton::Left)));
    if dragging && motion.delta.length_squared() > 0.0 {
        state.cam_manual_orbit = true;
        state.cam_yaw -= motion.delta.x * 0.006;
        state.cam_pitch = (state.cam_pitch + motion.delta.y * 0.005).clamp(-0.35, 1.15);
    }
    // Clear vertical Paint pan when exiting Paint or switching Paint tabs.
    if state.screen != GarageScreen::Paint
        || (state.cam_pan_y.is_some() && state.cam_pan_tab != Some(state.paint_tab))
    {
        state.cam_pan_y = None;
        state.cam_pan_tab = None;
    }

    // In Paint mode, holding middle mouse pans the camera focus vertically along the mech's body
    // and holds that height until exiting Paint or switching tabs.
    let mut panning_y = false;
    if state.screen == GarageScreen::Paint
        && buttons.pressed(MouseButton::Middle)
        && motion.delta.y.abs() > 0.0
    {
        panning_y = true;
        state.cam_manual_orbit = true;
        let pan_scale = 0.008 * (state.cam_dist / 8.0).clamp(0.4, 2.0);
        let min_y = garage.bounds.0.y.min(0.2);
        let max_y = garage.bounds.1.y.max(2.0);
        let base_y = state.cam_pan_y.unwrap_or(state.cam_focus.y);
        let next_y = (base_y + motion.delta.y * pan_scale).clamp(min_y, max_y);
        state.cam_pan_y = Some(next_y);
        state.cam_pan_tab = Some(state.paint_tab);
        state.cam_focus.y = next_y;
    }
    if !in_assemble {
        let mut right_stick = Vec2::ZERO;
        for pad in &gamepads {
            right_stick += pad.right_stick();
        }
        let s = &mut *state;
        if apply_right_stick_orbit(
            right_stick,
            time.delta_secs(),
            &mut s.cam_yaw,
            &mut s.cam_pitch,
        ) {
            s.cam_manual_orbit = true;
        }
    }
    let decal_wheel_mod = state.screen == GarageScreen::Paint
        && state.paint_tab == PaintTab::Decals
        && (keys.pressed(KeyCode::ShiftLeft)
            || keys.pressed(KeyCode::ShiftRight)
            || keys.pressed(KeyCode::ControlLeft)
            || keys.pressed(KeyCode::ControlRight));
    let hover_wheel_adjust = action_buttons.iter().any(|(interaction, action)| {
        matches!(*interaction, Interaction::Hovered | Interaction::Pressed)
            && action.wheel_adjust().is_some()
    });
    let ui_owns_wheel = in_assemble
        || state.wheel_consumed
        || assemble_ui_owns_wheel(&state, windows.single().ok())
        || hover_wheel_adjust;
    if !decal_wheel_mod && !ui_owns_wheel && scroll.delta.y.abs() > 0.0 {
        state.cam_manual_orbit = true;
        let max_d = (ws_row.camera_dist_max as f32).max(18.0);
        state.cam_dist = (state.cam_dist * (1.0 - scroll.delta.y * 0.09)).clamp(3.2, max_d);
    }

    let mut desired_focus = target.focus;
    if let Some(pan_y) = state.cam_pan_y {
        desired_focus.y = pan_y;
    }

    if !state.cam_initialized {
        state.cam_yaw = target.yaw;
        state.cam_pitch = target.pitch;
        state.cam_dist = target.dist;
        state.cam_focus = desired_focus;
        state.cam_initialized = true;
    } else if !state.cam_manual_orbit {
        let t = 1.0 - (-8.0 * time.delta_secs()).exp();
        let dyaw = shortest_angle(state.cam_yaw, target.yaw);
        state.cam_yaw += dyaw * t;
        state.cam_pitch += (target.pitch - state.cam_pitch) * t;
        state.cam_dist += (target.dist - state.cam_dist) * t;
        state.cam_focus = state.cam_focus.lerp(desired_focus, t);
    } else if !panning_y {
        // Smoothly align focus X/Z (and Y when not panned) if the AC height changed.
        let t = 1.0 - (-6.0 * time.delta_secs()).exp();
        state.cam_focus = state.cam_focus.lerp(desired_focus, t);
    }

    let min_y = (ws_row.camera_min_height as f32 * 0.01).clamp(0.8, 4.5);
    let offset = Vec3::new(
        state.cam_yaw.sin() * state.cam_pitch.cos(),
        state.cam_pitch.sin(),
        state.cam_yaw.cos() * state.cam_pitch.cos(),
    ) * state.cam_dist;
    let focus = state.cam_focus;
    let mut eye = focus + offset;
    if eye.y < min_y {
        eye.y = min_y;
    }
    *xf = Transform::from_translation(eye).looking_at(focus, Vec3::Y);
}

fn apply_right_stick_orbit(stick: Vec2, dt: f32, yaw: &mut f32, pitch: &mut f32) -> bool {
    const DEADZONE: f32 = 0.15;
    const YAW_RAD_PER_SEC: f32 = 2.2;
    const PITCH_RAD_PER_SEC: f32 = 1.5;

    let len = stick.length();
    if len <= DEADZONE || dt <= 0.0 {
        return false;
    }
    let mag = ((len - DEADZONE) / (1.0 - DEADZONE)).min(1.0);
    let dir = (stick / len) * mag;
    *yaw -= dir.x * YAW_RAD_PER_SEC * dt;
    *pitch = (*pitch - dir.y * PITCH_RAD_PER_SEC * dt).clamp(-0.35, 1.15);
    true
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

/// Bundles Options-menu resources so `handle_input` stays within Bevy's SystemParam limit.
#[derive(SystemParam)]
pub struct OptionsCtx<'w, 's> {
    settings: ResMut<'w, GameSettings>,
    global_volume: ResMut<'w, GlobalVolume>,
    bgm: Res<'w, BgmState>,
    sinks: Query<'w, 's, (Entity, &'static mut AudioSink)>,
    bgm_players: Query<'w, 's, Entity, With<BgmPlayer>>,
    audio_buses: Query<'w, 's, &'static AudioBus>,
    windows: Query<'w, 's, &'static mut Window, With<bevy::window::PrimaryWindow>>,
    app_exit: MessageWriter<'w, AppExit>,
    custom_camos: ResMut<'w, paint::CustomCamoLibrary>,
}

/// Handles keyboard, gamepad, and mouse-button interaction in the 3D Garage.
#[allow(clippy::too_many_arguments)]
pub fn handle_input(
    mut commands: Commands,
    mode: Res<Mode>,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    scroll: Res<AccumulatedMouseScroll>,
    gamepads: Query<&Gamepad>,
    mut state: ResMut<GarageState>,
    mut garage: ResMut<crate::Garage>,
    mut options: OptionsCtx,
    mut decals: ResMut<DecalState>,
    mut images: ResMut<Assets<Image>>,
    mut cues: ResMut<Cues>,
    mut audio_assets: ResMut<Assets<AudioSource>>,
    mut mode_switch: ResMut<ModeSwitch>,
    row_buttons: Query<(&Interaction, &UiRow), Changed<Interaction>>,
    action_buttons: Query<(Ref<Interaction>, &UiAction)>,
) {
    if *mode != Mode::Garage {
        return;
    }

    state.wheel_consumed = false;

    if let Some((_, _, ref mut ttl)) = state.banner {
        *ttl -= time.delta_secs();
        if *ttl <= 0.0 {
            state.banner = None;
            state.ui_dirty = true;
        }
    }

    // Flush debounced 3D AC preview rebuild once the user pauses scrolling parts.
    if let Some(ref mut rem) = state.ac_preview_debounce {
        *rem -= time.delta_secs();
        if *rem <= 0.0 {
            state.ac_preview_debounce = None;
            state.ac_preview_rebuilt = true;
            garage.dirty = true;
        }
    }

    let mut gp_up = false;
    let mut gp_down = false;
    let mut gp_left = false;
    let mut gp_right = false;
    let mut gp_confirm = false;
    let mut gp_cancel = false;
    let mut gp_view = false;
    let mut gp_start = false;
    for gp in &gamepads {
        gp_up |= gp.just_pressed(GamepadButton::DPadUp);
        gp_down |= gp.just_pressed(GamepadButton::DPadDown);
        gp_left |= gp.just_pressed(GamepadButton::DPadLeft);
        gp_right |= gp.just_pressed(GamepadButton::DPadRight);
        gp_confirm |= gp.just_pressed(GamepadButton::South);
        gp_cancel |= gp.just_pressed(GamepadButton::East);
        gp_view |= gp.just_pressed(GamepadButton::North);
        gp_start |= gp.just_pressed(GamepadButton::Start);
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

    // Poll any pending custom camouflage file-picker result.
    if let Some(res) = options.custom_camos.poll_picker(&mut images) {
        match res {
            Ok(_) => {
                let _ = ensure_custom_color(&mut garage);
                garage.dirty = true;
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_EQUIP);
                let lbl = options
                    .custom_camos
                    .active_label()
                    .unwrap_or("CUSTOM")
                    .to_string();
                state.set_banner(
                    format!("UPLOADED CUSTOM CAMO: {lbl}"),
                    Color::srgb(0.45, 0.92, 0.62),
                );
                state.ui_dirty = true;
            }
            Err(e) => {
                warn!("custom camo import failed: {e:#}");
                state.set_banner(
                    "FAILED TO LOAD CAMO IMAGE",
                    Color::srgb(0.96, 0.40, 0.36),
                );
            }
        }
    }

    // `R` resets manual camera orbit back to the active `AcsisDisplaySt` preset.
    if keys.just_pressed(KeyCode::KeyR) {
        state.cam_manual_orbit = false;
        state.cam_pan_y = None;
        state.cam_pan_tab = None;
    }

    // Paint menu session undo / redo (`Ctrl+Z` / `Ctrl+Shift+Z`).
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    if state.screen == GarageScreen::Paint && ctrl && keys.just_pressed(KeyCode::KeyZ) {
        let target = if shift {
            state.paint_history.redo().cloned()
        } else {
            state.paint_history.undo().cloned()
        };
        if let Some(snap) = target {
            snap.apply(&mut garage, &mut decals);
            state.paint_hsv_synced_channel = None;
            state.ui_dirty = true;
            let cue = if shift { CUE_MOVE } else { CUE_CANCEL };
            sound::cue(&mut commands, &mut cues, &mut audio_assets, cue);
        }
        return;
    }

    // Mouse button clicks on UI action widgets.
    // Defer container row selection (`SelectOptionRow`) so child `<` / `>` arrow buttons
    // inside a slider row always take priority when clicked.
    let mut fallback_select_row: Option<usize> = None;
    for (interaction, action) in &action_buttons {
        if !interaction.is_changed() || *interaction != Interaction::Pressed {
            continue;
        }
        match *action {
            UiAction::Back => {
                trigger_cancel(&mut commands, &mut state, &mut garage, &mut cues, &mut audio_assets);
                return;
            }
            UiAction::ListPage(delta) => {
                let count = current_list_len(&state, &garage);
                if count > LIST_WINDOW {
                    let next_scroll = step_list_page(state.scroll, count, LIST_WINDOW, delta);
                    state.scroll = next_scroll;
                    state.cursor = next_scroll;
                    state.cam_manual_orbit = false;
                    apply_cursor_preview(&mut state, &mut garage);
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                    state.ui_dirty = true;
                }
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
                    state.paint_drag = None;
                    state.paint_hsv_synced_channel = None;
                    state.cursor = 0;
                    state.scroll = 0;
                    state.cam_manual_orbit = false;
                    // Emblem tab defaults to the 360 catalog when the active emblem is retail.
                    if tab == PaintTab::Emblem
                        && decals
                            .ac_emblem_key
                            .as_deref()
                            .is_some_and(|k| k.starts_with("emblem:"))
                    {
                        decals.library_tab = DecalLibraryTab::Emblems;
                    }
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_CATEGORY);
                    state.ui_dirty = true;
                }
                return;
            }
            UiAction::PaintPickerMode(pmode) => {
                if state.paint_picker_mode != pmode {
                    state.paint_picker_mode = pmode;
                    state.paint_drag = None;
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                    state.ui_dirty = true;
                }
                return;
            }
            UiAction::PaintChannel(ch) => {
                state.paint_channel = ch.min(6);
                state.paint_hsv = paint::rgb_to_hsv(current_channel_color(&garage, state.paint_channel));
                state.paint_hsv_synced_channel = Some(state.paint_channel);
                state.paint_drag = None;
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                state.ui_dirty = true;
                return;
            }
            UiAction::PaintSwatch(idx) => {
                if let Some(&(_, rgba)) = PAINT_SWATCHES.get(idx) {
                    apply_custom_channel_color(&mut garage, state.paint_channel, rgba);
                    state.paint_hsv = paint::rgb_to_hsv(rgba);
                    state.paint_hsv_synced_channel = Some(state.paint_channel);
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_EQUIP);
                    state.ui_dirty = true;
                }
                return;
            }
            UiAction::PaintRgbDelta(comp, delta) => {
                if state.screen == GarageScreen::Options {
                    options.settings.hud_color[comp] =
                        (options.settings.hud_color[comp] as i16 + delta).clamp(0, 255) as u8;
                    options.settings.save();
                    let hc = options.settings.hud_color;
                    state.paint_hsv = paint::rgb_to_hsv([hc[0], hc[1], hc[2], 255]);
                    state.paint_hsv_synced_channel = Some(usize::MAX - 1);
                } else if state.paint_tab == PaintTab::Decals {
                    let mut c = current_decal_tint(&decals);
                    c[comp] = (c[comp] as i16 + delta).clamp(0, 255) as u8;
                    apply_decal_tint_color(&mut decals, c);
                    state.paint_hsv = paint::rgb_to_hsv(c);
                    state.paint_hsv_synced_channel = Some(usize::MAX);
                } else {
                    step_custom_channel_rgb(&mut garage, state.paint_channel, comp, delta);
                    state.paint_hsv = paint::rgb_to_hsv(current_channel_color(&garage, state.paint_channel));
                    state.paint_hsv_synced_channel = Some(state.paint_channel);
                }
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                state.ui_dirty = true;
                return;
            }
            UiAction::PaintCamoDelta(delta) => {
                options.custom_camos.active_key = None;
                step_custom_camo(&mut garage, delta);
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_EQUIP);
                state.ui_dirty = true;
                return;
            }
            UiAction::PaintCamoSelect(camo_id) => {
                options.custom_camos.active_key = None;
                let col = ensure_custom_color(&mut garage);
                let id = camo_id.min(11);
                col.camo.fill(id);
                garage.dirty = true;
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_EQUIP);
                state.ui_dirty = true;
                return;
            }
            UiAction::PaintCamoScaleStep(delta) => {
                if delta == 0 {
                    continue;
                }
                options.custom_camos.step_custom_scale(delta);
                if options.custom_camos.active_key.is_some() {
                    garage.dirty = true;
                }
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                state.ui_dirty = true;
                return;
            }
            UiAction::PaintCamoUpload => {
                if options.custom_camos.open_file_picker() {
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_OK);
                    state.set_banner(
                        "SELECT A CAMO IMAGE (PNG/JPG/WEBP/BMP/GIF)...",
                        Color::srgb(0.38, 0.86, 0.96),
                    );
                }
                return;
            }
            UiAction::PaintCustomCamoSelect(idx) => {
                if let Some(asset) = options.custom_camos.assets.get(idx) {
                    let key = asset.key.clone();
                    let label = asset.label.clone();
                    options.custom_camos.active_key = Some(key);
                    let _ = ensure_custom_color(&mut garage);
                    garage.dirty = true;
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_EQUIP);
                    state.set_banner(
                        format!("APPLIED CUSTOM CAMO: {label}"),
                        Color::srgb(0.45, 0.92, 0.62),
                    );
                    state.ui_dirty = true;
                }
                return;
            }
            UiAction::PaintCustomCamoDelete(idx) => {
                if let Some(label) = options.custom_camos.delete_custom_camo(idx) {
                    garage.dirty = true;
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_CANCEL);
                    state.set_banner(
                        format!("REMOVED CUSTOM CAMO: {label}"),
                        Color::srgb(0.96, 0.45, 0.36),
                    );
                    state.ui_dirty = true;
                }
                return;
            }
            UiAction::PaintExitSave => {
                finish_paint_exit_save(&mut commands, &mut state, &mut cues, &mut audio_assets);
                return;
            }
            UiAction::PaintExitDiscard => {
                finish_paint_exit_discard(
                    &mut commands,
                    &mut state,
                    &mut garage,
                    &mut cues,
                    &mut audio_assets,
                );
                return;
            }
            UiAction::DecalUpload => {
                if decals.open_file_picker() {
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_OK);
                    state.set_banner(
                        "SELECT AN IMAGE FILE (PNG/JPG/WEBP/BMP/GIF)...",
                        Color::srgb(0.38, 0.86, 0.96),
                    );
                }
                return;
            }
            UiAction::DecalLibTab(tab) => {
                if decals.library_tab != tab {
                    decals.library_tab = tab;
                    decals.library_page = 0;
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_CATEGORY);
                    state.ui_dirty = true;
                }
                return;
            }
            UiAction::DecalLibPage(delta) => {
                let total = match decals.library_tab {
                    DecalLibraryTab::Custom => decals.custom_assets.len(),
                    DecalLibraryTab::Emblems => decals.disc_emblems.len(),
                };
                let pages = total.div_ceil(DECAL_GRID_PAGE_SIZE).max(1);
                decals.library_page =
                    (decals.library_page as isize + delta as isize).rem_euclid(pages as isize) as usize;
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                state.ui_dirty = true;
                return;
            }
            UiAction::DecalSelectBrush(idx) => {
                let key = match decals.library_tab {
                    DecalLibraryTab::Custom => decals.custom_assets.get(idx).map(|a| a.key.clone()),
                    DecalLibraryTab::Emblems => {
                        let k = decals.disc_emblems.get(idx).map(|e| e.key.clone());
                        if let Some(ref key) = k {
                            let _ = decals.ensure_emblem(&garage.disc, key, &mut images);
                        }
                        k
                    }
                };
                if let Some(key) = key {
                    decals.active_image_key = Some(key);
                    decals.place_new_mode = true;
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                    state.set_banner(
                        "CLICK & DRAG ON THE AC TO PLACE DECAL",
                        Color::srgb(0.38, 0.86, 0.96),
                    );
                }
                return;
            }
            UiAction::DecalDeleteCustom(idx) => {
                if let Some(label) = decals.delete_custom_asset(idx) {
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_CANCEL);
                    state.set_banner(
                        format!("REMOVED CUSTOM IMAGE: {label}"),
                        Color::srgb(0.96, 0.45, 0.36),
                    );
                    state.ui_dirty = true;
                }
                return;
            }
            UiAction::DecalToggleNewMode => {
                decals.place_new_mode = !decals.place_new_mode;
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                state.ui_dirty = true;
                return;
            }
            UiAction::DecalSelectLayer(id) => {
                decals.selected_id = Some(id);
                decals.place_new_mode = false;
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                state.ui_dirty = true;
                return;
            }
            UiAction::DecalLayerMove(id, delta) => {
                if let Some(pos) = decals.layers.iter().position(|l| l.id == id) {
                    let target = (pos as isize + delta as isize)
                        .clamp(0, decals.layers.len().saturating_sub(1) as isize)
                        as usize;
                    if target != pos {
                        let item = decals.layers.remove(pos);
                        let (slot, mirror) = (item.slot, item.mirror_x);
                        decals.layers.insert(target, item);
                        decals.mark_decal_slots_dirty(slot, mirror, true);
                        if let Some(did) = decals.loaded_design_id {
                            decals.save_for_design(did);
                        }
                        sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                        state.ui_dirty = true;
                    }
                }
                return;
            }
            UiAction::DecalLayerDelete(id) => {
                if let Some(pos) = decals.layers.iter().position(|l| l.id == id) {
                    let removed = decals.layers.remove(pos);
                    if decals.selected_id == Some(id) {
                        decals.selected_id = decals.layers.last().map(|l| l.id);
                    }
                    decals.mark_decal_slots_dirty(removed.slot, removed.mirror_x, true);
                    if let Some(did) = decals.loaded_design_id {
                        decals.save_for_design(did);
                    }
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_CANCEL);
                    state.ui_dirty = true;
                }
                return;
            }
            UiAction::DecalScaleStep(delta) => {
                if delta == 0 {
                    continue;
                }
                if let Some(d) = decals.selected_layer_mut() {
                    d.scale = (d.scale + delta as f32 * 0.08).clamp(0.08, 2.5);
                    let (slot, mirror, sc) = (d.slot, d.mirror_x, d.scale);
                    decals.brush_scale = sc;
                    decals.mark_decal_slots_dirty(slot, mirror, true);
                    if let Some(did) = decals.loaded_design_id {
                        decals.save_for_design(did);
                    }
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                    state.ui_dirty = true;
                } else {
                    decals.brush_scale = (decals.brush_scale + delta as f32 * 0.08).clamp(0.08, 2.5);
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                    state.ui_dirty = true;
                }
                return;
            }
            UiAction::DecalRotateStep(delta) => {
                if delta == 0 {
                    continue;
                }
                if let Some(d) = decals.selected_layer_mut() {
                    d.rotation = (d.rotation + (delta as f32 * 15.0).to_radians())
                        .rem_euclid(std::f32::consts::TAU);
                    let (slot, mirror) = (d.slot, d.mirror_x);
                    decals.mark_decal_slots_dirty(slot, mirror, true);
                    if let Some(did) = decals.loaded_design_id {
                        decals.save_for_design(did);
                    }
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                    state.ui_dirty = true;
                }
                return;
            }
            UiAction::DecalOpacityStep(delta) => {
                if delta == 0 {
                    continue;
                }
                if let Some(d) = decals.selected_layer_mut() {
                    d.opacity = (d.opacity + delta as f32 * 0.1).clamp(0.1, 1.0);
                    let (slot, mirror) = (d.slot, d.mirror_x);
                    decals.mark_decal_slots_dirty(slot, mirror, true);
                    if let Some(did) = decals.loaded_design_id {
                        decals.save_for_design(did);
                    }
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                    state.ui_dirty = true;
                }
                return;
            }
            UiAction::DecalToggleMirror => {
                if let Some(d) = decals.selected_layer_mut() {
                    let slot = d.slot;
                    d.mirror_x = !d.mirror_x;
                    decals.brush_mirror_x = d.mirror_x;
                    decals.mark_slot_dirty(slot, true);
                    decals.mark_slot_dirty(slot.mirrored_slot(), true);
                    if let Some(did) = decals.loaded_design_id {
                        decals.save_for_design(did);
                    }
                } else {
                    decals.brush_mirror_x = !decals.brush_mirror_x;
                }
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                state.ui_dirty = true;
                return;
            }
            UiAction::DecalToggleFlip => {
                if let Some(d) = decals.selected_layer_mut() {
                    d.flip_h = !d.flip_h;
                    let (slot, mirror) = (d.slot, d.mirror_x);
                    decals.brush_flip_h = d.flip_h;
                    decals.mark_decal_slots_dirty(slot, mirror, true);
                    if let Some(did) = decals.loaded_design_id {
                        decals.save_for_design(did);
                    }
                } else {
                    decals.brush_flip_h = !decals.brush_flip_h;
                }
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                state.ui_dirty = true;
                return;
            }
            UiAction::DecalTint(idx) => {
                if let Some(&(_, rgba)) = DECAL_TINTS.get(idx) {
                    decals.brush_tint = rgba;
                    if let Some(d) = decals.selected_layer_mut() {
                        d.tint = rgba;
                        let (slot, mirror) = (d.slot, d.mirror_x);
                        decals.mark_decal_slots_dirty(slot, mirror, true);
                        if let Some(did) = decals.loaded_design_id {
                            decals.save_for_design(did);
                        }
                    }
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_EQUIP);
                    state.ui_dirty = true;
                }
                return;
            }
            UiAction::DecalDuplicate => {
                if let Some(src) = decals.selected_layer().cloned() {
                    if decals.slot_layer_count(src.slot) < decal::MAX_DECALS_PER_SLOT {
                        let id = decals.next_id;
                        decals.next_id += 1;
                        let mut copy = src;
                        copy.id = id;
                        copy.position[0] += 0.12;
                        let (slot, mirror) = (copy.slot, copy.mirror_x);
                        decals.layers.push(copy);
                        decals.selected_id = Some(id);
                        decals.mark_decal_slots_dirty(slot, mirror, true);
                        if let Some(did) = decals.loaded_design_id {
                            decals.save_for_design(did);
                        }
                        sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_EQUIP);
                        state.ui_dirty = true;
                    } else {
                        sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_ERROR);
                    }
                }
                return;
            }
            UiAction::DecalClearAll => {
                if !decals.layers.is_empty() {
                    decals.layers.clear();
                    decals.selected_id = None;
                    decals.dragging = false;
                    decals.mark_all_dirty(true);
                    if let Some(did) = decals.loaded_design_id {
                        decals.save_for_design(did);
                    }
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_CANCEL);
                    state.set_banner("CLEARED ALL DECALS", Color::srgb(0.95, 0.72, 0.32));
                }
                return;
            }
            UiAction::EmblemSelect(idx) => {
                let selected = match decals.library_tab {
                    DecalLibraryTab::Custom => decals
                        .custom_assets
                        .get(idx)
                        .map(|a| (a.key.clone(), a.label.clone())),
                    DecalLibraryTab::Emblems => decals.disc_emblems.get(idx).cloned().map(|e| {
                        let _ = decals.ensure_emblem(&garage.disc, &e.key, &mut images);
                        (e.key, e.name)
                    }),
                };
                if let Some((key, label)) = selected {
                    decals.set_ac_emblem(Some(key), true);
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_EQUIP);
                    state.set_banner(
                        format!("AC EMBLEM SET: {label} (SHOULDER & GARAGE)"),
                        Color::srgb(0.45, 0.92, 0.62),
                    );
                }
                return;
            }
            UiAction::EmblemResetDefault => {
                decals.reset_ac_emblem_to_default();
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_OK);
                let msg = match &decals.ac_emblem_key {
                    Some(k) => format!("RESET EMBLEM TO DESIGN DEFAULT ({})", k.strip_prefix("emblem:").unwrap_or(k)),
                    None => "RESET EMBLEM TO DESIGN DEFAULT (NONE)".to_string(),
                };
                state.set_banner(msg, Color::srgb(0.55, 0.85, 1.0));
                return;
            }
            UiAction::EmblemClear => {
                decals.set_ac_emblem(None, true);
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_CANCEL);
                state.set_banner("CLEARED AC EMBLEM", Color::srgb(0.95, 0.72, 0.32));
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
            UiAction::ToggleFullscreen => {
                state.cursor = 0;
                toggle_fullscreen_setting(
                    &mut commands,
                    &mut options,
                    &mut cues,
                    &mut audio_assets,
                    &mut state,
                );
                return;
            }
            UiAction::SelectOptionRow(row) => {
                fallback_select_row = Some(row);
                continue;
            }
            UiAction::UiScaleStep(delta) => {
                state.cursor = 1;
                step_ui_scale_setting(
                    &mut commands,
                    &mut options,
                    &mut cues,
                    &mut audio_assets,
                    &mut state,
                    delta,
                );
                return;
            }
            UiAction::HudSwayStep(delta) => {
                state.cursor = 2;
                step_hud_sway_setting(
                    &mut commands,
                    &mut options,
                    &mut cues,
                    &mut audio_assets,
                    &mut state,
                    delta,
                );
                return;
            }
            UiAction::VolumeStep(channel, delta) => {
                if let Some((row, _)) = GARAGE_OPTION_CHANNELS.iter().find(|&&(_, c)| c == channel)
                {
                    state.cursor = *row;
                }
                step_volume_setting(
                    &mut commands,
                    &mut options,
                    &mut cues,
                    &mut audio_assets,
                    &mut state,
                    channel,
                    delta,
                );
                return;
            }
            UiAction::QuitToDesktop => {
                options.app_exit.write(AppExit::Success);
                return;
            }
            UiAction::AssembleSelectSlot(new_slot) => {
                switch_assemble_slot(
                    &mut commands,
                    &mut state,
                    &mut garage,
                    &mut cues,
                    &mut audio_assets,
                    new_slot,
                );
                return;
            }
            UiAction::AssembleCatStep(delta) => {
                if let GarageScreen::AssembleParts { slot } = state.screen {
                    let cur = AssembleSlot::ALL
                        .iter()
                        .position(|&s| s == slot)
                        .unwrap_or(0);
                    let len = AssembleSlot::ALL.len();
                    let next = (cur + len + (delta as isize).rem_euclid(len as isize) as usize) % len;
                    let new_slot = AssembleSlot::ALL[next];
                    switch_assemble_slot(
                        &mut commands,
                        &mut state,
                        &mut garage,
                        &mut cues,
                        &mut audio_assets,
                        new_slot,
                    );
                }
                return;
            }
            UiAction::AssemblePartStep(delta) => {
                if let GarageScreen::AssembleParts { slot } = state.screen {
                    let count = crate::garage_assemble::filtered_parts_for_slot(
                        slot,
                        state.assemble_subcat,
                        state.assemble_dmg_filter,
                    )
                    .len();
                    if count > 0 {
                        let next = if delta < 0 {
                            if state.cursor == 0 { count - 1 } else { state.cursor - 1 }
                        } else {
                            (state.cursor + 1) % count
                        };
                        state.cursor = next;
                        clamp_scroll_win(
                            &mut state.scroll,
                            next,
                            count,
                            crate::garage_assemble::ASSEMBLE_THUMB_WINDOW,
                        );
                        apply_cursor_preview(&mut state, &mut garage);
                        sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                        state.ui_dirty = true;
                    }
                }
                return;
            }
            UiAction::AssembleSubcat(sub) => {
                if let GarageScreen::AssembleParts { slot } = state.screen {
                    if state.assemble_subcat != sub {
                        state.assemble_subcat = sub;
                        state.cursor = 0;
                        state.scroll = 0;
                        apply_cursor_preview(&mut state, &mut garage);
                        sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_CATEGORY);
                        state.ui_dirty = true;
                    }
                    let _ = slot;
                }
                return;
            }
            UiAction::AssembleDmgFilter(dmg) => {
                if let GarageScreen::AssembleParts { .. } = state.screen {
                    if state.assemble_dmg_filter != dmg {
                        state.assemble_dmg_filter = dmg;
                        state.cursor = 0;
                        state.scroll = 0;
                        apply_cursor_preview(&mut state, &mut garage);
                        sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_CATEGORY);
                        state.ui_dirty = true;
                    }
                }
                return;
            }
            UiAction::AssembleToggleDetail => {
                state.assemble_detail_view = !state.assemble_detail_view;
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_CATEGORY);
                state.ui_dirty = true;
                return;
            }
        }
    }
    if let Some(row) = fallback_select_row {
        if state.cursor != row {
            state.cursor = row;
            sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
            state.ui_dirty = true;
        }
        return;
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
                &mut options,
                &mut cues,
                &mut audio_assets,
                &mut mode_switch,
            );
            return;
        }
    }

    // Mouse-wheel over `@PartsCategoryList` / `@PartsList` scrolls those strips instead of zooming.
    if matches!(state.screen, GarageScreen::AssembleParts { .. }) && scroll.delta.y.abs() > 0.0 {
        if let Ok(window) = options.windows.single() {
            if let Some(cursor) = window.cursor_position() {
                let strip = crate::garage_assemble::hovered_scroll_strip(
                    Vec2::new(window.width(), window.height()),
                    cursor,
                );
                // Bevy: positive Y = scroll up / away from user.
                let steps = (-scroll.delta.y).round() as i32;
                let steps = if steps == 0 {
                    if scroll.delta.y < 0.0 { 1 } else { -1 }
                } else {
                    steps
                };
                match strip {
                    Some(crate::garage_assemble::AssembleScrollStrip::CategoryList) => {
                        let GarageScreen::AssembleParts { slot } = state.screen else {
                            unreachable!()
                        };
                        let cur = AssembleSlot::ALL
                            .iter()
                            .position(|&s| s == slot)
                            .unwrap_or(0);
                        let len = AssembleSlot::ALL.len() as isize;
                        let next = (cur as isize + steps as isize).rem_euclid(len) as usize;
                        switch_assemble_slot(
                            &mut commands,
                            &mut state,
                            &mut garage,
                            &mut cues,
                            &mut audio_assets,
                            AssembleSlot::ALL[next],
                        );
                        return;
                    }
                    Some(crate::garage_assemble::AssembleScrollStrip::PartsList) => {
                        let GarageScreen::AssembleParts { slot } = state.screen else {
                            unreachable!()
                        };
                        let count = crate::garage_assemble::filtered_parts_for_slot(
                            slot,
                            state.assemble_subcat,
                            state.assemble_dmg_filter,
                        )
                        .len();
                        if count > 0 {
                            let next = (state.cursor as isize + steps as isize)
                                .rem_euclid(count as isize)
                                as usize;
                            state.cursor = next;
                            clamp_scroll_win(
                                &mut state.scroll,
                                next,
                                count,
                                crate::garage_assemble::ASSEMBLE_THUMB_WINDOW,
                            );
                            apply_cursor_preview(&mut state, &mut garage);
                            sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                            state.ui_dirty = true;
                        }
                        return;
                    }
                    None => {}
                }
            }
        }
    }

    // Mouse-wheel over any hovered adjustable garage widget (Options sliders, Paint RGB/Camo,
    // Decal scale/rotate/opacity, Sortie map/target) steps its value up/down.
    if scroll.delta.y.abs() > 0.0 {
        if let Some(target) = action_buttons.iter().find_map(|(interaction, action)| {
            matches!(*interaction, Interaction::Hovered | Interaction::Pressed)
                .then(|| action.wheel_adjust())
                .flatten()
        }) {
            state.wheel_consumed = true;
            let delta = wheel_step_direction(scroll.delta.y);
            match target {
                WheelAdjust::UiScale => {
                    state.cursor = 1;
                    step_ui_scale_setting(
                        &mut commands,
                        &mut options,
                        &mut cues,
                        &mut audio_assets,
                        &mut state,
                        delta,
                    );
                }
                WheelAdjust::HudSway => {
                    state.cursor = 2;
                    step_hud_sway_setting(
                        &mut commands,
                        &mut options,
                        &mut cues,
                        &mut audio_assets,
                        &mut state,
                        delta,
                    );
                }
                WheelAdjust::Volume(channel) => {
                    if let Some((row, _)) =
                        GARAGE_OPTION_CHANNELS.iter().find(|&&(_, c)| c == channel)
                    {
                        state.cursor = *row;
                    }
                    step_volume_setting(
                        &mut commands,
                        &mut options,
                        &mut cues,
                        &mut audio_assets,
                        &mut state,
                        channel,
                        delta,
                    );
                }
                WheelAdjust::PaintRgb(comp) => {
                    if state.screen == GarageScreen::Options {
                        options.settings.hud_color[comp] =
                            (options.settings.hud_color[comp] as i16 + delta as i16 * 8)
                                .clamp(0, 255) as u8;
                        options.settings.save();
                        let hc = options.settings.hud_color;
                        state.paint_hsv = paint::rgb_to_hsv([hc[0], hc[1], hc[2], 255]);
                        state.paint_hsv_synced_channel = Some(usize::MAX - 1);
                    } else if state.paint_tab == PaintTab::Decals {
                        let mut c = current_decal_tint(&decals);
                        c[comp] = (c[comp] as i16 + delta as i16 * 8).clamp(0, 255) as u8;
                        apply_decal_tint_color(&mut decals, c);
                        state.paint_hsv = paint::rgb_to_hsv(c);
                        state.paint_hsv_synced_channel = Some(usize::MAX);
                    } else {
                        step_custom_channel_rgb(
                            &mut garage,
                            state.paint_channel,
                            comp,
                            delta as i16 * 16,
                        );
                        state.paint_hsv =
                            paint::rgb_to_hsv(current_channel_color(&garage, state.paint_channel));
                        state.paint_hsv_synced_channel = Some(state.paint_channel);
                    }
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                    state.ui_dirty = true;
                }
                WheelAdjust::PaintCamo => {
                    step_custom_camo(&mut garage, delta);
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                    state.ui_dirty = true;
                }
                WheelAdjust::PaintCamoScale => {
                    options.custom_camos.step_custom_scale(delta);
                    if options.custom_camos.active_key.is_some() {
                        garage.dirty = true;
                    }
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                    state.ui_dirty = true;
                }
                WheelAdjust::DecalScale => {
                    if let Some(d) = decals.selected_layer_mut() {
                        d.scale = (d.scale + delta as f32 * 0.08).clamp(0.08, 2.5);
                        let (slot, mirror, sc) = (d.slot, d.mirror_x, d.scale);
                        decals.brush_scale = sc;
                        decals.mark_decal_slots_dirty(slot, mirror, true);
                        if let Some(did) = decals.loaded_design_id {
                            decals.save_for_design(did);
                        }
                    } else {
                        decals.brush_scale =
                            (decals.brush_scale + delta as f32 * 0.08).clamp(0.08, 2.5);
                    }
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                    state.ui_dirty = true;
                }
                WheelAdjust::DecalRotate => {
                    if let Some(d) = decals.selected_layer_mut() {
                        d.rotation = (d.rotation + (delta as f32 * 15.0).to_radians())
                            .rem_euclid(std::f32::consts::TAU);
                        let (slot, mirror) = (d.slot, d.mirror_x);
                        decals.mark_decal_slots_dirty(slot, mirror, true);
                        if let Some(did) = decals.loaded_design_id {
                            decals.save_for_design(did);
                        }
                        sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                        state.ui_dirty = true;
                    }
                }
                WheelAdjust::DecalOpacity => {
                    if let Some(d) = decals.selected_layer_mut() {
                        d.opacity = (d.opacity + delta as f32 * 0.1).clamp(0.1, 1.0);
                        let (slot, mirror) = (d.slot, d.mirror_x);
                        decals.mark_decal_slots_dirty(slot, mirror, true);
                        if let Some(did) = decals.loaded_design_id {
                            decals.save_for_design(did);
                        }
                        sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                        state.ui_dirty = true;
                    }
                }
                WheelAdjust::SortieMap => {
                    let n = SORTIE_MAPS.len() as isize;
                    state.sortie_map_idx =
                        (state.sortie_map_idx as isize + delta as isize).rem_euclid(n) as usize;
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                    state.ui_dirty = true;
                }
                WheelAdjust::SortieTarget => {
                    let n = SORTIE_TARGETS.len() as isize;
                    state.sortie_target_idx =
                        (state.sortie_target_idx as isize + delta as isize).rem_euclid(n) as usize;
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                    state.ui_dirty = true;
                }
            }
            return;
        }
    }

    if state.paint_exit_prompt {
        let toggle = keys.just_pressed(KeyCode::ArrowLeft)
            || keys.just_pressed(KeyCode::ArrowRight)
            || keys.just_pressed(KeyCode::KeyA)
            || keys.just_pressed(KeyCode::KeyD)
            || keys.just_pressed(KeyCode::Tab)
            || gp_left
            || gp_right;
        if toggle {
            state.paint_exit_prompt_choice = !state.paint_exit_prompt_choice;
            sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
            state.ui_dirty = true;
            return;
        }
        if keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space) || gp_confirm {
            if state.paint_exit_prompt_choice {
                finish_paint_exit_save(&mut commands, &mut state, &mut cues, &mut audio_assets);
            } else {
                finish_paint_exit_discard(
                    &mut commands,
                    &mut state,
                    &mut garage,
                    &mut cues,
                    &mut audio_assets,
                );
            }
            return;
        }
        if keys.just_pressed(KeyCode::Escape) || keys.just_pressed(KeyCode::Backspace) || gp_cancel {
            state.paint_exit_prompt = false;
            sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_CANCEL);
            state.ui_dirty = true;
            return;
        }
        return;
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

    // `Esc` / Gamepad `Start` toggles the Options menu from Garage Top.
    if (keys.just_pressed(KeyCode::Escape) || gp_start) && state.screen == GarageScreen::Top {
        open_options_screen(&mut commands, &mut state, &mut cues, &mut audio_assets);
        return;
    }

    // Cancel / Back (`Esc` or `Backspace` or Gamepad East, or `Start` while in Options).
    if keys.just_pressed(KeyCode::Escape)
        || keys.just_pressed(KeyCode::Backspace)
        || gp_cancel
        || (gp_start && state.screen == GarageScreen::Options)
    {
        trigger_cancel(&mut commands, &mut state, &mut garage, &mut cues, &mut audio_assets);
        return;
    }

    // In `AssembleParts`, `Tab` toggles Normal View vs Detail Specs (`OnBtnAccel_ChangeView`),
    // `A`/`D` or Left/Right switches slot tabs directly, and `Q`/`E` cycles subcategory tabs.
    if let GarageScreen::AssembleParts { slot } = state.screen {
        if keys.just_pressed(KeyCode::Tab) {
            state.assemble_detail_view = !state.assemble_detail_view;
            sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_CATEGORY);
            state.ui_dirty = true;
            return;
        }
        if keys.just_pressed(KeyCode::KeyQ) || keys.just_pressed(KeyCode::KeyE) {
            let subs = crate::garage_assemble::subcategories_for_slot(slot, None);
            if !subs.is_empty() {
                let mut opts: Vec<Option<u8>> = Vec::with_capacity(subs.len() + 1);
                opts.push(None);
                opts.extend(subs.into_iter().map(|(id, _)| Some(id)));
                let cur_i = opts
                    .iter()
                    .position(|o| *o == state.assemble_subcat)
                    .unwrap_or(0);
                let delta: isize = if keys.just_pressed(KeyCode::KeyE) { 1 } else { -1 };
                let next_i = (cur_i as isize + delta).rem_euclid(opts.len() as isize) as usize;
                state.assemble_subcat = opts[next_i];
                state.cursor = 0;
                state.scroll = 0;
                apply_cursor_preview(&mut state, &mut garage);
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_CATEGORY);
                state.ui_dirty = true;
                return;
            }
        }
    }

    // Paint tab switching via `Tab` (and `Q`/`E` outside the Decal editor).
    let tab_switch = keys.just_pressed(KeyCode::Tab)
        || (state.paint_tab != PaintTab::Decals
            && (keys.just_pressed(KeyCode::KeyQ) || keys.just_pressed(KeyCode::KeyE)));
    if state.screen == GarageScreen::Paint && tab_switch {
        state.paint_tab = match state.paint_tab {
            PaintTab::Presets => PaintTab::Custom,
            PaintTab::Custom => PaintTab::Decals,
            PaintTab::Decals => PaintTab::Emblem,
            PaintTab::Emblem => PaintTab::Presets,
        };
        state.cursor = 0;
        state.scroll = 0;
        state.cam_manual_orbit = false;
        sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_CATEGORY);
        state.ui_dirty = true;
        return;
    }

    // In `Paint -> Decals`, let `decal::interact_and_gizmo` own mouse & keyboard shortcuts cleanly.
    if state.screen == GarageScreen::Paint && state.paint_tab == PaintTab::Decals {
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

    // `AssembleParts` Left/Right switches directly between AC assembly slot tabs.
    if let GarageScreen::AssembleParts { slot } = state.screen {
        if left || right {
            let delta: isize = if right { 1 } else { -1 };
            let cur_i = AssembleSlot::ALL
                .iter()
                .position(|&s| s == slot)
                .unwrap_or(0);
            let n = AssembleSlot::ALL.len() as isize;
            let next_slot = AssembleSlot::ALL[(cur_i as isize + delta).rem_euclid(n) as usize];
            switch_assemble_slot(
                &mut commands,
                &mut state,
                &mut garage,
                &mut cues,
                &mut audio_assets,
                next_slot,
            );
            return;
        }
    }

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

    // Options screen: Left/Right toggles fullscreen or nudges UI scale / audio sliders.
    if state.screen == GarageScreen::Options && (left || right) {
        let delta: i8 = if right { 1 } else { -1 };
        match state.cursor {
            0 => {
                toggle_fullscreen_setting(
                    &mut commands,
                    &mut options,
                    &mut cues,
                    &mut audio_assets,
                    &mut state,
                );
                return;
            }
            1 => {
                step_ui_scale_setting(
                    &mut commands,
                    &mut options,
                    &mut cues,
                    &mut audio_assets,
                    &mut state,
                    delta,
                );
                return;
            }
            2 => {
                step_hud_sway_setting(
                    &mut commands,
                    &mut options,
                    &mut cues,
                    &mut audio_assets,
                    &mut state,
                    delta,
                );
                return;
            }
            3 => {
                step_volume_setting(
                    &mut commands,
                    &mut options,
                    &mut cues,
                    &mut audio_assets,
                    &mut state,
                    AudioChannel::Master,
                    delta,
                );
                return;
            }
            4 => {
                step_volume_setting(
                    &mut commands,
                    &mut options,
                    &mut cues,
                    &mut audio_assets,
                    &mut state,
                    AudioChannel::Music,
                    delta,
                );
                return;
            }
            5 => {
                step_volume_setting(
                    &mut commands,
                    &mut options,
                    &mut cues,
                    &mut audio_assets,
                    &mut state,
                    AudioChannel::Sfx,
                    delta,
                );
                return;
            }
            6 => {
                step_volume_setting(
                    &mut commands,
                    &mut options,
                    &mut cues,
                    &mut audio_assets,
                    &mut state,
                    AudioChannel::Ui,
                    delta,
                );
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
        let win = if matches!(state.screen, GarageScreen::AssembleParts { .. }) {
            crate::garage_assemble::ASSEMBLE_THUMB_WINDOW
        } else {
            LIST_WINDOW
        };
        clamp_scroll_win(&mut state.scroll, next_cursor, count, win);
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
            &mut options,
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
        GarageScreen::AssembleParts { slot } => crate::garage_assemble::filtered_parts_for_slot(
            slot,
            state.assemble_subcat,
            state.assemble_dmg_filter,
        )
        .len(),
        GarageScreen::Paint => match state.paint_tab {
            PaintTab::Presets => state.color_presets.len(),
            PaintTab::Custom => 8, // 7 channels + Camouflage
            PaintTab::Decals | PaintTab::Emblem => 0,
        },
        GarageScreen::Design => garage.designs.len(),
        GarageScreen::GarageCustom => state.workshops.len(),
        GarageScreen::SortieConfig => 3, // Target Set, Map, Launch
        GarageScreen::Options => 9, // Fullscreen, UI Scale, HUD Sway, Master, Music, SFX, Menu/UI, Back, Quit to Desktop
        GarageScreen::ViewMode => 0,
    }
}

fn toggle_fullscreen_setting(
    commands: &mut Commands,
    options: &mut OptionsCtx,
    cues: &mut Cues,
    audio_assets: &mut Assets<AudioSource>,
    state: &mut GarageState,
) {
    options.settings.fullscreen = !options.settings.fullscreen;
    if let Ok(mut window) = options.windows.single_mut() {
        settings::apply_fullscreen(&mut window, options.settings.fullscreen);
    }
    options.settings.save();
    sound::cue(commands, cues, audio_assets, CUE_OK);
    state.set_banner(
        if options.settings.fullscreen {
            "FULLSCREEN: ON"
        } else {
            "FULLSCREEN: OFF"
        },
        Color::srgb(0.45, 0.92, 0.62),
    );
    state.ui_dirty = true;
}

fn step_ui_scale_setting(
    commands: &mut Commands,
    options: &mut OptionsCtx,
    cues: &mut Cues,
    audio_assets: &mut Assets<AudioSource>,
    state: &mut GarageState,
    delta: i8,
) {
    options.settings.step_ui_scale(delta);
    options.settings.save();
    sound::cue(commands, cues, audio_assets, CUE_MOVE);
    state.set_banner(
        format!("UI SCALE: {}%", options.settings.ui_scale_percent()),
        Color::srgb(0.45, 0.92, 0.62),
    );
    state.ui_dirty = true;
}

fn step_hud_sway_setting(
    commands: &mut Commands,
    options: &mut OptionsCtx,
    cues: &mut Cues,
    audio_assets: &mut Assets<AudioSource>,
    state: &mut GarageState,
    delta: i8,
) {
    options.settings.step_hud_sway(delta);
    options.settings.save();
    sound::cue(commands, cues, audio_assets, CUE_MOVE);
    state.set_banner(
        format!("HUD SWAY: {}%", options.settings.hud_sway_percent()),
        Color::srgb(0.45, 0.92, 0.62),
    );
    state.ui_dirty = true;
}

fn step_volume_setting(
    commands: &mut Commands,
    options: &mut OptionsCtx,
    cues: &mut Cues,
    audio_assets: &mut Assets<AudioSource>,
    state: &mut GarageState,
    channel: AudioChannel,
    delta: i8,
) {
    options.settings.step_channel(channel, delta);
    settings::apply_volume(
        &options.settings,
        &options.bgm,
        cues,
        &mut options.global_volume,
        &mut options.sinks,
        &options.bgm_players,
        &options.audio_buses,
    );
    options.settings.save();
    sound::cue(commands, cues, audio_assets, CUE_MOVE);
    state.set_banner(
        format!(
            "{}: {}%",
            channel.label(),
            options.settings.channel_percent(channel)
        ),
        Color::srgb(0.45, 0.92, 0.62),
    );
    state.ui_dirty = true;
}

fn clamp_scroll_win(scroll: &mut usize, cursor: usize, count: usize, window: usize) {
    if count <= window {
        *scroll = 0;
        return;
    }
    if cursor < *scroll {
        *scroll = cursor;
    } else if cursor >= *scroll + window {
        *scroll = cursor + 1 - window;
    }
    *scroll = (*scroll).min(count.saturating_sub(window));
}

fn clamp_scroll(scroll: &mut usize, cursor: usize, count: usize) {
    clamp_scroll_win(scroll, cursor, count, LIST_WINDOW);
}

fn step_list_page(scroll: usize, count: usize, window: usize, delta: i8) -> usize {
    if count <= window || window == 0 {
        return 0;
    }
    if delta > 0 {
        let next = scroll.saturating_add(window);
        if next >= count { 0 } else { next }
    } else if delta < 0 {
        if scroll == 0 {
            ((count - 1) / window) * window
        } else {
            scroll.saturating_sub(window)
        }
    } else {
        scroll.min(count - 1)
    }
}

fn switch_assemble_slot(
    commands: &mut Commands,
    state: &mut GarageState,
    garage: &mut crate::Garage,
    cues: &mut Cues,
    audio_assets: &mut Assets<AudioSource>,
    new_slot: AssembleSlot,
) {
    revert_preview(state, garage);
    if has_weapon_arms(&garage.active_design) && new_slot.blocked_by_weapon_arm() {
        sound::cue(commands, cues, audio_assets, CUE_ERROR);
        state.set_banner(
            "LOCKED BY WEAPON ARMS — SWAP ARMS FIRST",
            Color::srgb(0.95, 0.35, 0.30),
        );
        return;
    }
    let idx = AssembleSlot::ALL
        .iter()
        .position(|&s| s == new_slot)
        .unwrap_or(0);
    state.slot_cursor = idx;
    clamp_scroll_win(
        &mut state.assemble_cat_scroll,
        idx,
        AssembleSlot::ALL.len(),
        crate::garage_assemble::ASSEMBLE_CAT_WINDOW,
    );
    state.assemble_subcat = None;
    state.assemble_dmg_filter = None;
    state.saved_design = Some(garage.active_design);
    let parts = parts_for_slot(new_slot);
    let equipped = new_slot.get(&garage.active_design);
    state.cursor = parts.iter().position(|&id| id == equipped).unwrap_or(0);
    clamp_scroll_win(
        &mut state.scroll,
        state.cursor,
        parts.len(),
        crate::garage_assemble::ASSEMBLE_THUMB_WINDOW,
    );
    state.screen = GarageScreen::AssembleParts { slot: new_slot };
    state.cam_manual_orbit = false;
    sound::cue(commands, cues, audio_assets, CUE_CATEGORY);
    state.ui_dirty = true;
}

/// Whether changing `slot` alters any visible 3D mesh on the assembled AC.
/// Internal parts (FCS, Generator, Recon) have no mounted AC mesh so hovering/equipping them
/// never needs to rebuild the 3D AC or re-bake decals.
fn slot_has_visible_ac_model(slot: AssembleSlot) -> bool {
    !matches!(
        slot,
        AssembleSlot::Fcs | AssembleSlot::Generator | AssembleSlot::Recon
    )
}

/// True when two designs differ in any visible 3D AC part slot or coloring ID.
fn visible_ac_design_differs(a: &AcAssemblyDesignSt, b: &AcAssemblyDesignSt) -> bool {
    a.coloring_id != b.coloring_id
        || AssembleSlot::ALL
            .iter()
            .any(|&s| slot_has_visible_ac_model(s) && s.get(a) != s.get(b))
}

fn revert_preview(state: &mut GarageState, garage: &mut crate::Garage) {
    state.ac_preview_debounce = None;
    let was_rebuilt = std::mem::take(&mut state.ac_preview_rebuilt);
    let mut visual_changed = false;
    if let Some(d) = state.saved_design.take() {
        if visible_ac_design_differs(&garage.active_design, &d) && was_rebuilt {
            visual_changed = true;
        }
        garage.active_design = d;
    }
    if let Some(c) = state.saved_color.take() {
        if garage.custom_color != c {
            visual_changed = true;
        }
        garage.custom_color = c;
    }
    if let Some(n) = state.saved_name.take() {
        garage.active_name = n;
    }
    if visual_changed {
        garage.dirty = true;
    }
}

fn apply_cursor_preview(state: &mut GarageState, garage: &mut crate::Garage) {
    match state.screen {
        GarageScreen::AssembleParts { slot } => {
            let parts = crate::garage_assemble::filtered_parts_for_slot(
                slot,
                state.assemble_subcat,
                state.assemble_dmg_filter,
            );
            if let Some(&part_id) = parts.get(state.cursor) {
                if slot.get(&garage.active_design) != part_id {
                    // Only the isolated `ASSEMBLY VIEW` preview AC updates while browsing parts;
                    // the main garage mech in the blurred background updates on Enter (equip).
                    slot.set(&mut garage.active_design, part_id);
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

/// Screen transition when pressing `Esc` in the Garage:
/// - On `Top`, opens `Options`.
/// - On `Options` (or any sub-screen), returns to `Top`.
fn escape_target_screen(screen: GarageScreen) -> GarageScreen {
    match screen {
        GarageScreen::Top => GarageScreen::Options,
        _ => GarageScreen::Top,
    }
}

fn open_options_screen(
    commands: &mut Commands,
    state: &mut GarageState,
    cues: &mut Cues,
    audio_assets: &mut Assets<AudioSource>,
) {
    state.screen = escape_target_screen(GarageScreen::Top);
    state.cursor = 0;
    state.scroll = 0;
    state.cam_manual_orbit = false;
    sound::cue(commands, cues, audio_assets, CUE_OK);
    state.ui_dirty = true;
}

/// Returns `true` when the player has modified the AC's paint preset or custom color/camo
/// since entering `GarageScreen::Paint` (or since their last explicit Save).
fn paint_has_unsaved_changes(state: &GarageState, garage: &crate::Garage) -> bool {
    let color_changed = state
        .saved_color
        .is_some_and(|saved| garage.custom_color != saved);
    let preset_changed = state
        .saved_design
        .is_some_and(|saved| garage.active_design.coloring_id != saved.coloring_id);
    color_changed || preset_changed
}

fn finish_paint_exit_save(
    commands: &mut Commands,
    state: &mut GarageState,
    cues: &mut Cues,
    audio_assets: &mut Assets<AudioSource>,
) {
    state.paint_exit_prompt = false;
    state.saved_design = None;
    state.saved_color = None;
    state.screen = GarageScreen::Top;
    state.cursor = 0;
    state.scroll = 0;
    state.cam_manual_orbit = false;
    sound::cue(commands, cues, audio_assets, CUE_OK);
    state.set_banner("PAINT SAVED", Color::srgb(0.45, 0.92, 0.62));
    state.ui_dirty = true;
}

fn finish_paint_exit_discard(
    commands: &mut Commands,
    state: &mut GarageState,
    garage: &mut crate::Garage,
    cues: &mut Cues,
    audio_assets: &mut Assets<AudioSource>,
) {
    state.paint_exit_prompt = false;
    revert_preview(state, garage);
    state.screen = GarageScreen::Top;
    state.cursor = 0;
    state.scroll = 0;
    state.cam_manual_orbit = false;
    sound::cue(commands, cues, audio_assets, CUE_CANCEL);
    state.ui_dirty = true;
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
        GarageScreen::Paint => {
            if paint_has_unsaved_changes(state, garage) && !state.paint_exit_prompt {
                state.paint_exit_prompt = true;
                state.paint_exit_prompt_choice = true;
                sound::cue(commands, cues, audio_assets, CUE_CATEGORY);
                state.ui_dirty = true;
                return;
            }
            finish_paint_exit_discard(commands, state, garage, cues, audio_assets);
        }
        GarageScreen::AssembleParts { .. } => {
            revert_preview(state, garage);
            state.screen = GarageScreen::Top;
            state.cursor = 0;
            state.scroll = 0;
            state.cam_manual_orbit = false;
            sound::cue(commands, cues, audio_assets, CUE_CANCEL);
            state.ui_dirty = true;
        }
        GarageScreen::AssembleSlots
        | GarageScreen::Design
        | GarageScreen::GarageCustom
        | GarageScreen::SortieConfig
        | GarageScreen::Options
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
    options: &mut OptionsCtx,
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
                    let slot = AssembleSlot::ALL
                        .get(state.slot_cursor)
                        .copied()
                        .unwrap_or(AssembleSlot::Head);
                    clamp_scroll_win(
                        &mut state.assemble_cat_scroll,
                        state.slot_cursor,
                        AssembleSlot::ALL.len(),
                        crate::garage_assemble::ASSEMBLE_CAT_WINDOW,
                    );
                    state.assemble_subcat = None;
                    state.assemble_dmg_filter = None;
                    state.saved_design = Some(garage.active_design);
                    let parts = parts_for_slot(slot);
                    let equipped = slot.get(&garage.active_design);
                    state.cursor = parts.iter().position(|&id| id == equipped).unwrap_or(0);
                    clamp_scroll_win(
                        &mut state.scroll,
                        state.cursor,
                        parts.len(),
                        crate::garage_assemble::ASSEMBLE_THUMB_WINDOW,
                    );
                    state.screen = GarageScreen::AssembleParts { slot };
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
                6 => {
                    state.screen = GarageScreen::Options;
                    state.cursor = 0;
                    state.scroll = 0;
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
            clamp_scroll_win(
                &mut state.assemble_cat_scroll,
                state.cursor,
                AssembleSlot::ALL.len(),
                crate::garage_assemble::ASSEMBLE_CAT_WINDOW,
            );
            state.assemble_subcat = None;
            state.assemble_dmg_filter = None;
            state.saved_design = Some(garage.active_design);
            let parts = parts_for_slot(slot);
            let equipped = slot.get(&garage.active_design);
            state.cursor = parts.iter().position(|&id| id == equipped).unwrap_or(0);
            clamp_scroll_win(
                &mut state.scroll,
                state.cursor,
                parts.len(),
                crate::garage_assemble::ASSEMBLE_THUMB_WINDOW,
            );
            state.screen = GarageScreen::AssembleParts { slot };
            state.cam_manual_orbit = false;
            sound::cue(commands, cues, audio_assets, CUE_CATEGORY);
            state.ui_dirty = true;
        }
        GarageScreen::AssembleParts { slot } => {
            let parts = crate::garage_assemble::filtered_parts_for_slot(
                slot,
                state.assemble_subcat,
                state.assemble_dmg_filter,
            );
            let Some(&part_id) = parts.get(state.cursor) else {
                return;
            };
            let orig = state.saved_design.unwrap_or(garage.active_design);
            let changed = slot.get(&orig) != part_id;
            slot.set(&mut garage.active_design, part_id);
            let pending = state.ac_preview_debounce.take().is_some();
            state.ac_preview_rebuilt = false;
            if slot_has_visible_ac_model(slot) && (changed || pending) {
                garage.dirty = true;
            }
            // Commit the newly equipped part as the active baseline while staying right in the
            // retail `AssembleParts` screen (matching retail ACVD — no jump back to `AssembleSlots`).
            state.saved_design = Some(garage.active_design);
            let cue = if part_id == 0 { CUE_UNEQUIP } else { CUE_EQUIP };
            sound::cue(commands, cues, audio_assets, cue);
            let pname = part_name(state.parts_fmg.as_ref(), slot.category(), part_id);
            state.set_banner(
                format!("{}: {}", slot.label(), pname),
                Color::srgb(0.45, 0.92, 0.62),
            );
            state.ui_dirty = true;
        }
        GarageScreen::Paint => {
            match state.paint_tab {
                PaintTab::Presets => {
                    if let Some(&color_id) = state.color_presets.get(state.cursor) {
                        garage.active_design.coloring_id = color_id;
                        garage.custom_color = paint::load_accolor(&garage.disc, color_id);
                        garage.dirty = true;
                        state.saved_design = Some(garage.active_design);
                        state.saved_color = Some(garage.custom_color);
                        sound::cue(commands, cues, audio_assets, CUE_EQUIP);
                        state.set_banner(
                            format!("APPLIED COLOR PRESET {:04}", color_id),
                            Color::srgb(0.45, 0.92, 0.62),
                        );
                    }
                }
                PaintTab::Custom => {
                    state.saved_design = Some(garage.active_design);
                    state.saved_color = Some(garage.custom_color);
                    sound::cue(commands, cues, audio_assets, CUE_OK);
                    state.set_banner("CUSTOM PAINT SAVED", Color::srgb(0.45, 0.92, 0.62));
                }
                PaintTab::Decals | PaintTab::Emblem => {}
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
        GarageScreen::Options => match state.cursor {
            0 => {
                toggle_fullscreen_setting(commands, options, cues, audio_assets, state);
            }
            1 => {
                step_ui_scale_setting(commands, options, cues, audio_assets, state, 1);
            }
            2 => {
                step_hud_sway_setting(commands, options, cues, audio_assets, state, 1);
            }
            3 => {
                step_volume_setting(
                    commands,
                    options,
                    cues,
                    audio_assets,
                    state,
                    AudioChannel::Master,
                    1,
                );
            }
            4 => {
                step_volume_setting(
                    commands,
                    options,
                    cues,
                    audio_assets,
                    state,
                    AudioChannel::Music,
                    1,
                );
            }
            5 => {
                step_volume_setting(
                    commands,
                    options,
                    cues,
                    audio_assets,
                    state,
                    AudioChannel::Sfx,
                    1,
                );
            }
            6 => {
                step_volume_setting(
                    commands,
                    options,
                    cues,
                    audio_assets,
                    state,
                    AudioChannel::Ui,
                    1,
                );
            }
            7 => {
                trigger_cancel(commands, state, garage, cues, audio_assets);
            }
            8 => {
                options.app_exit.write(AppExit::Success);
            }
            _ => {}
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

fn apply_custom_channel_color_live(
    garage: &mut crate::Garage,
    channel: usize,
    rgba: [u8; 4],
    ac_meshes: &Query<(&MeshMaterial3d<paint::AcPaintMaterial>, &paint::AcPaintGroup)>,
    paint_materials: &mut Assets<paint::AcPaintMaterial>,
) {
    let col = ensure_custom_color(garage);
    if channel >= 6 {
        col.eye = rgba;
    } else {
        for g in 0..GROUP_COUNT {
            col.groups[g][channel] = rgba;
        }
    }
    if ac_meshes.is_empty() {
        garage.dirty = true;
    } else {
        let snapshot = *col;
        paint::update_ac_materials_in_place(&snapshot, ac_meshes, paint_materials);
    }
}

/// Maps a physical-pixel cursor coordinate onto a UI track rect (`center`, `size`), returning
/// clamped `(u, v) in [0, 1]^2` and whether the cursor was inside the rect.
pub fn normalize_track_point(center_phys: Vec2, size_phys: Vec2, cursor_phys: Vec2) -> (Vec2, bool) {
    let half = size_phys * 0.5;
    let min = center_phys - half;
    let max = center_phys + half;
    let inside = cursor_phys.x >= min.x
        && cursor_phys.x <= max.x
        && cursor_phys.y >= min.y
        && cursor_phys.y <= max.y;
    let u = ((cursor_phys.x - min.x) / size_phys.x.max(1.0)).clamp(0.0, 1.0);
    let v = ((cursor_phys.y - min.y) / size_phys.y.max(1.0)).clamp(0.0, 1.0);
    (Vec2::new(u, v), inside)
}

const MAX_PAINT_HISTORY: usize = 64;

/// Full reversible snapshot of Paint menu state (AC coloring/camo, placed decals, and emblem).
#[derive(Debug, Clone, PartialEq)]
pub struct PaintSnapshot {
    pub coloring_id: u16,
    pub custom_color: Option<acvd_formats::accolor::AcColor>,
    pub decal_layers: Vec<decal::PlacedDecal>,
    pub selected_decal_id: Option<u64>,
    pub ac_emblem_key: Option<String>,
    pub emblem_override: bool,
}

impl PaintSnapshot {
    pub fn capture(garage: &crate::Garage, decals: &DecalState) -> Self {
        Self {
            coloring_id: garage.active_design.coloring_id,
            custom_color: garage.custom_color,
            decal_layers: decals.layers.clone(),
            selected_decal_id: decals.selected_id,
            ac_emblem_key: decals.ac_emblem_key.clone(),
            emblem_override: decals.emblem_override,
        }
    }

    pub fn apply(&self, garage: &mut crate::Garage, decals: &mut DecalState) {
        if garage.active_design.coloring_id != self.coloring_id
            || garage.custom_color != self.custom_color
        {
            garage.active_design.coloring_id = self.coloring_id;
            garage.custom_color = self.custom_color;
            garage.dirty = true;
        }
        let decals_changed = decals.layers != self.decal_layers
            || decals.selected_id != self.selected_decal_id
            || decals.ac_emblem_key != self.ac_emblem_key
            || decals.emblem_override != self.emblem_override;
        if decals_changed {
            decals.layers = self.decal_layers.clone();
            decals.selected_id = self.selected_decal_id;
            decals.ac_emblem_key = self.ac_emblem_key.clone();
            decals.emblem_override = self.emblem_override;
            decals.emblem_texture_dirty = true;
            for slot in decal::DecalSlot::ALL {
                decals.mark_decal_slots_dirty(slot, false, true);
            }
            if let Some(did) = decals.loaded_design_id {
                decals.save_for_design(did);
            }
        }
    }
}

/// Session-scoped undo/redo stack for the Paint menu (active while in `GarageScreen::Paint`,
/// cleared when leaving it).
#[derive(Debug, Default)]
pub struct PaintUndoHistory {
    snapshots: Vec<PaintSnapshot>,
    cursor: usize,
    active: bool,
}

impl PaintUndoHistory {
    pub fn reset(&mut self) {
        self.snapshots.clear();
        self.cursor = 0;
        self.active = false;
    }

    pub fn begin(&mut self, initial: PaintSnapshot) {
        self.snapshots.clear();
        self.snapshots.push(initial);
        self.cursor = 0;
        self.active = true;
    }

    pub fn record(&mut self, snap: PaintSnapshot) {
        if !self.active {
            self.begin(snap);
            return;
        }
        if self.snapshots.get(self.cursor) == Some(&snap) {
            return;
        }
        self.snapshots.truncate(self.cursor + 1);
        self.snapshots.push(snap);
        if self.snapshots.len() > MAX_PAINT_HISTORY {
            self.snapshots.remove(0);
        }
        self.cursor = self.snapshots.len() - 1;
    }

    pub fn undo(&mut self) -> Option<&PaintSnapshot> {
        if self.active && self.cursor > 0 {
            self.cursor -= 1;
            self.snapshots.get(self.cursor)
        } else {
            None
        }
    }

    pub fn redo(&mut self) -> Option<&PaintSnapshot> {
        if self.active && self.cursor + 1 < self.snapshots.len() {
            self.cursor += 1;
            self.snapshots.get(self.cursor)
        } else {
            None
        }
    }
}

/// Tracks Paint menu session snapshots for `Ctrl+Z` / `Ctrl+Shift+Z` undo/redo, coalescing
/// continuous color-picker and decal drags into a single step and clearing on Paint exit.
pub fn sync_paint_history(
    mode: Res<Mode>,
    mut state: ResMut<GarageState>,
    garage: Res<crate::Garage>,
    decals: Res<DecalState>,
) {
    if *mode != Mode::Garage || state.screen != GarageScreen::Paint {
        if state.paint_history.active {
            state.paint_history.reset();
        }
        return;
    }
    if !state.paint_history.active {
        state
            .paint_history
            .begin(PaintSnapshot::capture(&garage, &decals));
        return;
    }
    // Coalesce continuous drags until mouse release.
    if state.paint_drag.is_some() || decals.dragging {
        return;
    }
    let snap = PaintSnapshot::capture(&garage, &decals);
    state.paint_history.record(snap);
}

fn current_decal_tint(decals: &DecalState) -> [u8; 4] {
    decals
        .selected_layer()
        .map(|d| d.tint)
        .unwrap_or(decals.brush_tint)
}

fn apply_decal_tint_color(decals: &mut DecalState, rgba: [u8; 4]) {
    let rgba = [rgba[0], rgba[1], rgba[2], 255];
    decals.brush_tint = rgba;
    if let Some(d) = decals.selected_layer_mut() {
        d.tint = rgba;
        let (slot, mirror) = (d.slot, d.mirror_x);
        decals.mark_decal_slots_dirty(slot, mirror, true);
        if let Some(did) = decals.loaded_design_id {
            decals.save_for_design(did);
        }
    }
}

/// Handles click-and-drag scrubbing (and hover-to-scroll) on the HSV (2D SV square + Hue bar) and
/// RGB (R/G/B bars) color picker tracks in `PAINT -> PALETTE` and `PAINT -> DECALS`.
#[allow(clippy::too_many_arguments)]
pub fn handle_paint_color_drag(
    mode: Res<Mode>,
    mut state: ResMut<GarageState>,
    mut garage: ResMut<crate::Garage>,
    mut decals: ResMut<DecalState>,
    mut settings: ResMut<GameSettings>,
    mut pause_state: ResMut<crate::pause::PauseState>,
    buttons: Res<ButtonInput<MouseButton>>,
    scroll: Res<AccumulatedMouseScroll>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    tracks: Query<(&ColorPickerTrack, &ComputedNode, &UiGlobalTransform)>,
    ac_meshes: Query<(&MeshMaterial3d<paint::AcPaintMaterial>, &paint::AcPaintGroup)>,
    mut paint_materials: ResMut<Assets<paint::AcPaintMaterial>>,
    mut picker_assets: ResMut<PaintPickerAssets>,
    mut images: ResMut<Assets<Image>>,
) {
    let in_hud_options = (*mode == Mode::Garage && state.screen == GarageScreen::Options)
        || (*mode == Mode::Sortie
            && pause_state.open
            && pause_state.screen == crate::pause::PauseScreen::Options);
    let in_decal_tab = !in_hud_options && state.paint_tab == PaintTab::Decals;
    if !in_hud_options
        && (*mode != Mode::Garage
            || state.screen != GarageScreen::Paint
            || !matches!(state.paint_tab, PaintTab::Custom | PaintTab::Decals))
    {
        state.paint_drag = None;
        return;
    }

    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor_logical) = window.cursor_position() else {
        return;
    };
    let cursor_phys = cursor_logical * window.scale_factor();

    let sync_key = if in_hud_options {
        usize::MAX - 1
    } else if in_decal_tab {
        usize::MAX
    } else {
        state.paint_channel
    };
    let active_rgba =
        |garage: &crate::Garage, decals: &DecalState, settings: &GameSettings, ch: usize| -> [u8; 4] {
            if in_hud_options {
                let c = settings.hud_color;
                [c[0], c[1], c[2], 255]
            } else if in_decal_tab {
                current_decal_tint(decals)
            } else {
                current_channel_color(garage, ch)
            }
        };

    // Hover-to-scroll on 1D color picker slider tracks (Hue bar and RGB gradient bars).
    if scroll.delta.y.abs() > 0.0 {
        let hovered_slider = tracks.iter().find_map(|(track, computed, tf)| {
            if track.0 == PaintDragTarget::HsvSv {
                return None;
            }
            let (_, inside) = normalize_track_point(tf.translation, computed.size(), cursor_phys);
            inside.then_some(track.0)
        });
        if let Some(target) = hovered_slider {
            state.wheel_consumed = true;
            let delta = wheel_step_direction(scroll.delta.y);
            let prev_rgba = active_rgba(&garage, &decals, &settings, state.paint_channel);
            if state.paint_hsv_synced_channel != Some(sync_key) {
                state.paint_hsv = paint::rgb_to_hsv(prev_rgba);
                state.paint_hsv_synced_channel = Some(sync_key);
            }
            let new_rgba = match target {
                PaintDragTarget::HsvHue => {
                    state.paint_hsv[0] = (state.paint_hsv[0] + delta as f32 * 5.0).rem_euclid(360.0);
                    picker_assets.ensure_loaded(state.paint_hsv[0], &mut images);
                    paint::hsv_to_rgb(state.paint_hsv[0], state.paint_hsv[1], state.paint_hsv[2])
                }
                PaintDragTarget::RgbR => {
                    let mut c = prev_rgba;
                    c[0] = (c[0] as i16 + delta as i16 * 8).clamp(0, 255) as u8;
                    state.paint_hsv = paint::rgb_to_hsv(c);
                    c
                }
                PaintDragTarget::RgbG => {
                    let mut c = prev_rgba;
                    c[1] = (c[1] as i16 + delta as i16 * 8).clamp(0, 255) as u8;
                    state.paint_hsv = paint::rgb_to_hsv(c);
                    c
                }
                PaintDragTarget::RgbB => {
                    let mut c = prev_rgba;
                    c[2] = (c[2] as i16 + delta as i16 * 8).clamp(0, 255) as u8;
                    state.paint_hsv = paint::rgb_to_hsv(c);
                    c
                }
                PaintDragTarget::HsvSv => prev_rgba,
            };
            if in_hud_options {
                settings.hud_color = [new_rgba[0], new_rgba[1], new_rgba[2]];
                settings.save();
                if *mode == Mode::Sortie {
                    pause_state.dirty = true;
                } else {
                    state.ui_dirty = true;
                }
            } else if in_decal_tab {
                apply_decal_tint_color(&mut decals, new_rgba);
                state.ui_dirty = true;
            } else {
                apply_custom_channel_color_live(
                    &mut garage,
                    state.paint_channel,
                    new_rgba,
                    &ac_meshes,
                    &mut paint_materials,
                );
                state.ui_dirty = true;
            }
            return;
        }
    }

    if !buttons.pressed(MouseButton::Left) {
        state.paint_drag = None;
        return;
    }

    if buttons.just_pressed(MouseButton::Left) {
        state.paint_drag = None;
        for (track, computed, tf) in &tracks {
            let (_, inside) = normalize_track_point(tf.translation, computed.size(), cursor_phys);
            if inside {
                state.paint_drag = Some(track.0);
                break;
            }
        }
    }

    let Some(active_target) = state.paint_drag else {
        return;
    };

    let Some((_, computed, tf)) = tracks.iter().find(|(t, _, _)| t.0 == active_target) else {
        return;
    };
    if computed.size().x <= 1.0 || computed.size().y <= 1.0 {
        return;
    }
    let (uv, _) = normalize_track_point(tf.translation, computed.size(), cursor_phys);

    let prev_rgba = active_rgba(&garage, &decals, &settings, state.paint_channel);
    if state.paint_hsv_synced_channel != Some(sync_key) {
        state.paint_hsv = paint::rgb_to_hsv(prev_rgba);
        state.paint_hsv_synced_channel = Some(sync_key);
    }

    let prev_hsv = state.paint_hsv;

    let new_rgba = match active_target {
        PaintDragTarget::HsvSv => {
            state.paint_hsv[1] = uv.x;
            state.paint_hsv[2] = 1.0 - uv.y;
            paint::hsv_to_rgb(state.paint_hsv[0], state.paint_hsv[1], state.paint_hsv[2])
        }
        PaintDragTarget::HsvHue => {
            // Clamp slightly below 360 so dragging to the far right edge stays cleanly at 359.9°.
            state.paint_hsv[0] = (uv.x * 360.0).min(359.9);
            picker_assets.ensure_loaded(state.paint_hsv[0], &mut images);
            paint::hsv_to_rgb(state.paint_hsv[0], state.paint_hsv[1], state.paint_hsv[2])
        }
        PaintDragTarget::RgbR => {
            let mut c = prev_rgba;
            c[0] = (uv.x * 255.0).round() as u8;
            state.paint_hsv = paint::rgb_to_hsv(c);
            c
        }
        PaintDragTarget::RgbG => {
            let mut c = prev_rgba;
            c[1] = (uv.x * 255.0).round() as u8;
            state.paint_hsv = paint::rgb_to_hsv(c);
            c
        }
        PaintDragTarget::RgbB => {
            let mut c = prev_rgba;
            c[2] = (uv.x * 255.0).round() as u8;
            state.paint_hsv = paint::rgb_to_hsv(c);
            c
        }
    };

    let hsv_changed = (state.paint_hsv[0] - prev_hsv[0]).abs() > 0.1
        || (state.paint_hsv[1] - prev_hsv[1]).abs() > 0.002
        || (state.paint_hsv[2] - prev_hsv[2]).abs() > 0.002;
    if new_rgba != prev_rgba || hsv_changed {
        if in_hud_options {
            settings.hud_color = [new_rgba[0], new_rgba[1], new_rgba[2]];
            settings.save();
            if *mode == Mode::Sortie {
                pause_state.dirty = true;
            } else {
                state.ui_dirty = true;
            }
        } else if in_decal_tab {
            apply_decal_tint_color(&mut decals, new_rgba);
            state.ui_dirty = true;
        } else {
            apply_custom_channel_color_live(
                &mut garage,
                state.paint_channel,
                new_rgba,
                &ac_meshes,
                &mut paint_materials,
            );
            state.ui_dirty = true;
        }
    }
}

/// Handles click-anywhere-to-set and click-and-drag scrubbing on every horizontal slider gauge bar
/// (`SliderTrack`) across Garage Options, Pause Options, Paint Custom Camo Scale, and Decal
/// Size/Rotate/Opacity.
#[allow(clippy::too_many_arguments)]
pub fn handle_slider_track_drag(
    mut commands: Commands,
    mode: Res<Mode>,
    mut state: ResMut<GarageState>,
    mut garage: ResMut<crate::Garage>,
    mut decals: ResMut<DecalState>,
    mut options: OptionsCtx,
    mut pause_state: ResMut<crate::pause::PauseState>,
    mut cues: ResMut<Cues>,
    mut audio_assets: ResMut<Assets<AudioSource>>,
    buttons: Res<ButtonInput<MouseButton>>,
    tracks: Query<(&SliderTrack, &ComputedNode, &UiGlobalTransform)>,
) {
    let in_pause_options = *mode == Mode::Sortie
        && pause_state.open
        && pause_state.screen == crate::pause::PauseScreen::Options;
    let in_garage_sliders = *mode == Mode::Garage
        && matches!(state.screen, GarageScreen::Options | GarageScreen::Paint);
    if !in_pause_options && !in_garage_sliders {
        state.slider_drag = None;
        return;
    }

    if !buttons.pressed(MouseButton::Left) {
        state.slider_drag = None;
        return;
    }

    let Ok(window) = options.windows.single() else {
        return;
    };
    let Some(cursor_logical) = window.cursor_position() else {
        return;
    };
    let cursor_phys = cursor_logical * window.scale_factor();

    let just_pressed = buttons.just_pressed(MouseButton::Left);
    if just_pressed {
        state.slider_drag = None;
        for (track, computed, tf) in &tracks {
            let size = computed.size();
            if size.x <= 1.0 || size.y <= 1.0 {
                continue;
            }
            let (_, inside) = normalize_track_point(tf.translation, size, cursor_phys);
            if inside {
                state.slider_drag = Some((track.0, tf.translation, size));
                break;
            }
        }
    }

    let Some((active_target, center_phys, size_phys)) = state.slider_drag else {
        return;
    };
    let (uv, _) = normalize_track_point(center_phys, size_phys, cursor_phys);
    let fill = uv.x;

    let mut changed = false;
    match active_target {
        SliderTrackTarget::UiScale => {
            let prev = options.settings.ui_scale_percent();
            options.settings.set_ui_scale_from_fill(fill);
            if options.settings.ui_scale_percent() != prev || just_pressed {
                options.settings.save();
                if in_pause_options {
                    pause_state.cursor = 1;
                    pause_state.dirty = true;
                } else {
                    state.cursor = 1;
                    state.ui_dirty = true;
                }
                changed = true;
            }
        }
        SliderTrackTarget::HudSway => {
            let prev = options.settings.hud_sway_percent();
            options.settings.set_hud_sway_from_fill(fill);
            if options.settings.hud_sway_percent() != prev || just_pressed {
                options.settings.save();
                if in_pause_options {
                    pause_state.cursor = 2;
                    pause_state.dirty = true;
                } else {
                    state.cursor = 2;
                    state.ui_dirty = true;
                }
                changed = true;
            }
        }
        SliderTrackTarget::Volume(channel) => {
            let prev = options.settings.channel_percent(channel);
            options.settings.set_channel_from_fill(channel, fill);
            if options.settings.channel_percent(channel) != prev || just_pressed {
                settings::apply_volume(
                    &options.settings,
                    &options.bgm,
                    &mut cues,
                    &mut options.global_volume,
                    &mut options.sinks,
                    &options.bgm_players,
                    &options.audio_buses,
                );
                options.settings.save();
                if let Some((row, _)) =
                    GARAGE_OPTION_CHANNELS.iter().find(|&&(_, c)| c == channel)
                {
                    if in_pause_options {
                        pause_state.cursor = *row;
                    } else {
                        state.cursor = *row;
                    }
                }
                if in_pause_options {
                    pause_state.dirty = true;
                } else {
                    state.ui_dirty = true;
                }
                changed = true;
            }
        }
        SliderTrackTarget::PaintCamoScale => {
            let prev = options.custom_camos.custom_scale_value();
            options.custom_camos.set_custom_scale_from_fill(fill);
            if (options.custom_camos.custom_scale_value() - prev).abs() > 1e-4 || just_pressed {
                if options.custom_camos.active_key.is_some() {
                    garage.dirty = true;
                }
                state.ui_dirty = true;
                changed = true;
            }
        }
        SliderTrackTarget::DecalScale => {
            let next = ((0.15 + fill * 2.85) * 100.0).round() / 100.0;
            let next = next.clamp(0.15, 3.0);
            if let Some(d) = decals.selected_layer_mut() {
                if (d.scale - next).abs() > 1e-4 || just_pressed {
                    d.scale = next;
                    let (slot, mirror) = (d.slot, d.mirror_x);
                    decals.brush_scale = next;
                    decals.mark_decal_slots_dirty(slot, mirror, true);
                    if let Some(did) = decals.loaded_design_id {
                        decals.save_for_design(did);
                    }
                    state.ui_dirty = true;
                    changed = true;
                }
            } else if (decals.brush_scale - next).abs() > 1e-4 || just_pressed {
                decals.brush_scale = next;
                state.ui_dirty = true;
                changed = true;
            }
        }
        SliderTrackTarget::DecalRotate => {
            let deg = (fill * 360.0).round().clamp(0.0, 359.0);
            let next = deg.to_radians();
            if let Some(d) = decals.selected_layer_mut() {
                if (d.rotation - next).abs() > 1e-4 || just_pressed {
                    d.rotation = next;
                    let (slot, mirror) = (d.slot, d.mirror_x);
                    decals.mark_decal_slots_dirty(slot, mirror, true);
                    if let Some(did) = decals.loaded_design_id {
                        decals.save_for_design(did);
                    }
                    state.ui_dirty = true;
                    changed = true;
                }
            }
        }
        SliderTrackTarget::DecalOpacity => {
            let next = ((fill * 100.0).round() / 100.0).clamp(0.1, 1.0);
            if let Some(d) = decals.selected_layer_mut() {
                if (d.opacity - next).abs() > 1e-4 || just_pressed {
                    d.opacity = next;
                    let (slot, mirror) = (d.slot, d.mirror_x);
                    decals.mark_decal_slots_dirty(slot, mirror, true);
                    if let Some(did) = decals.loaded_design_id {
                        decals.save_for_design(did);
                    }
                    state.ui_dirty = true;
                    changed = true;
                }
            }
        }
    }

    if changed && just_pressed {
        sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
    }
}

/// Shows the corner `X` delete button on custom decal/emblem thumbnail cells while hovered.
pub fn update_custom_thumb_delete_hover(
    cells: Query<&Interaction, Without<CustomThumbDeleteBtn>>,
    mut btns: Query<(
        &CustomThumbDeleteBtn,
        &Interaction,
        &mut Visibility,
        &mut BackgroundColor,
        &mut BorderColor,
    )>,
) {
    for (btn, btn_interaction, mut vis, mut bg, mut border) in &mut btns {
        let cell_hovered = cells
            .get(btn.cell)
            .is_ok_and(|i| matches!(*i, Interaction::Hovered | Interaction::Pressed));
        let self_hovered = matches!(*btn_interaction, Interaction::Hovered | Interaction::Pressed);
        let show = cell_hovered || self_hovered;
        let next_vis = if show {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *vis != next_vis {
            *vis = next_vis;
        }
        let next_bg = if self_hovered {
            Color::srgba(0.84, 0.16, 0.16, 0.96)
        } else {
            Color::srgba(0.18, 0.06, 0.06, 0.92)
        };
        let next_border = if self_hovered {
            Color::srgba(1.0, 0.55, 0.48, 0.98)
        } else {
            Color::srgba(0.85, 0.28, 0.24, 0.90)
        };
        if bg.0 != next_bg {
            bg.0 = next_bg;
        }
        let next_border_color = BorderColor::all(next_border);
        if *border != next_border_color {
            *border = next_border_color;
        }
    }
}

/// Rebuilds the interactive ACVD Garage HUD whenever `GarageState::ui_dirty` is set.
#[allow(clippy::too_many_arguments)]
pub fn sync_ui(
    mut commands: Commands,
    mode: Res<Mode>,
    mut state: ResMut<GarageState>,
    garage: Res<crate::Garage>,
    settings: Res<GameSettings>,
    mut decals: ResMut<DecalState>,
    mut custom_camos: ResMut<paint::CustomCamoLibrary>,
    mut assemble_assets: ResMut<crate::garage_assemble::AssembleUiAssets>,
    mut picker_assets: ResMut<PaintPickerAssets>,
    mut images: ResMut<Assets<Image>>,
    window: Query<&Window, With<bevy::window::PrimaryWindow>>,
    existing: Query<Entity, With<GarageUi>>,
) {
    if *mode != Mode::Garage {
        for e in &existing {
            commands.entity(e).despawn();
        }
        return;
    }
    let win_size = window
        .single()
        .map(|w| Vec2::new(w.width(), w.height()))
        .unwrap_or(Vec2::new(1280.0, 720.0));
    let resized = (win_size - state.last_window_size).length_squared() > 1.0;
    let ui_scale_changed = (settings.ui_scale - state.last_ui_scale).abs() > 1e-4;
    if !state.ui_dirty && !resized && !ui_scale_changed && !existing.is_empty() {
        return;
    }
    state.ui_dirty = false;
    state.last_window_size = win_size;
    state.last_ui_scale = settings.ui_scale;
    decals.init_assets(&mut images);
    custom_camos.init_assets(&mut images);
    if state.screen == GarageScreen::Paint
        && matches!(state.paint_tab, PaintTab::Decals | PaintTab::Emblem)
        && decals.library_tab == DecalLibraryTab::Emblems
    {
        decals.warm_emblem_page(&garage.disc, &mut images, DECAL_GRID_PAGE_SIZE);
    }
    for e in &existing {
        commands.entity(e).despawn();
    }

    // Authentic retail `lang/en/menu/assemble.drb.dcx` UI + real thumbnails + live 3D part preview.
    if let GarageScreen::AssembleParts { slot } = state.screen {
        // When launched directly into a part slot via CLI (`--garage-screen weapon_r`, etc.),
        // snap the top category tab scroll and vertical part cursor to the currently equipped part.
        if state.saved_design.is_none() {
            state.saved_design = Some(garage.active_design);
            let sidx = AssembleSlot::ALL
                .iter()
                .position(|&s| s == slot)
                .unwrap_or(0);
            state.slot_cursor = sidx;
            clamp_scroll_win(
                &mut state.assemble_cat_scroll,
                sidx,
                AssembleSlot::ALL.len(),
                crate::garage_assemble::ASSEMBLE_CAT_WINDOW,
            );
            let parts = crate::garage_assemble::filtered_parts_for_slot(
                slot,
                state.assemble_subcat,
                state.assemble_dmg_filter,
            );
            let equipped = slot.get(&garage.active_design);
            if let Some(pos) = parts.iter().position(|&id| id == equipped) {
                state.cursor = pos;
                clamp_scroll_win(
                    &mut state.scroll,
                    pos,
                    parts.len(),
                    crate::garage_assemble::ASSEMBLE_THUMB_WINDOW,
                );
            }
        }
        if crate::garage_assemble::spawn_assemble_ui(
            &mut commands,
            &mut assemble_assets,
            &state,
            &garage,
            slot,
            win_size,
            &mut images,
        ) {
            return;
        }
    }

    // Load retail ACVD CCM fonts & chrome sprites once (shared with Assemble & Pause menus).
    assemble_assets.ensure_loaded(&garage.disc, &mut images);
    let kit = assemble_assets.kit.clone();
    let scale = ((win_size.x / 1280.0).min(win_size.y / 720.0) * settings.ui_scale)
        .clamp(0.55, 2.2);

    let bg_row = Color::srgba(0.09, 0.12, 0.16, 0.65);
    let bg_sel = Color::srgba(0.88, 0.58, 0.14, 0.92);
    let fg_text = Color::srgb(0.90, 0.93, 0.96);
    let fg_dim = Color::srgb(0.58, 0.66, 0.74);
    let fg_cyan = Color::srgb(0.38, 0.86, 0.96);
    let fg_amber = Color::srgb(0.98, 0.74, 0.24);
    let fg_dark = Color::srgb(0.06, 0.07, 0.09);

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
        spawn_garage_view_mode_bar(&mut commands, root, &kit, scale);
        return;
    }

    let ws_label = state
        .active_workshop()
        .map(|w| format!("{} - {}", w.title, w.theme))
        .unwrap_or_else(|| "Workshop 5 - Underpass".to_string());

    // Top bar: ACVD GARAGE title + active design + workshop name.
    spawn_garage_top_bar(
        &mut commands,
        root,
        &kit,
        state.screen,
        &garage.active_name,
        &ws_label,
        scale,
    );

    // Transient feedback banner (top-center).
    if let Some((ref msg, col, _)) = state.banner {
        spawn_garage_banner(&mut commands, root, &kit, msg, col, scale);
    }

    // Left interactive menu panel.
    if state.screen == GarageScreen::Top {
        spawn_garage_top_menu(&mut commands, root, &kit, state.cursor, scale);
    } else if state.screen == GarageScreen::Options {
        if state.paint_hsv_synced_channel != Some(usize::MAX - 1) {
            let c = settings.hud_color;
            state.paint_hsv = paint::rgb_to_hsv([c[0], c[1], c[2], 255]);
            state.paint_hsv_synced_channel = Some(usize::MAX - 1);
        }
        picker_assets.ensure_loaded(state.paint_hsv[0], &mut images);
        spawn_garage_options_menu(
            &mut commands,
            root,
            &kit,
            &settings,
            &state,
            &picker_assets,
            state.cursor,
            scale,
        );
    } else if state.screen == GarageScreen::Paint {
        if state.paint_tab == PaintTab::Custom {
            if state.paint_hsv_synced_channel != Some(state.paint_channel) {
                state.paint_hsv =
                    paint::rgb_to_hsv(current_channel_color(&garage, state.paint_channel));
                state.paint_hsv_synced_channel = Some(state.paint_channel);
            }
            picker_assets.ensure_loaded(state.paint_hsv[0], &mut images);
            picker_assets.ensure_camo_thumbs(&garage.disc, &mut images);
        } else if state.paint_tab == PaintTab::Decals {
            if state.paint_hsv_synced_channel != Some(usize::MAX) {
                state.paint_hsv = paint::rgb_to_hsv(current_decal_tint(&decals));
                state.paint_hsv_synced_channel = Some(usize::MAX);
            }
            picker_assets.ensure_loaded(state.paint_hsv[0], &mut images);
        }
        spawn_garage_paint_menu(
            &mut commands,
            root,
            &kit,
            &state,
            &garage,
            &decals,
            &custom_camos,
            &picker_assets,
            scale,
        );
    } else {
        let left_panel = kit.spawn_panel(
            &mut commands,
            Some(root),
            PanelStyle::Tactical,
            Node {
                position_type: PositionType::Absolute,
                left: px((20.0 * scale).round()),
                top: px((66.0 * scale).round()),
                width: px((370.0 * scale).round()),
                padding: UiRect::all(px((12.0 * scale).round())),
                flex_direction: FlexDirection::Column,
                row_gap: px((4.0 * scale).round()),
                ..default()
            },
            scale,
        );
        commands.entity(left_panel).with_children(|panel| {
            match state.screen {
                GarageScreen::Top => {}
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
                    spawn_list_pager(
                        panel,
                        start,
                        end,
                        AssembleSlot::ALL.len(),
                        bg_row,
                        fg_cyan,
                        fg_dim,
                    );
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
                    spawn_list_pager(panel, start, end, parts.len(), bg_row, fg_cyan, fg_dim);
                    spawn_back_btn(panel, "< CANCEL / BACK (Esc)", bg_row, fg_cyan);
                }
                GarageScreen::Paint => {}
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
                    spawn_list_pager(
                        panel,
                        start,
                        end,
                        garage.designs.len(),
                        bg_row,
                        fg_cyan,
                        fg_dim,
                    );
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
                    spawn_list_pager(
                        panel,
                        start,
                        end,
                        state.workshops.len(),
                        bg_row,
                        fg_cyan,
                        fg_dim,
                    );
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
                GarageScreen::Options => {}
                GarageScreen::ViewMode => {}
            }
        });
    }

    // Right panel: Decal Inspector & Layers when in `PAINT -> DECALS`, otherwise AC SPECS telemetry.
    let in_decal_mode =
        state.screen == GarageScreen::Paint && state.paint_tab == PaintTab::Decals;

    if in_decal_mode {
        spawn_garage_decal_inspector(&mut commands, root, &kit, &state, &decals, &picker_assets, scale);
    } else {
        // Right AC SPECS telemetry panel (live `AcCtrlParam` calculation + preview delta!).
        let cur_specs = AcSpecs::calculate(&garage.active_design);
        let base_specs = state
            .saved_design
            .as_ref()
            .map(AcSpecs::calculate)
            .unwrap_or(cur_specs);
        spawn_garage_specs_panel(&mut commands, root, &kit, cur_specs, base_specs, scale);
    }

    // Bottom key-guide footer.
    let footer_text = if in_decal_mode {
        "L-DRAG PLACE/MOVE   R-DRAG ORBIT   SHIFT+WHEEL SIZE   CTRL+WHEEL ROTATE   M MIRROR   F FLIP   DEL REMOVE"
    } else if state.screen == GarageScreen::Paint && state.paint_tab == PaintTab::Custom {
        "L-DRAG COLOR PICKER   W/S CHANNEL   TAB NEXT TAB   R-DRAG ORBIT   WHEEL ZOOM   ESC BACK"
    } else if state.screen == GarageScreen::Options {
        "W/S NAVIGATE   ENTER/LEFT/RIGHT ADJUST   ESC CLOSE   R-DRAG ORBIT   F9 MUTE BGM"
    } else if state.screen == GarageScreen::Top {
        "W/S NAVIGATE   ENTER SELECT   ESC OPTIONS   R-DRAG ORBIT   WHEEL ZOOM   V VIEW MODE   F9 MUTE BGM"
    } else {
        "W/S NAVIGATE   ENTER SELECT   ESC BACK   R-DRAG ORBIT   WHEEL ZOOM   V VIEW MODE   F9 MUTE BGM"
    };
    spawn_garage_footer(&mut commands, root, &kit, footer_text, scale);

    if state.screen == GarageScreen::Paint && state.paint_exit_prompt {
        spawn_paint_exit_prompt(
            &mut commands,
            root,
            &kit,
            state.paint_exit_prompt_choice,
            scale,
        );
    }
}

/// Modal YES / NO confirmation prompt rendered with `AcvdUiKit` when pressing ESC in Paint
/// with unsaved paint changes.
fn spawn_paint_exit_prompt(
    commands: &mut Commands,
    root: Entity,
    kit: &AcvdUiKit,
    save_selected: bool,
    scale: f32,
) {
    let overlay = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: px(0.0),
                left: px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            ZIndex(20),
            BackgroundColor(Color::srgba(0.02, 0.03, 0.04, 0.68)),
            ChildOf(root),
        ))
        .id();

    let modal = kit.spawn_panel(
        commands,
        Some(overlay),
        PanelStyle::Framed,
        Node {
            width: px((420.0 * scale).round()),
            padding: UiRect::axes(px((22.0 * scale).round()), px((18.0 * scale).round())),
            flex_direction: FlexDirection::Column,
            row_gap: px((12.0 * scale).round()),
            align_items: AlignItems::Center,
            ..default()
        },
        scale,
    );

    kit.spawn_text(
        commands,
        modal,
        FontRole::Header,
        "SAVE BEFORE EXIT?",
        scale * 1.05,
        palette::ACCENT_AMBER,
    );

    kit.spawn_text(
        commands,
        modal,
        FontRole::Body,
        "Your paint configuration has unsaved changes.",
        scale * 0.90,
        palette::TEXT_PRIMARY,
    );

    let btn_row = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                margin: UiRect::top(px((4.0 * scale).round())),
                flex_direction: FlexDirection::Row,
                column_gap: px((10.0 * scale).round()),
                ..default()
            },
            ChildOf(modal),
        ))
        .id();

    let yes_btn = spawn_kit_pill(
        commands,
        kit,
        btn_row,
        "YES — SAVE",
        if save_selected {
            ButtonState::Active
        } else {
            ButtonState::Normal
        },
        1.0,
        14.0,
        8.0,
        0.94,
        scale,
    );
    commands
        .entity(yes_btn)
        .insert((Button, UiAction::PaintExitSave));

    let no_btn = spawn_kit_pill(
        commands,
        kit,
        btn_row,
        "NO — DISCARD",
        if !save_selected {
            ButtonState::Active
        } else {
            ButtonState::Normal
        },
        1.0,
        14.0,
        8.0,
        0.94,
        scale,
    );
    commands
        .entity(no_btn)
        .insert((Button, UiAction::PaintExitDiscard));

    kit.spawn_text(
        commands,
        modal,
        FontRole::Small,
        "ENTER CONFIRM   •   LEFT/RIGHT SELECT   •   ESC CANCEL",
        scale * 0.78,
        palette::TEXT_MUTED,
    );
}

fn screen_title_and_tag(screen: GarageScreen) -> (&'static str, &'static str) {
    match screen {
        GarageScreen::Top => ("GARAGE", "MAIN MENU"),
        GarageScreen::AssembleSlots => ("ASSEMBLE", "SLOTS"),
        GarageScreen::AssembleParts { slot } => (slot.label(), "ASSEMBLE"),
        GarageScreen::Paint => ("PAINT", "CUSTOMIZE"),
        GarageScreen::Design => ("AC DESIGN", "BLUEPRINTS"),
        GarageScreen::GarageCustom => ("GARAGE CUSTOM", "WORKSHOPS"),
        GarageScreen::SortieConfig => ("AC TEST", "SORTIE"),
        GarageScreen::Options => ("OPTIONS", "SETTINGS"),
        GarageScreen::ViewMode => ("VIEW MODE", "INSPECT"),
    }
}

fn spawn_garage_top_bar(
    commands: &mut Commands,
    root: Entity,
    kit: &AcvdUiKit,
    screen: GarageScreen,
    ac_name: &str,
    ws_label: &str,
    scale: f32,
) {
    let (title, tag) = screen_title_and_tag(screen);
    let pad_x = (14.0 * scale).round();
    let pad_y = (6.0 * scale).round();
    let bar = kit.spawn_panel(
        commands,
        Some(root),
        PanelStyle::Framed,
        Node {
            position_type: PositionType::Absolute,
            left: px((20.0 * scale).round()),
            right: px((20.0 * scale).round()),
            top: px((12.0 * scale).round()),
            min_height: px((40.0 * scale).round()),
            padding: UiRect::axes(px(pad_x), px(pad_y)),
            flex_direction: FlexDirection::Row,
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Center,
            column_gap: px((12.0 * scale).round()),
            ..default()
        },
        scale,
    );

    // Subtle amber accent bar along the left edge of the header strip.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: px(0.0),
            top: px(0.0),
            bottom: px(0.0),
            width: px((3.0 * scale).round().max(2.0)),
            ..default()
        },
        BackgroundColor(palette::ACCENT_AMBER),
        ChildOf(bar),
    ));

    // Left: marker + ACVD // <SCREEN> + mode badge.
    let left = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: px((10.0 * scale).round()),
                ..default()
            },
            ChildOf(bar),
        ))
        .id();

    let sq = (7.0 * scale).round().max(4.0);
    commands.spawn((
        Node {
            width: px(sq),
            height: px(sq),
            flex_shrink: 0.0,
            ..default()
        },
        BackgroundColor(palette::ACCENT_AMBER),
        ChildOf(left),
    ));

    kit.spawn_text(
        commands,
        left,
        FontRole::Header,
        &format!("ACVD  {title}"),
        scale,
        palette::TEXT_BRIGHT,
    );
    kit.spawn_badge(commands, left, tag, palette::ACCENT_TEAL, true, scale);

    // Right: active AC identity + workshop tag.
    let right = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: px((10.0 * scale).round()),
                ..default()
            },
            ChildOf(bar),
        ))
        .id();

    kit.spawn_badge(commands, right, "AC", palette::ACCENT_AMBER, false, scale);
    kit.spawn_text(
        commands,
        right,
        FontRole::Body,
        ac_name,
        scale * 0.92,
        palette::TEXT_PRIMARY,
    );

    // Thin vertical separator.
    commands.spawn((
        Node {
            width: px((1.0 * scale).max(1.0)),
            height: px((16.0 * scale).round()),
            ..default()
        },
        BackgroundColor(palette::BORDER_BUTTON),
        ChildOf(right),
    ));

    kit.spawn_text(
        commands,
        right,
        FontRole::Small,
        ws_label,
        scale * 0.88,
        palette::TEXT_PRIMARY,
    );
}

fn spawn_garage_banner(
    commands: &mut Commands,
    root: Entity,
    kit: &AcvdUiKit,
    msg: &str,
    col: Color,
    scale: f32,
) {
    let w = (440.0 * scale).round();
    let banner = kit.spawn_panel(
        commands,
        Some(root),
        PanelStyle::Framed,
        Node {
            position_type: PositionType::Absolute,
            top: px((60.0 * scale).round()),
            left: Val::Percent(50.0),
            margin: UiRect::left(px(-w * 0.5)),
            width: px(w),
            padding: UiRect::axes(px((14.0 * scale).round()), px((6.0 * scale).round())),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        scale,
    );
    commands.entity(banner).insert(BorderColor::all(col));
    kit.spawn_text(commands, banner, FontRole::Body, msg, scale * 0.92, col);
}

fn spawn_garage_top_menu(
    commands: &mut Commands,
    root: Entity,
    kit: &AcvdUiKit,
    cursor: usize,
    scale: f32,
) {
    let pad = (14.0 * scale).round();
    let panel = kit.spawn_panel(
        commands,
        Some(root),
        PanelStyle::Tactical,
        Node {
            position_type: PositionType::Absolute,
            left: px((20.0 * scale).round()),
            top: px((64.0 * scale).round()),
            width: px((336.0 * scale).round()),
            padding: UiRect::all(px(pad)),
            flex_direction: FlexDirection::Column,
            row_gap: px((8.0 * scale).round()),
            ..default()
        },
        scale,
    );

    kit.spawn_header(commands, panel, "GARAGE MENU", Some("SELECT"), scale);

    let list = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                row_gap: px((6.0 * scale).round()),
                margin: UiRect::vertical(px((2.0 * scale).round())),
                ..default()
            },
            ChildOf(panel),
        ))
        .id();

    for (idx, &(title, tag, _desc)) in TOP_ITEMS.iter().enumerate() {
        let btn_state = if cursor == idx {
            ButtonState::Selected
        } else {
            ButtonState::Normal
        };
        let btn = kit.spawn_button(commands, list, title, Some(tag), btn_state, scale);
        commands.entity(btn).insert((Button, UiRow(idx)));
    }

    kit.spawn_trace(commands, panel, TraceStyle::Stepped, scale);

    // Retail-style helpline card explaining the currently focused menu item.
    let active_desc = TOP_ITEMS
        .get(cursor)
        .map(|&(_, _, d)| d)
        .unwrap_or("");
    let help = kit.spawn_panel(
        commands,
        Some(panel),
        PanelStyle::Subtle,
        Node {
            width: Val::Percent(100.0),
            padding: UiRect::axes(px((10.0 * scale).round()), px((7.0 * scale).round())),
            flex_direction: FlexDirection::Column,
            row_gap: px((3.0 * scale).round()),
            ..default()
        },
        scale,
    );
    kit.spawn_text(
        commands,
        help,
        FontRole::Small,
        "DESCRIPTION",
        scale * 0.9,
        palette::TEXT_MUTED,
    );
    kit.spawn_text(
        commands,
        help,
        FontRole::Small,
        active_desc,
        scale * 0.86,
        palette::TEXT_BRIGHT,
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_garage_options_menu(
    commands: &mut Commands,
    root: Entity,
    kit: &AcvdUiKit,
    settings: &GameSettings,
    state: &GarageState,
    picker_assets: &PaintPickerAssets,
    cursor: usize,
    scale: f32,
) {
    let pad = (14.0 * scale).round();
    let panel = kit.spawn_panel(
        commands,
        Some(root),
        PanelStyle::Tactical,
        Node {
            position_type: PositionType::Absolute,
            left: px((20.0 * scale).round()),
            top: px((64.0 * scale).round()),
            width: px((372.0 * scale).round()),
            padding: UiRect::all(px(pad)),
            flex_direction: FlexDirection::Column,
            row_gap: px((7.0 * scale).round()),
            ..default()
        },
        scale,
    );

    kit.spawn_header(commands, panel, "OPTIONS", Some("SETTINGS"), scale);

    let list = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                row_gap: px((5.0 * scale).round()),
                margin: UiRect::vertical(px((2.0 * scale).round())),
                ..default()
            },
            ChildOf(panel),
        ))
        .id();

    kit.spawn_text(
        commands,
        list,
        FontRole::Small,
        "DISPLAY",
        scale * 0.9,
        palette::TEXT_MUTED,
    );

    // Row 0: Fullscreen toggle
    let fs_tag = if settings.fullscreen { "ON" } else { "OFF" };
    let fs_state = if cursor == 0 {
        ButtonState::Selected
    } else {
        ButtonState::Normal
    };
    let fs_btn = kit.spawn_button(commands, list, "FULLSCREEN", Some(fs_tag), fs_state, scale);
    commands
        .entity(fs_btn)
        .insert((Button, UiAction::ToggleFullscreen));

    // Row 1: UI Scale slider (75% - 150%, I marker at 100%)
    let ui_scale_txt = format!("{}%", settings.ui_scale_percent());
    let ui_slider = kit.spawn_slider_row(
        commands,
        list,
        "UI SCALE",
        &ui_scale_txt,
        settings.ui_scale_fill(),
        Some(GameSettings::ui_scale_default_mark()),
        cursor == 1,
        104.0,
        scale,
    );
    commands
        .entity(ui_slider.row)
        .insert((Button, UiAction::SelectOptionRow(1)));
    commands
        .entity(ui_slider.track)
        .insert(SliderTrack(SliderTrackTarget::UiScale));
    commands
        .entity(ui_slider.dec_btn)
        .insert((Button, UiAction::UiScaleStep(-1)));
    commands
        .entity(ui_slider.inc_btn)
        .insert((Button, UiAction::UiScaleStep(1)));

    // Row 2: Crosshair HUD Sway slider (0% - 100%)
    let sway_txt = format!("{}%", settings.hud_sway_percent());
    let sway_slider = kit.spawn_slider_row(
        commands,
        list,
        "HUD SWAY",
        &sway_txt,
        settings.hud_sway.clamp(0.0, 1.0),
        None,
        cursor == 2,
        104.0,
        scale,
    );
    commands
        .entity(sway_slider.row)
        .insert((Button, UiAction::SelectOptionRow(2)));
    commands
        .entity(sway_slider.track)
        .insert(SliderTrack(SliderTrackTarget::HudSway));
    commands
        .entity(sway_slider.dec_btn)
        .insert((Button, UiAction::HudSwayStep(-1)));
    commands
        .entity(sway_slider.inc_btn)
        .insert((Button, UiAction::HudSwayStep(1)));

    // Combat HUD Color HSV / RGB picker
    kit.spawn_text(
        commands,
        list,
        FontRole::Small,
        "COMBAT HUD COLOR",
        scale * 0.86,
        palette::TEXT_MUTED,
    );
    let hc = settings.hud_color;
    spawn_color_picker_card(
        commands,
        list,
        kit,
        state.paint_picker_mode,
        [hc[0], hc[1], hc[2], 255],
        state.paint_hsv,
        picker_assets,
        64.0,
        scale * 0.88,
    );

    kit.spawn_trace(commands, list, TraceStyle::Simple, scale);

    kit.spawn_text(
        commands,
        list,
        FontRole::Small,
        "SOUND SETTINGS",
        scale * 0.9,
        palette::TEXT_MUTED,
    );

    for &(row_idx, channel) in &GARAGE_OPTION_CHANNELS {
        let pct_txt = format!("{}%", settings.channel_percent(channel));
        let slider = kit.spawn_slider_row(
            commands,
            list,
            channel.short_label(),
            &pct_txt,
            settings.channel_volume(channel),
            None,
            cursor == row_idx,
            104.0,
            scale,
        );
        commands
            .entity(slider.row)
            .insert((Button, UiAction::SelectOptionRow(row_idx)));
        commands
            .entity(slider.track)
            .insert(SliderTrack(SliderTrackTarget::Volume(channel)));
        commands
            .entity(slider.dec_btn)
            .insert((Button, UiAction::VolumeStep(channel, -1)));
        commands
            .entity(slider.inc_btn)
            .insert((Button, UiAction::VolumeStep(channel, 1)));
    }

    kit.spawn_trace(commands, list, TraceStyle::Simple, scale);

    // Row 7: Back to Garage Top
    let back_state = if cursor == 7 {
        ButtonState::Selected
    } else {
        ButtonState::Normal
    };
    let back_btn = kit.spawn_button(
        commands,
        list,
        "BACK TO GARAGE",
        Some("ESC"),
        back_state,
        scale,
    );
    commands.entity(back_btn).insert((Button, UiAction::Back));

    // Row 8: Quit to Desktop
    let quit_state = if cursor == 8 {
        ButtonState::Selected
    } else {
        ButtonState::Normal
    };
    let quit_btn = kit.spawn_button(
        commands,
        list,
        "QUIT TO DESKTOP",
        None,
        quit_state,
        scale,
    );
    commands
        .entity(quit_btn)
        .insert((Button, UiAction::QuitToDesktop));

    kit.spawn_trace(commands, panel, TraceStyle::Stepped, scale);

    // Retail-style helpline card explaining the currently focused option.
    let active_desc = OPTIONS_ROW_DESCS.get(cursor).copied().unwrap_or("");
    let help = kit.spawn_panel(
        commands,
        Some(panel),
        PanelStyle::Subtle,
        Node {
            width: Val::Percent(100.0),
            padding: UiRect::axes(px((10.0 * scale).round()), px((7.0 * scale).round())),
            flex_direction: FlexDirection::Column,
            row_gap: px((3.0 * scale).round()),
            ..default()
        },
        scale,
    );
    kit.spawn_text(
        commands,
        help,
        FontRole::Small,
        "DESCRIPTION",
        scale * 0.9,
        palette::TEXT_MUTED,
    );
    kit.spawn_text(
        commands,
        help,
        FontRole::Small,
        active_desc,
        scale * 0.86,
        palette::TEXT_BRIGHT,
    );
}

/// Compact UI-kit shaded pill button (used for tabs, sub-tabs, mode toggles, and compact actions).
fn spawn_kit_pill(
    commands: &mut Commands,
    kit: &AcvdUiKit,
    parent: Entity,
    label: &str,
    state: ButtonState,
    flex_grow: f32,
    pad_x: f32,
    pad_y: f32,
    text_scale: f32,
    scale: f32,
) -> Entity {
    let bw = (1.0 * scale).max(1.0);
    let (top_c, bot_c, border_c, text_c) = match state {
        ButtonState::Normal => (
            palette::BG_BUTTON_TOP,
            palette::BG_BUTTON_BOT,
            palette::BORDER_BUTTON,
            palette::TEXT_PRIMARY,
        ),
        ButtonState::Selected => (
            palette::ACCENT_AMBER_FILL_TOP,
            palette::ACCENT_AMBER_FILL_BOT,
            palette::ACCENT_AMBER,
            palette::TEXT_BRIGHT,
        ),
        ButtonState::Active => (
            palette::ACCENT_TEAL_FILL_TOP,
            palette::ACCENT_TEAL_FILL_BOT,
            palette::ACCENT_TEAL,
            palette::TEXT_BRIGHT,
        ),
        ButtonState::Disabled => (
            palette::BG_BUTTON_DISABLED,
            palette::BG_BUTTON_DISABLED,
            palette::BORDER_SUBTLE,
            palette::TEXT_MUTED,
        ),
    };
    let btn = commands
        .spawn((
            Node {
                flex_grow,
                padding: UiRect::axes(px((pad_x * scale).round()), px((pad_y * scale).round())),
                border: UiRect::all(px(bw)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundGradient::from(LinearGradient::to_bottom(vec![
                ColorStop::auto(top_c),
                ColorStop::auto(bot_c),
            ])),
            BorderColor::all(border_c),
            ChildOf(parent),
        ))
        .id();
    kit.spawn_text(commands, btn, FontRole::Small, label, scale * text_scale, text_c);
    btn
}

/// Compact UI-kit arrow stepper button (`<` or `>`).
fn spawn_kit_arrow_btn(
    commands: &mut Commands,
    kit: &AcvdUiKit,
    parent: Entity,
    dir: ArrowDir,
    label: Option<&str>,
    action: UiAction,
    scale: f32,
) -> Entity {
    let bw = (1.0 * scale).max(1.0);
    let btn = commands
        .spawn((
            Button,
            action,
            Node {
                padding: UiRect::axes(px((7.0 * scale).round()), px((3.5 * scale).round())),
                border: UiRect::all(px(bw)),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                column_gap: px((5.0 * scale).round()),
                flex_shrink: 0.0,
                ..default()
            },
            BackgroundGradient::from(LinearGradient::to_bottom(vec![
                ColorStop::auto(palette::BG_BUTTON_TOP),
                ColorStop::auto(palette::BG_BUTTON_BOT),
            ])),
            BorderColor::all(palette::BORDER_BUTTON),
            ChildOf(parent),
        ))
        .id();
    if dir == ArrowDir::Left {
        kit.spawn_arrow(commands, btn, ArrowDir::Left, scale * 0.78, palette::TEXT_PRIMARY);
    }
    if let Some(txt) = label {
        kit.spawn_text(commands, btn, FontRole::Small, txt, scale * 0.86, palette::TEXT_PRIMARY);
    }
    if dir == ArrowDir::Right {
        kit.spawn_arrow(commands, btn, ArrowDir::Right, scale * 0.78, palette::TEXT_PRIMARY);
    }
    btn
}

#[allow(clippy::too_many_arguments)]
fn spawn_garage_paint_menu(
    commands: &mut Commands,
    root: Entity,
    kit: &AcvdUiKit,
    state: &GarageState,
    garage: &crate::Garage,
    decals: &DecalState,
    custom_camos: &paint::CustomCamoLibrary,
    picker_assets: &PaintPickerAssets,
    scale: f32,
) {
    let pad = (11.0 * scale).round();
    let panel = kit.spawn_panel(
        commands,
        Some(root),
        PanelStyle::Tactical,
        Node {
            position_type: PositionType::Absolute,
            left: px((20.0 * scale).round()),
            top: px((64.0 * scale).round()),
            width: px((392.0 * scale).round()),
            padding: UiRect::all(px(pad)),
            flex_direction: FlexDirection::Column,
            row_gap: px((5.0 * scale).round()),
            ..default()
        },
        scale,
    );

    let tab_tag = match state.paint_tab {
        PaintTab::Presets => "PRESETS",
        PaintTab::Custom => "PALETTE",
        PaintTab::Decals => "DECALS",
        PaintTab::Emblem => "EMBLEM",
    };
    kit.spawn_header(commands, panel, "PAINT", Some(tab_tag), scale);

    // Top tab strip: PRESETS | PALETTE | DECALS | EMBLEM
    let tab_bar = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                column_gap: px((4.0 * scale).round()),
                ..default()
            },
            ChildOf(panel),
        ))
        .id();
    for (tab, label) in [
        (PaintTab::Presets, "PRESETS"),
        (PaintTab::Custom, "PALETTE"),
        (PaintTab::Decals, "DECALS"),
        (PaintTab::Emblem, "EMBLEM"),
    ] {
        let btn_state = if state.paint_tab == tab {
            ButtonState::Selected
        } else {
            ButtonState::Normal
        };
        let b = spawn_kit_pill(commands, kit, tab_bar, label, btn_state, 1.0, 5.0, 4.5, 0.90, scale);
        commands.entity(b).insert((Button, UiAction::PaintTab(tab)));
    }

    kit.spawn_trace(commands, panel, TraceStyle::Simple, scale);

    match state.paint_tab {
        PaintTab::Presets => {
            spawn_paint_presets_tab(commands, kit, panel, state, garage, scale);
        }
        PaintTab::Custom => {
            spawn_paint_palette_tab(
                commands,
                kit,
                panel,
                state,
                garage,
                custom_camos,
                picker_assets,
                scale,
            );
        }
        PaintTab::Decals => {
            spawn_paint_decals_tab(commands, kit, panel, decals, scale);
        }
        PaintTab::Emblem => {
            spawn_paint_emblem_tab(commands, kit, panel, decals, scale);
        }
    }

    kit.spawn_trace(commands, panel, TraceStyle::Simple, scale);
    let back_btn = kit.spawn_button(
        commands,
        panel,
        "BACK TO GARAGE",
        Some("ESC"),
        ButtonState::Normal,
        scale * 0.92,
    );
    commands.entity(back_btn).insert((Button, UiAction::Back));
}

fn spawn_paint_presets_tab(
    commands: &mut Commands,
    kit: &AcvdUiKit,
    panel: Entity,
    state: &GarageState,
    garage: &crate::Garage,
    scale: f32,
) {
    let bw = (1.0 * scale).max(1.0);
    let list = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                row_gap: px((3.0 * scale).round()),
                ..default()
            },
            ChildOf(panel),
        ))
        .id();

    let start = state.scroll;
    let end = (start + LIST_WINDOW).min(state.color_presets.len());
    for idx in start..end {
        let cid = state.color_presets[idx];
        let sel = state.cursor == idx;
        let preview = paint::load_accolor(&garage.disc, cid);
        let (top_c, bot_c, border_c, fg_c) = if sel {
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
                Button,
                UiRow(idx),
                Node {
                    width: Val::Percent(100.0),
                    padding: UiRect::axes(px((8.0 * scale).round()), px((3.5 * scale).round())),
                    border: UiRect::all(px(bw)),
                    flex_direction: FlexDirection::Row,
                    justify_content: JustifyContent::SpaceBetween,
                    align_items: AlignItems::Center,
                    ..default()
                },
                BackgroundGradient::from(LinearGradient::to_bottom(vec![
                    ColorStop::auto(top_c),
                    ColorStop::auto(bot_c),
                ])),
                BorderColor::all(border_c),
                ChildOf(list),
            ))
            .id();

        let left = commands
            .spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: px((5.0 * scale).round()),
                    ..default()
                },
                ChildOf(row),
            ))
            .id();
        if sel {
            if let Some(arrow) = &kit.focus_arrow {
                let s = arrow.size() * (0.48 * scale);
                arrow.spawn(
                    commands,
                    left,
                    Node {
                        width: px(s.x.round()),
                        height: px(s.y.round()),
                        flex_shrink: 0.0,
                        ..default()
                    },
                    palette::TEXT_BRIGHT,
                    true,
                    false,
                    true,
                );
            }
        }
        kit.spawn_text(
            commands,
            left,
            FontRole::Body,
            &format!("COLOR PRESET {cid:04}"),
            scale * 0.84,
            fg_c,
        );

        if let Some(col) = preview {
            let dots = commands
                .spawn((
                    Node {
                        flex_direction: FlexDirection::Row,
                        column_gap: px((3.0 * scale).round()),
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    ChildOf(row),
                ))
                .id();
            for ch in [CH_BASE, CH_AID, CH_OPTIONAL, CH_KOJIMA] {
                let c = col.groups[0][ch];
                commands.spawn((
                    Node {
                        width: px((14.0 * scale).round()),
                        height: px((11.0 * scale).round()),
                        border: UiRect::all(px(bw)),
                        ..default()
                    },
                    BackgroundColor(Color::srgb_u8(c[0], c[1], c[2])),
                    BorderColor::all(palette::BORDER_SUBTLE),
                    ChildOf(dots),
                ));
            }
        }
    }

    // Pager bar
    if state.color_presets.len() > LIST_WINDOW {
        let pager = commands
            .spawn((
                Node {
                    width: Val::Percent(100.0),
                    flex_direction: FlexDirection::Row,
                    justify_content: JustifyContent::SpaceBetween,
                    align_items: AlignItems::Center,
                    margin: UiRect::top(px((2.0 * scale).round())),
                    ..default()
                },
                ChildOf(panel),
            ))
            .id();
        spawn_kit_arrow_btn(
            commands,
            kit,
            pager,
            ArrowDir::Left,
            Some("PREV"),
            UiAction::ListPage(-1),
            scale,
        );
        kit.spawn_text(
            commands,
            pager,
            FontRole::Small,
            &format!("{}-{} / {}", start + 1, end, state.color_presets.len()),
            scale * 0.88,
            palette::TEXT_MUTED,
        );
        spawn_kit_arrow_btn(
            commands,
            kit,
            pager,
            ArrowDir::Right,
            Some("NEXT"),
            UiAction::ListPage(1),
            scale,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_paint_palette_tab(
    commands: &mut Commands,
    kit: &AcvdUiKit,
    panel: Entity,
    state: &GarageState,
    garage: &crate::Garage,
    custom_camos: &paint::CustomCamoLibrary,
    picker_assets: &PaintPickerAssets,
    scale: f32,
) {
    let bw = (1.0 * scale).max(1.0);

    // 7 Channel selector rows
    let ch_list = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                row_gap: px((2.5 * scale).round()),
                ..default()
            },
            ChildOf(panel),
        ))
        .id();

    for (ch, &name) in PAINT_CHANNEL_NAMES.iter().enumerate() {
        let sel = state.paint_channel == ch;
        let rgba = current_channel_color(garage, ch);
        let (top_c, bot_c, border_c, fg_c, sub_c) = if sel {
            (
                palette::ACCENT_AMBER_FILL_TOP,
                palette::ACCENT_AMBER_FILL_BOT,
                palette::ACCENT_AMBER,
                palette::TEXT_BRIGHT,
                palette::TEXT_BRIGHT,
            )
        } else {
            (
                palette::BG_BUTTON_TOP,
                palette::BG_BUTTON_BOT,
                palette::BORDER_SUBTLE,
                palette::TEXT_PRIMARY,
                palette::TEXT_MUTED,
            )
        };
        let row = commands
            .spawn((
                Button,
                UiAction::PaintChannel(ch),
                Node {
                    width: Val::Percent(100.0),
                    padding: UiRect::axes(px((8.0 * scale).round()), px((3.0 * scale).round())),
                    border: UiRect::all(px(bw)),
                    flex_direction: FlexDirection::Row,
                    justify_content: JustifyContent::SpaceBetween,
                    align_items: AlignItems::Center,
                    ..default()
                },
                BackgroundGradient::from(LinearGradient::to_bottom(vec![
                    ColorStop::auto(top_c),
                    ColorStop::auto(bot_c),
                ])),
                BorderColor::all(border_c),
                ChildOf(ch_list),
            ))
            .id();

        let left = commands
            .spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: px((5.0 * scale).round()),
                    ..default()
                },
                ChildOf(row),
            ))
            .id();
        if sel {
            if let Some(arrow) = &kit.focus_arrow {
                let s = arrow.size() * (0.45 * scale);
                arrow.spawn(
                    commands,
                    left,
                    Node {
                        width: px(s.x.round()),
                        height: px(s.y.round()),
                        flex_shrink: 0.0,
                        ..default()
                    },
                    palette::TEXT_BRIGHT,
                    true,
                    false,
                    true,
                );
            }
        }
        kit.spawn_text(commands, left, FontRole::Small, name, scale * 0.92, fg_c);

        let right = commands
            .spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: px((7.0 * scale).round()),
                    ..default()
                },
                ChildOf(row),
            ))
            .id();
        kit.spawn_text(
            commands,
            right,
            FontRole::Small,
            &format!("{:3},{:3},{:3}", rgba[0], rgba[1], rgba[2]),
            scale * 0.84,
            sub_c,
        );
        commands.spawn((
            Node {
                width: px((22.0 * scale).round()),
                height: px((12.0 * scale).round()),
                border: UiRect::all(px(bw)),
                ..default()
            },
            BackgroundColor(Color::srgb_u8(rgba[0], rgba[1], rgba[2])),
            BorderColor::all(if sel { Color::WHITE } else { palette::BORDER_BUTTON }),
            ChildOf(right),
        ));
    }

    // Click-drag RGB / HSV Color Selector card
    let rgba = current_channel_color(garage, state.paint_channel);
    spawn_color_picker_card(
        commands,
        panel,
        kit,
        state.paint_picker_mode,
        rgba,
        state.paint_hsv,
        picker_assets,
        104.0,
        scale,
    );

    // 12-slot Camouflage pattern selector grid (6 cols × 2 rows: `NONE` + `CA0001..CA0011`)
    let camo_id = garage.custom_color.map(|c| c.camo[0]).unwrap_or(0);
    let has_custom_camo = custom_camos.active_key.is_some();
    let camo_grid = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_wrap: FlexWrap::Wrap,
                row_gap: px((3.5 * scale).round()),
                column_gap: px((4.0 * scale).round()),
                ..default()
            },
            ChildOf(panel),
        ))
        .id();
    for id in 0u8..=11 {
        let matched = !has_custom_camo && camo_id == id;
        let tile = commands
            .spawn((
                Button,
                UiAction::PaintCamoSelect(id),
                Node {
                    width: px((57.0 * scale).round()),
                    height: px((22.0 * scale).round()),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    overflow: Overflow::clip(),
                    border: UiRect::all(px(if matched {
                        (2.0 * scale).round().max(1.0)
                    } else {
                        bw
                    })),
                    ..default()
                },
                BackgroundColor(if matched {
                    palette::ACCENT_AMBER_FILL_TOP
                } else {
                    palette::BG_BAR
                }),
                BorderColor::all(if matched {
                    palette::ACCENT_AMBER
                } else {
                    palette::BORDER_SUBTLE
                }),
                ChildOf(camo_grid),
            ))
            .id();
        if id == 0 {
            kit.spawn_text(
                commands,
                tile,
                FontRole::Small,
                "NONE",
                scale * 0.72,
                if matched {
                    palette::ACCENT_AMBER
                } else {
                    palette::TEXT_MUTED
                },
            );
        } else if let Some(handle) = picker_assets.camo_thumbs[id as usize].clone() {
            commands.spawn((
                ImageNode::new(handle),
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                ChildOf(tile),
            ));
        } else {
            kit.spawn_text(
                commands,
                tile,
                FontRole::Small,
                &format!("{id:02}"),
                scale * 0.74,
                palette::TEXT_PRIMARY,
            );
        }
    }

    // Uploaded custom camo tiles (if any), each with hover-revealed corner `X` delete button.
    if !custom_camos.assets.is_empty() {
        let custom_grid = commands
            .spawn((
                Node {
                    width: Val::Percent(100.0),
                    flex_wrap: FlexWrap::Wrap,
                    row_gap: px((3.5 * scale).round()),
                    column_gap: px((4.0 * scale).round()),
                    ..default()
                },
                ChildOf(panel),
            ))
            .id();
        for (idx, asset) in custom_camos.assets.iter().enumerate() {
            let matched = custom_camos.active_key.as_deref() == Some(asset.key.as_str());
            let tile = commands
                .spawn((
                    Button,
                    UiAction::PaintCustomCamoSelect(idx),
                    Node {
                        width: px((57.0 * scale).round()),
                        height: px((22.0 * scale).round()),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        overflow: Overflow::clip(),
                        border: UiRect::all(px(if matched {
                            (2.0 * scale).round().max(1.0)
                        } else {
                            bw
                        })),
                        ..default()
                    },
                    BackgroundColor(palette::BG_BAR),
                    BorderColor::all(if matched {
                        palette::ACCENT_AMBER
                    } else {
                        palette::BORDER_SUBTLE
                    }),
                    ChildOf(custom_grid),
                ))
                .id();
            commands.spawn((
                ImageNode::new(asset.handle.clone()),
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                ChildOf(tile),
            ));
            let x_sz = (13.0 * scale).round().max(10.0);
            let x_btn = commands
                .spawn((
                    Button,
                    UiAction::PaintCustomCamoDelete(idx),
                    CustomThumbDeleteBtn { cell: tile },
                    Visibility::Hidden,
                    ZIndex(2),
                    Node {
                        position_type: PositionType::Absolute,
                        top: px((1.0 * scale).round()),
                        right: px((1.0 * scale).round()),
                        width: px(x_sz),
                        height: px(x_sz),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        border: UiRect::all(px(bw)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.18, 0.06, 0.06, 0.92)),
                    BorderColor::all(Color::srgba(0.85, 0.28, 0.24, 0.90)),
                    ChildOf(tile),
                ))
                .id();
            kit.spawn_text(
                commands,
                x_btn,
                FontRole::Small,
                "X",
                scale * 0.62,
                Color::srgb(1.0, 0.90, 0.88),
            );
        }
    }

    // Camouflage active readout + prev/next steppers + UPLOAD button
    let camo_label = if let Some(lbl) = custom_camos.active_label() {
        format!("CAMO: {lbl}")
    } else if camo_id == 0 {
        "CAMO: NONE".to_string()
    } else {
        format!("CAMO: CA{camo_id:04}")
    };
    let camo_row = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::Center,
                column_gap: px((4.0 * scale).round()),
                ..default()
            },
            ChildOf(panel),
        ))
        .id();
    spawn_kit_arrow_btn(
        commands,
        kit,
        camo_row,
        ArrowDir::Left,
        None,
        UiAction::PaintCamoDelta(-1),
        scale,
    );
    kit.spawn_text(
        commands,
        camo_row,
        FontRole::Body,
        &camo_label,
        scale * 0.84,
        palette::ACCENT_AMBER,
    );
    spawn_kit_arrow_btn(
        commands,
        kit,
        camo_row,
        ArrowDir::Right,
        None,
        UiAction::PaintCamoDelta(1),
        scale,
    );
    let upload_camo_btn = spawn_kit_pill(
        commands,
        kit,
        camo_row,
        "+ UPLOAD",
        ButtonState::Normal,
        0.0,
        8.0,
        4.0,
        0.78,
        scale,
    );
    commands
        .entity(upload_camo_btn)
        .insert((Button, UiAction::PaintCamoUpload));

    // Custom Camo Scale hover-to-scroll slider (affects uploaded custom camos only, not default CA0001..CA0011)
    let camo_scale_txt = format!("{:.2}x", custom_camos.custom_scale_value());
    let camo_scale_slider = kit.spawn_slider_row(
        commands,
        panel,
        "CUSTOM CAMO SCALE",
        &camo_scale_txt,
        custom_camos.custom_scale_fill(),
        Some(paint::CustomCamoLibrary::custom_scale_default_mark()),
        false,
        132.0,
        scale * 0.86,
    );
    commands
        .entity(camo_scale_slider.row)
        .insert((Button, UiAction::PaintCamoScaleStep(0)));
    commands
        .entity(camo_scale_slider.track)
        .insert(SliderTrack(SliderTrackTarget::PaintCamoScale));
    commands
        .entity(camo_scale_slider.dec_btn)
        .insert((Button, UiAction::PaintCamoScaleStep(-1)));
    commands
        .entity(camo_scale_slider.inc_btn)
        .insert((Button, UiAction::PaintCamoScaleStep(1)));
}

fn spawn_decal_library_grid(
    commands: &mut Commands,
    kit: &AcvdUiKit,
    panel: Entity,
    decals: &DecalState,
    for_emblem: bool,
    scale: f32,
) {
    let bw = (1.0 * scale).max(1.0);

    // Library sub-tabs: CUSTOM | 360 EMBLEMS
    let ltabs = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                column_gap: px((4.0 * scale).round()),
                ..default()
            },
            ChildOf(panel),
        ))
        .id();
    for (tab, lbl) in [
        (
            DecalLibraryTab::Custom,
            format!("CUSTOM ({})", decals.custom_assets.len()),
        ),
        (
            DecalLibraryTab::Emblems,
            format!("360 EMBLEMS ({})", decals.disc_emblems.len()),
        ),
    ] {
        let bstate = if decals.library_tab == tab {
            ButtonState::Active
        } else {
            ButtonState::Normal
        };
        let b = spawn_kit_pill(commands, kit, ltabs, &lbl, bstate, 1.0, 6.0, 4.0, 0.86, scale);
        commands
            .entity(b)
            .insert((Button, UiAction::DecalLibTab(tab)));
    }

    let total = match decals.library_tab {
        DecalLibraryTab::Custom => decals.custom_assets.len(),
        DecalLibraryTab::Emblems => decals.disc_emblems.len(),
    };
    let pages = total.div_ceil(DECAL_GRID_PAGE_SIZE).max(1);
    let page = decals.library_page.min(pages - 1);
    let start = page * DECAL_GRID_PAGE_SIZE;
    let end = (start + DECAL_GRID_PAGE_SIZE).min(total);

    if total == 0 {
        let empty = kit.spawn_panel(
            commands,
            Some(panel),
            PanelStyle::Subtle,
            Node {
                width: Val::Percent(100.0),
                padding: UiRect::all(px((10.0 * scale).round())),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            scale,
        );
        kit.spawn_text(
            commands,
            empty,
            FontRole::Small,
            if for_emblem {
                "No custom images yet. Upload or drop a PNG/JPG!"
            } else {
                "No custom images yet. Click UPLOAD or drop a PNG/JPG!"
            },
            scale * 0.84,
            palette::TEXT_MUTED,
        );
        return;
    }

    let grid = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_wrap: FlexWrap::Wrap,
                row_gap: px((5.0 * scale).round()),
                column_gap: px((5.0 * scale).round()),
                ..default()
            },
            ChildOf(panel),
        ))
        .id();
    for idx in start..end {
        let (key, label, thumb) = match decals.library_tab {
            DecalLibraryTab::Custom => {
                let a = &decals.custom_assets[idx];
                (a.key.as_str(), a.label.as_str(), Some(a.thumb.clone()))
            }
            DecalLibraryTab::Emblems => {
                let e = &decals.disc_emblems[idx];
                let t = decals.peek_asset(&e.key).map(|a| a.thumb.clone());
                (e.key.as_str(), e.name.as_str(), t)
            }
        };
        let selected = if for_emblem {
            decals.ac_emblem_key.as_deref() == Some(key)
        } else {
            decals.active_image_key.as_deref() == Some(key)
        };
        let action = if for_emblem {
            UiAction::EmblemSelect(idx)
        } else {
            UiAction::DecalSelectBrush(idx)
        };
        let cell = commands
            .spawn((
                Button,
                action,
                Node {
                    width: px((87.0 * scale).round()),
                    height: px((82.0 * scale).round()),
                    padding: UiRect::all(px((3.0 * scale).round())),
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::SpaceBetween,
                    border: UiRect::all(px(if selected { (2.0 * scale).round().max(1.0) } else { bw })),
                    ..default()
                },
                BackgroundColor(if selected {
                    palette::ACCENT_AMBER_FILL_TOP
                } else {
                    palette::BG_BAR
                }),
                BorderColor::all(if selected {
                    palette::ACCENT_AMBER
                } else {
                    palette::BORDER_SUBTLE
                }),
                ChildOf(grid),
            ))
            .id();
        if let Some(handle) = thumb {
            commands.spawn((
                ImageNode::new(handle),
                Node {
                    width: px((56.0 * scale).round()),
                    height: px((56.0 * scale).round()),
                    ..default()
                },
                ChildOf(cell),
            ));
        }
        if decals.library_tab == DecalLibraryTab::Custom {
            let x_sz = (16.0 * scale).round().max(12.0);
            let x_btn = commands
                .spawn((
                    Button,
                    UiAction::DecalDeleteCustom(idx),
                    CustomThumbDeleteBtn { cell },
                    Visibility::Hidden,
                    ZIndex(2),
                    Node {
                        position_type: PositionType::Absolute,
                        top: px((2.0 * scale).round()),
                        right: px((2.0 * scale).round()),
                        width: px(x_sz),
                        height: px(x_sz),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        border: UiRect::all(px(bw)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.18, 0.06, 0.06, 0.92)),
                    BorderColor::all(Color::srgba(0.85, 0.28, 0.24, 0.90)),
                    ChildOf(cell),
                ))
                .id();
            kit.spawn_text(
                commands,
                x_btn,
                FontRole::Small,
                "X",
                scale * 0.72,
                Color::srgb(1.0, 0.90, 0.88),
            );
        }
        kit.spawn_text(
            commands,
            cell,
            FontRole::Small,
            &truncate(label, 10),
            scale * 0.78,
            if selected {
                palette::TEXT_BRIGHT
            } else {
                palette::TEXT_MUTED
            },
        );
    }

    if pages > 1 {
        let pg = commands
            .spawn((
                Node {
                    width: Val::Percent(100.0),
                    flex_direction: FlexDirection::Row,
                    justify_content: JustifyContent::SpaceBetween,
                    align_items: AlignItems::Center,
                    ..default()
                },
                ChildOf(panel),
            ))
            .id();
        spawn_kit_arrow_btn(
            commands,
            kit,
            pg,
            ArrowDir::Left,
            Some("PREV"),
            UiAction::DecalLibPage(-1),
            scale,
        );
        kit.spawn_text(
            commands,
            pg,
            FontRole::Small,
            &format!("PAGE {} / {}", page + 1, pages),
            scale * 0.86,
            palette::TEXT_MUTED,
        );
        spawn_kit_arrow_btn(
            commands,
            kit,
            pg,
            ArrowDir::Right,
            Some("NEXT"),
            UiAction::DecalLibPage(1),
            scale,
        );
    }
}

fn spawn_paint_decals_tab(
    commands: &mut Commands,
    kit: &AcvdUiKit,
    panel: Entity,
    decals: &DecalState,
    scale: f32,
) {
    let upload = kit.spawn_button(
        commands,
        panel,
        "+ UPLOAD IMAGE",
        Some("PNG / JPG"),
        ButtonState::Active,
        scale * 0.90,
    );
    commands
        .entity(upload)
        .insert((Button, UiAction::DecalUpload));

    kit.spawn_text(
        commands,
        panel,
        FontRole::Small,
        "Drop PNG/JPG onto window, then click the AC.",
        scale * 0.80,
        palette::TEXT_MUTED,
    );

    let mode_btn = kit.spawn_button(
        commands,
        panel,
        if decals.place_new_mode {
            "MODE: STAMP NEW DECAL"
        } else {
            "MODE: DRAG EXISTING / PLACE"
        },
        Some("TOGGLE"),
        if decals.place_new_mode {
            ButtonState::Selected
        } else {
            ButtonState::Normal
        },
        scale * 0.86,
    );
    commands
        .entity(mode_btn)
        .insert((Button, UiAction::DecalToggleNewMode));

    spawn_decal_library_grid(commands, kit, panel, decals, false, scale);
}

fn spawn_paint_emblem_tab(
    commands: &mut Commands,
    kit: &AcvdUiKit,
    panel: Entity,
    decals: &DecalState,
    scale: f32,
) {
    let active_label = decals
        .active_emblem_asset()
        .map(|a| a.label.clone())
        .or_else(|| {
            decals.ac_emblem_key.as_ref().map(|k| {
                k.strip_prefix("emblem:")
                    .or_else(|| k.strip_prefix("custom:"))
                    .unwrap_or(k)
                    .to_string()
            })
        })
        .unwrap_or_else(|| "NONE".to_string());
    let default_label = match &decals.default_emblem_key {
        Some(k) => k.strip_prefix("emblem:").unwrap_or(k).to_string(),
        None => "NONE".to_string(),
    };
    let short_active = truncate(&active_label, 18);
    let status = if decals.emblem_override {
        format!("ACTIVE: {short_active}")
    } else {
        format!("ACTIVE: {short_active} (DEF)")
    };

    let card = kit.spawn_panel(
        commands,
        Some(panel),
        PanelStyle::Subtle,
        Node {
            width: Val::Percent(100.0),
            padding: UiRect::axes(px((8.0 * scale).round()), px((6.0 * scale).round())),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: px((10.0 * scale).round()),
            ..default()
        },
        scale,
    );
    commands.spawn((
        ImageNode::new(decals.emblem_texture.clone()),
        Node {
            width: px((56.0 * scale).round()),
            height: px((56.0 * scale).round()),
            flex_shrink: 0.0,
            ..default()
        },
        ChildOf(card),
    ));
    let info = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Column,
                row_gap: px((3.0 * scale).round()),
                flex_grow: 1.0,
                ..default()
            },
            ChildOf(card),
        ))
        .id();
    kit.spawn_text(commands, info, FontRole::Body, &status, scale * 0.86, palette::ACCENT_AMBER);
    kit.spawn_text(
        commands,
        info,
        FontRole::Small,
        &format!("DEFAULT: {default_label}"),
        scale * 0.82,
        palette::TEXT_MUTED,
    );

    let upload = kit.spawn_button(
        commands,
        panel,
        "+ UPLOAD CUSTOM EMBLEM",
        Some("PNG / JPG"),
        ButtonState::Active,
        scale * 0.90,
    );
    commands
        .entity(upload)
        .insert((Button, UiAction::DecalUpload));

    let act_row = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                column_gap: px((5.0 * scale).round()),
                ..default()
            },
            ChildOf(panel),
        ))
        .id();
    let rst = spawn_kit_pill(
        commands,
        kit,
        act_row,
        "RESET TO DESIGN",
        ButtonState::Normal,
        1.0,
        6.0,
        4.5,
        0.84,
        scale,
    );
    commands
        .entity(rst)
        .insert((Button, UiAction::EmblemResetDefault));
    let clr = spawn_kit_pill(
        commands,
        kit,
        act_row,
        "CLEAR EMBLEM",
        ButtonState::Normal,
        1.0,
        6.0,
        4.5,
        0.84,
        scale,
    );
    commands
        .entity(clr)
        .insert((Button, UiAction::EmblemClear));

    spawn_decal_library_grid(commands, kit, panel, decals, true, scale);
}

/// Reusable click-drag + hover-scroll RGB / HSV color picker card (used by both `PAINT -> PALETTE`
/// and the `PAINT -> DECALS` tint color selector).
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_color_picker_card(
    commands: &mut Commands,
    parent: Entity,
    kit: &AcvdUiKit,
    picker_mode: PaintPickerMode,
    rgba: [u8; 4],
    hsv: [f32; 3],
    picker_assets: &PaintPickerAssets,
    sv_height: f32,
    scale: f32,
) {
    let bw = (1.0 * scale).max(1.0);
    let [h_deg, sat, val] = hsv;
    let picker_card = kit.spawn_panel(
        commands,
        Some(parent),
        PanelStyle::Subtle,
        Node {
            width: Val::Percent(100.0),
            padding: UiRect::all(px((7.0 * scale).round())),
            flex_direction: FlexDirection::Column,
            row_gap: px((5.0 * scale).round()),
            ..default()
        },
        scale,
    );

    // Picker header: [HSV] [RGB] toggle pills + live swatch + #HEX
    let picker_hdr = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::Center,
                column_gap: px((6.0 * scale).round()),
                ..default()
            },
            ChildOf(picker_card),
        ))
        .id();

    let mode_pills = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                column_gap: px((4.0 * scale).round()),
                ..default()
            },
            ChildOf(picker_hdr),
        ))
        .id();
    for (pmode, lbl) in [(PaintPickerMode::Hsv, "HSV"), (PaintPickerMode::Rgb, "RGB")] {
        let pstate = if picker_mode == pmode {
            ButtonState::Active
        } else {
            ButtonState::Normal
        };
        let b = spawn_kit_pill(commands, kit, mode_pills, lbl, pstate, 0.0, 10.0, 3.0, 0.88, scale);
        commands
            .entity(b)
            .insert((Button, UiAction::PaintPickerMode(pmode)));
    }

    let hex_box = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: px((6.0 * scale).round()),
                ..default()
            },
            ChildOf(picker_hdr),
        ))
        .id();
    kit.spawn_text(
        commands,
        hex_box,
        FontRole::Small,
        &format!("#{:02X}{:02X}{:02X}", rgba[0], rgba[1], rgba[2]),
        scale * 0.90,
        palette::TEXT_BRIGHT,
    );
    commands.spawn((
        Node {
            width: px((28.0 * scale).round()),
            height: px((15.0 * scale).round()),
            border: UiRect::all(px(bw)),
            ..default()
        },
        BackgroundColor(Color::srgb_u8(rgba[0], rgba[1], rgba[2])),
        BorderColor::all(Color::WHITE),
        ChildOf(hex_box),
    ));

    match picker_mode {
        PaintPickerMode::Hsv => {
            // 2D Saturation × Value drag square
            let sv_box = commands
                .spawn((
                    ColorPickerTrack(PaintDragTarget::HsvSv),
                    Node {
                        width: Val::Percent(100.0),
                        height: px((sv_height * scale).round()),
                        border: UiRect::all(px(bw)),
                        position_type: PositionType::Relative,
                        overflow: Overflow::clip(),
                        ..default()
                    },
                    BorderColor::all(palette::BORDER_BUTTON),
                    ChildOf(picker_card),
                ))
                .id();
            if let Some(sv_img) = &picker_assets.sv_image {
                commands.spawn((
                    ImageNode::new(sv_img.clone()),
                    Node {
                        position_type: PositionType::Absolute,
                        left: px(0.0),
                        right: px(0.0),
                        top: px(0.0),
                        bottom: px(0.0),
                        width: Val::Percent(100.0),
                        height: Val::Percent(100.0),
                        ..default()
                    },
                    ChildOf(sv_box),
                ));
            }
            // High-contrast crosshair reticle at (S, 1 - V)
            let ring_sz = (12.0 * scale).round().max(8.0);
            let ring_half = (ring_sz * 0.5).round();
            let ring = commands
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Percent((sat * 100.0).clamp(0.0, 100.0)),
                        top: Val::Percent(((1.0 - val) * 100.0).clamp(0.0, 100.0)),
                        width: px(ring_sz),
                        height: px(ring_sz),
                        margin: UiRect {
                            left: px(-ring_half),
                            top: px(-ring_half),
                            ..default()
                        },
                        border: UiRect::all(px((2.0 * scale).round().max(1.0))),
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::Center,
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.25)),
                    BorderColor::all(Color::WHITE),
                    ChildOf(sv_box),
                ))
                .id();
            commands.spawn((
                Node {
                    width: px((4.0 * scale).round().max(2.0)),
                    height: px((4.0 * scale).round().max(2.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.85)),
                ChildOf(ring),
            ));

            // Horizontal Hue spectrum drag + hover-scroll bar
            let hue_box = commands
                .spawn((
                    ColorPickerTrack(PaintDragTarget::HsvHue),
                    Node {
                        width: Val::Percent(100.0),
                        height: px((17.0 * scale).round()),
                        border: UiRect::all(px(bw)),
                        position_type: PositionType::Relative,
                        ..default()
                    },
                    BorderColor::all(palette::BORDER_BUTTON),
                    ChildOf(picker_card),
                ))
                .id();
            if let Some(hue_img) = &picker_assets.hue_image {
                commands.spawn((
                    ImageNode::new(hue_img.clone()),
                    Node {
                        position_type: PositionType::Absolute,
                        left: px(0.0),
                        right: px(0.0),
                        top: px(0.0),
                        bottom: px(0.0),
                        width: Val::Percent(100.0),
                        height: Val::Percent(100.0),
                        ..default()
                    },
                    ChildOf(hue_box),
                ));
            }
            let thumb_w = (6.0 * scale).round().max(4.0);
            commands.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Percent(((h_deg / 360.0) * 100.0).clamp(0.0, 100.0)),
                    top: px(-2.0 * scale),
                    bottom: px(-2.0 * scale),
                    width: px(thumb_w),
                    margin: UiRect::left(px(-(thumb_w * 0.5).round())),
                    border: UiRect::all(px(bw)),
                    ..default()
                },
                BackgroundColor(Color::WHITE),
                BorderColor::all(Color::BLACK),
                ChildOf(hue_box),
            ));

            // HSV + RGB readout strip
            let readout = commands
                .spawn((
                    Node {
                        width: Val::Percent(100.0),
                        flex_direction: FlexDirection::Row,
                        justify_content: JustifyContent::SpaceBetween,
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    ChildOf(picker_card),
                ))
                .id();
            kit.spawn_text(
                commands,
                readout,
                FontRole::Small,
                &format!(
                    "H {:3}  S {:3}%  V {:3}%",
                    h_deg.round() as i32,
                    (sat * 100.0).round() as i32,
                    (val * 100.0).round() as i32,
                ),
                scale * 0.84,
                palette::TEXT_PRIMARY,
            );
            kit.spawn_text(
                commands,
                readout,
                FontRole::Small,
                "DRAG TO PICK",
                scale * 0.80,
                palette::TEXT_MUTED,
            );
        }
        PaintPickerMode::Rgb => {
            // 3 click-and-drag + hover-scroll RGB gradient slider bars + fine steppers
            let rgb_specs = [
                (
                    0usize,
                    "R",
                    PaintDragTarget::RgbR,
                    Color::srgb(1.0, 0.45, 0.42),
                    Color::srgb_u8(0, rgba[1], rgba[2]),
                    Color::srgb_u8(255, rgba[1], rgba[2]),
                ),
                (
                    1usize,
                    "G",
                    PaintDragTarget::RgbG,
                    Color::srgb(0.45, 0.96, 0.55),
                    Color::srgb_u8(rgba[0], 0, rgba[2]),
                    Color::srgb_u8(rgba[0], 255, rgba[2]),
                ),
                (
                    2usize,
                    "B",
                    PaintDragTarget::RgbB,
                    Color::srgb(0.45, 0.74, 1.0),
                    Color::srgb_u8(rgba[0], rgba[1], 0),
                    Color::srgb_u8(rgba[0], rgba[1], 255),
                ),
            ];
            for (comp, lbl, target, lbl_col, min_c, max_c) in rgb_specs {
                let row = commands
                    .spawn((
                        Node {
                            width: Val::Percent(100.0),
                            flex_direction: FlexDirection::Row,
                            align_items: AlignItems::Center,
                            column_gap: px((5.0 * scale).round()),
                            ..default()
                        },
                        ChildOf(picker_card),
                    ))
                    .id();

                let lbl_box = commands
                    .spawn((
                        Node {
                            width: px((14.0 * scale).round()),
                            flex_shrink: 0.0,
                            justify_content: JustifyContent::Center,
                            ..default()
                        },
                        ChildOf(row),
                    ))
                    .id();
                kit.spawn_text(commands, lbl_box, FontRole::Body, lbl, scale * 0.86, lbl_col);

                spawn_kit_arrow_btn(
                    commands,
                    kit,
                    row,
                    ArrowDir::Left,
                    None,
                    UiAction::PaintRgbDelta(comp, -8),
                    scale * 0.86,
                );

                // Click-drag gradient bar
                let track = commands
                    .spawn((
                        ColorPickerTrack(target),
                        Node {
                            flex_grow: 1.0,
                            height: px((18.0 * scale).round()),
                            border: UiRect::all(px(bw)),
                            position_type: PositionType::Relative,
                            ..default()
                        },
                        BackgroundGradient::from(LinearGradient::to_right(vec![
                            ColorStop::auto(min_c),
                            ColorStop::auto(max_c),
                        ])),
                        BorderColor::all(palette::BORDER_BUTTON),
                        ChildOf(row),
                    ))
                    .id();

                let pct = (rgba[comp] as f32 / 255.0) * 100.0;
                let thumb_w = (6.0 * scale).round().max(4.0);
                commands.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Percent(pct),
                        top: px(-2.0 * scale),
                        bottom: px(-2.0 * scale),
                        width: px(thumb_w),
                        margin: UiRect::left(px(-(thumb_w * 0.5).round())),
                        border: UiRect::all(px(bw)),
                        ..default()
                    },
                    BackgroundColor(Color::WHITE),
                    BorderColor::all(Color::BLACK),
                    ChildOf(track),
                ));

                spawn_kit_arrow_btn(
                    commands,
                    kit,
                    row,
                    ArrowDir::Right,
                    None,
                    UiAction::PaintRgbDelta(comp, 8),
                    scale * 0.86,
                );

                let val_box = commands
                    .spawn((
                        Node {
                            width: px((28.0 * scale).round()),
                            flex_shrink: 0.0,
                            justify_content: JustifyContent::FlexEnd,
                            ..default()
                        },
                        ChildOf(row),
                    ))
                    .id();
                kit.spawn_text(
                    commands,
                    val_box,
                    FontRole::Small,
                    &format!("{:3}", rgba[comp]),
                    scale * 0.88,
                    palette::TEXT_BRIGHT,
                );
            }

            let readout = commands
                .spawn((
                    Node {
                        width: Val::Percent(100.0),
                        flex_direction: FlexDirection::Row,
                        justify_content: JustifyContent::SpaceBetween,
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    ChildOf(picker_card),
                ))
                .id();
            kit.spawn_text(
                commands,
                readout,
                FontRole::Small,
                &format!(
                    "H {:3}  S {:3}%  V {:3}%",
                    h_deg.round() as i32,
                    (sat * 100.0).round() as i32,
                    (val * 100.0).round() as i32,
                ),
                scale * 0.82,
                palette::TEXT_MUTED,
            );
            kit.spawn_text(
                commands,
                readout,
                FontRole::Small,
                "DRAG BARS",
                scale * 0.80,
                palette::TEXT_MUTED,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_garage_decal_inspector(
    commands: &mut Commands,
    root: Entity,
    kit: &AcvdUiKit,
    state: &GarageState,
    decals: &DecalState,
    picker_assets: &PaintPickerAssets,
    scale: f32,
) {
    let bw = (1.0 * scale).max(1.0);
    let insp = kit.spawn_panel(
        commands,
        Some(root),
        PanelStyle::Tactical,
        Node {
            position_type: PositionType::Absolute,
            right: px((20.0 * scale).round()),
            top: px((64.0 * scale).round()),
            width: px((304.0 * scale).round()),
            padding: UiRect::all(px((11.0 * scale).round())),
            flex_direction: FlexDirection::Column,
            row_gap: px((5.0 * scale).round()),
            ..default()
        },
        scale,
    );

    let tag = format!("{} PLACED", decals.layers.len());
    kit.spawn_header(commands, insp, "INSPECTOR", Some(&tag), scale);

    let sel_layer = decals.selected_layer().cloned();
    let (size_val, rot_deg, opacity_pct, mirror_on, flip_on, slot_txt) =
        if let Some(ref d) = sel_layer {
            (
                d.scale,
                d.rotation.to_degrees().round() as i32,
                (d.opacity * 100.0).round() as i32,
                d.mirror_x,
                d.flip_h,
                format!("PART: {}  (DRAG AC)", d.slot.label()),
            )
        } else {
            (
                decals.brush_scale,
                0,
                100,
                decals.brush_mirror_x,
                decals.brush_flip_h,
                "BRUSH DEFAULTS  (CLICK AC)".to_string(),
            )
        };

    let status_bar = kit.spawn_panel(
        commands,
        Some(insp),
        PanelStyle::Subtle,
        Node {
            width: Val::Percent(100.0),
            padding: UiRect::axes(px((7.0 * scale).round()), px((4.0 * scale).round())),
            ..default()
        },
        scale,
    );
    kit.spawn_text(
        commands,
        status_bar,
        FontRole::Small,
        &slot_txt,
        scale * 0.82,
        palette::ACCENT_AMBER,
    );

    // Size / Rotate / Opacity UI-kit slider rows
    let size_slider = kit.spawn_slider_row(
        commands,
        insp,
        "SIZE",
        &format!("{size_val:.2}m"),
        ((size_val - 0.15) / 2.85).clamp(0.0, 1.0),
        None,
        false,
        66.0,
        scale * 0.90,
    );
    commands
        .entity(size_slider.row)
        .insert((Button, UiAction::DecalScaleStep(0)));
    commands
        .entity(size_slider.track)
        .insert(SliderTrack(SliderTrackTarget::DecalScale));
    commands
        .entity(size_slider.dec_btn)
        .insert((Button, UiAction::DecalScaleStep(-1)));
    commands
        .entity(size_slider.inc_btn)
        .insert((Button, UiAction::DecalScaleStep(1)));

    let rot_slider = kit.spawn_slider_row(
        commands,
        insp,
        "ROTATE",
        &format!("{rot_deg} deg"),
        ((rot_deg.rem_euclid(360)) as f32 / 360.0).clamp(0.0, 1.0),
        None,
        false,
        66.0,
        scale * 0.90,
    );
    commands
        .entity(rot_slider.row)
        .insert((Button, UiAction::DecalRotateStep(0)));
    commands
        .entity(rot_slider.track)
        .insert(SliderTrack(SliderTrackTarget::DecalRotate));
    commands
        .entity(rot_slider.dec_btn)
        .insert((Button, UiAction::DecalRotateStep(-1)));
    commands
        .entity(rot_slider.inc_btn)
        .insert((Button, UiAction::DecalRotateStep(1)));

    let op_slider = kit.spawn_slider_row(
        commands,
        insp,
        "OPACITY",
        &format!("{opacity_pct}%"),
        (opacity_pct as f32 / 100.0).clamp(0.0, 1.0),
        None,
        false,
        66.0,
        scale * 0.90,
    );
    commands
        .entity(op_slider.row)
        .insert((Button, UiAction::DecalOpacityStep(0)));
    commands
        .entity(op_slider.track)
        .insert(SliderTrack(SliderTrackTarget::DecalOpacity));
    commands
        .entity(op_slider.dec_btn)
        .insert((Button, UiAction::DecalOpacityStep(-1)));
    commands
        .entity(op_slider.inc_btn)
        .insert((Button, UiAction::DecalOpacityStep(1)));

    // Mirror X + Flip H toggles
    let toggle_row = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                column_gap: px((5.0 * scale).round()),
                ..default()
            },
            ChildOf(insp),
        ))
        .id();
    for (act, lbl, on) in [
        (UiAction::DecalToggleMirror, "MIRROR X (M)", mirror_on),
        (UiAction::DecalToggleFlip, "FLIP H (F)", flip_on),
    ] {
        let b = spawn_kit_pill(
            commands,
            kit,
            toggle_row,
            lbl,
            if on { ButtonState::Active } else { ButtonState::Normal },
            1.0,
            6.0,
            4.5,
            0.84,
            scale,
        );
        commands.entity(b).insert((Button, act));
    }

    // Tint color RGB / HSV selector (reused from Paint Palette)
    kit.spawn_text(commands, insp, FontRole::Small, "TINT COLOR", scale * 0.84, palette::TEXT_MUTED);
    spawn_color_picker_card(
        commands,
        insp,
        kit,
        state.paint_picker_mode,
        current_decal_tint(decals),
        state.paint_hsv,
        picker_assets,
        72.0,
        scale * 0.88,
    );

    // Duplicate / Clear All
    let act_row = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                column_gap: px((5.0 * scale).round()),
                ..default()
            },
            ChildOf(insp),
        ))
        .id();
    let dup = spawn_kit_pill(
        commands,
        kit,
        act_row,
        "DUPLICATE",
        ButtonState::Normal,
        1.0,
        6.0,
        4.0,
        0.84,
        scale,
    );
    commands.entity(dup).insert((Button, UiAction::DecalDuplicate));
    let clr = commands
        .spawn((
            Button,
            UiAction::DecalClearAll,
            Node {
                flex_grow: 1.0,
                padding: UiRect::axes(px((6.0 * scale).round()), px((4.0 * scale).round())),
                border: UiRect::all(px(bw)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.42, 0.12, 0.10, 0.85)),
            BorderColor::all(palette::TEXT_ALERT),
            ChildOf(act_row),
        ))
        .id();
    kit.spawn_text(commands, clr, FontRole::Small, "CLEAR ALL", scale * 0.84, palette::TEXT_BRIGHT);

    kit.spawn_trace(commands, insp, TraceStyle::Simple, scale);

    // Placed layers list
    kit.spawn_text(
        commands,
        insp,
        FontRole::Small,
        "PLACED LAYERS  (TOP -> BOTTOM)",
        scale * 0.84,
        palette::TEXT_MUTED,
    );
    if decals.layers.is_empty() {
        kit.spawn_text(
            commands,
            insp,
            FontRole::Small,
            "Click anywhere on the AC to stamp your image.",
            scale * 0.80,
            palette::TEXT_MUTED,
        );
    } else {
        for layer in decals.layers.iter().rev().take(8) {
            let is_sel = decals.selected_id == Some(layer.id);
            let name = decals
                .peek_asset(&layer.image_key)
                .map(|a| a.label.as_str())
                .unwrap_or(&layer.image_key);
            let row = commands
                .spawn((
                    Node {
                        width: Val::Percent(100.0),
                        flex_direction: FlexDirection::Row,
                        column_gap: px((3.0 * scale).round()),
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    ChildOf(insp),
                ))
                .id();
            let sel_btn = spawn_kit_pill(
                commands,
                kit,
                row,
                &format!("[{}] {}", layer.slot.label(), truncate(name, 11)),
                if is_sel { ButtonState::Selected } else { ButtonState::Normal },
                1.0,
                6.0,
                3.5,
                0.80,
                scale,
            );
            commands
                .entity(sel_btn)
                .insert((Button, UiAction::DecalSelectLayer(layer.id)));

            let up_btn = spawn_kit_pill(
                commands,
                kit,
                row,
                "^",
                ButtonState::Normal,
                0.0,
                5.0,
                3.5,
                0.80,
                scale,
            );
            commands
                .entity(up_btn)
                .insert((Button, UiAction::DecalLayerMove(layer.id, 1)));

            let del_btn = commands
                .spawn((
                    Button,
                    UiAction::DecalLayerDelete(layer.id),
                    Node {
                        padding: UiRect::axes(px((6.0 * scale).round()), px((3.5 * scale).round())),
                        border: UiRect::all(px(bw)),
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::Center,
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.45, 0.14, 0.12, 0.85)),
                    BorderColor::all(palette::TEXT_ALERT),
                    ChildOf(row),
                ))
                .id();
            kit.spawn_text(commands, del_btn, FontRole::Small, "X", scale * 0.80, palette::TEXT_BRIGHT);
        }
    }
}

fn spawn_garage_specs_panel(
    commands: &mut Commands,
    root: Entity,
    kit: &AcvdUiKit,
    cur_specs: AcSpecs,
    base_specs: AcSpecs,
    scale: f32,
) {
    let pad = (12.0 * scale).round();
    let stat_scale = scale * 0.86;
    let spec = kit.spawn_panel(
        commands,
        Some(root),
        PanelStyle::Tactical,
        Node {
            position_type: PositionType::Absolute,
            right: px((20.0 * scale).round()),
            top: px((64.0 * scale).round()),
            width: px((304.0 * scale).round()),
            padding: UiRect::all(px(pad)),
            flex_direction: FlexDirection::Column,
            row_gap: px((6.0 * scale).round()),
            ..default()
        },
        scale,
    );

    kit.spawn_header(commands, spec, "AC SPECS", Some("STATUS"), scale * 0.94);

    // Alert banners when AC violates build / weight / EN constraints.
    for (active, label) in [
        (!cur_specs.buildable, "! UNBUILDABLE !"),
        (cur_specs.overweight, "! OVERWEIGHT !"),
        (cur_specs.en_shortage, "! EN SHORTAGE !"),
    ] {
        if active {
            let alert = commands
                .spawn((
                    Node {
                        width: Val::Percent(100.0),
                        padding: UiRect::axes(px((8.0 * scale).round()), px((3.0 * scale).round())),
                        border: UiRect::all(px((1.0 * scale).max(1.0))),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.45, 0.08, 0.06, 0.88)),
                    BorderColor::all(palette::TEXT_ALERT),
                    ChildOf(spec),
                ))
                .id();
            kit.spawn_text(
                commands,
                alert,
                FontRole::Small,
                label,
                scale,
                palette::TEXT_BRIGHT,
            );
        }
    }

    // Retail `@WeightBar` & `@EnergyBar` capacity gauges.
    let gauges = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                row_gap: px((4.0 * scale).round()),
                ..default()
            },
            ChildOf(spec),
        ))
        .id();
    let load_ratio = if cur_specs.leg_payload > 0.0 {
        cur_specs.carried_load / cur_specs.leg_payload
    } else {
        1.0
    };
    let en_ratio = if cur_specs.en_output > 0.0 {
        cur_specs.en_drain / cur_specs.en_output
    } else {
        1.0
    };
    kit.spawn_gauge(
        commands,
        gauges,
        "LOAD",
        load_ratio,
        Some(0.85),
        cur_specs.overweight,
        stat_scale,
    );
    kit.spawn_gauge(
        commands,
        gauges,
        "ENERGY",
        en_ratio,
        Some(0.80),
        cur_specs.en_shortage,
        stat_scale,
    );

    kit.spawn_trace(commands, spec, TraceStyle::Simple, scale);

    // Alternating `@ParamLine` telemetry rows with live preview deltas (`PREV > NEW`).
    let rows_box = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                row_gap: px((1.0 * scale).max(1.0)),
                ..default()
            },
            ChildOf(spec),
        ))
        .id();

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

    for (idx, &(label, val, prev, higher_is_better, unit)) in rows.iter().enumerate() {
        let diff = val - prev;
        let is_secs = unit == " s";
        let has_diff = if is_secs {
            diff.abs() >= 0.01
        } else {
            diff.abs() >= 0.5
        };

        if has_diff {
            let prev_str = if is_secs {
                format!("{prev:.2}")
            } else {
                format!("{prev:.0}")
            };
            let new_str = if is_secs {
                format!("{val:.2}{unit}")
            } else {
                format!("{val:.0}{unit}")
            };
            let cmp = if (diff > 0.0) == higher_is_better { 1 } else { -1 };
            kit.spawn_stat_row(
                commands,
                rows_box,
                label,
                &prev_str,
                Some(StatDelta {
                    text: &new_str,
                    cmp,
                }),
                idx % 2 == 0,
                stat_scale,
            );
        } else {
            let val_str = if is_secs {
                format!("{val:.2}{unit}")
            } else {
                format!("{val:.0}{unit}")
            };
            kit.spawn_stat_row(
                commands,
                rows_box,
                label,
                &val_str,
                None,
                idx % 2 == 0,
                stat_scale,
            );
        }
    }
}

fn spawn_garage_footer(
    commands: &mut Commands,
    root: Entity,
    kit: &AcvdUiKit,
    footer_text: &str,
    scale: f32,
) {
    let foot = kit.spawn_panel(
        commands,
        Some(root),
        PanelStyle::Framed,
        Node {
            position_type: PositionType::Absolute,
            left: px((20.0 * scale).round()),
            right: px((20.0 * scale).round()),
            bottom: px((12.0 * scale).round()),
            min_height: px((30.0 * scale).round()),
            padding: UiRect::axes(px((12.0 * scale).round()), px((4.0 * scale).round())),
            flex_direction: FlexDirection::Row,
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Center,
            column_gap: px((12.0 * scale).round()),
            ..default()
        },
        scale,
    );

    kit.spawn_text(
        commands,
        foot,
        FontRole::Small,
        footer_text,
        scale * 0.90,
        palette::TEXT_PRIMARY,
    );

    let view_btn = kit.spawn_badge(
        commands,
        foot,
        "VIEW MODE [V]",
        palette::ACCENT_TEAL,
        true,
        scale,
    );
    commands
        .entity(view_btn)
        .insert((Button, UiAction::ToggleViewMode));
}

fn spawn_garage_view_mode_bar(
    commands: &mut Commands,
    root: Entity,
    kit: &AcvdUiKit,
    scale: f32,
) {
    let bar = kit.spawn_panel(
        commands,
        Some(root),
        PanelStyle::Framed,
        Node {
            position_type: PositionType::Absolute,
            left: px((24.0 * scale).round()),
            bottom: px((18.0 * scale).round()),
            padding: UiRect::axes(px((12.0 * scale).round()), px((7.0 * scale).round())),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: px((12.0 * scale).round()),
            ..default()
        },
        scale,
    );
    kit.spawn_badge(commands, bar, "VIEW MODE", palette::ACCENT_TEAL, true, scale);
    kit.spawn_text(
        commands,
        bar,
        FontRole::Small,
        "DRAG ORBIT   WHEEL ZOOM   R RESET   V / ESC RETURN",
        scale,
        palette::TEXT_PRIMARY,
    );
}

fn spawn_list_pager(
    panel: &mut ChildSpawnerCommands,
    start: usize,
    end: usize,
    total: usize,
    bg: Color,
    fg_cyan: Color,
    fg_dim: Color,
) {
    if total <= LIST_WINDOW {
        return;
    }
    let pages = total.div_ceil(LIST_WINDOW);
    let page = (start / LIST_WINDOW) + 1;
    let label = if start % LIST_WINDOW == 0 {
        format!("PAGE {page} / {pages}")
    } else {
        format!("{}-{} / {}", start + 1, end, total)
    };
    panel
        .spawn(Node {
            width: Val::Percent(100.0),
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Center,
            margin: UiRect::top(px(4.0)),
            ..default()
        })
        .with_children(|pg| {
            pg.spawn((
                Button,
                UiAction::ListPage(-1),
                Node {
                    padding: UiRect::axes(px(10.0), px(4.0)),
                    ..default()
                },
                BackgroundColor(bg),
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
            pg.spawn((
                Text::new(label),
                TextFont {
                    font_size: FontSize::Px(11.0),
                    ..default()
                },
                TextColor(fg_dim),
            ));
            pg.spawn((
                Button,
                UiAction::ListPage(1),
                Node {
                    padding: UiRect::axes(px(10.0), px(4.0)),
                    ..default()
                },
                BackgroundColor(bg),
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

    #[test]
    fn garage_part_swap_reuses_cached_textures_and_flvers() {
        use std::time::Instant;
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let Ok(disc) = Disc::open(&acvd_formats::vfs::default_disc(&root)) else {
            return;
        };
        let (_, rows) = AC_ASSEMBLY_DESIGN_ST_FILES[0];
        let base_row = rows
            .iter()
            .find(|r| r.id == 5001)
            .expect("design 5001");
        let mut design = base_row.data;
        let accolor = paint::load_accolor(&disc, design.coloring_id);
        let mut packs = Packs::default();
        let mut flvers = acvd_render::FlverCache::default();
        let mut images = Assets::<Image>::default();
        let orig_head = AssembleSlot::Head.get(&design);
        let alt_head = parts_for_slot(AssembleSlot::Head)
            .into_iter()
            .find(|&id| id != orig_head)
            .unwrap();
        let alt_wep = parts_for_slot(AssembleSlot::ArmUnitR)
            .into_iter()
            .find(|&id| id != 0 && id != AssembleSlot::ArmUnitR.get(&design))
            .unwrap();

        for pass in ["cold_initial", "warm_head_swap", "warm_weapon_swap", "warm_return_head"] {
            if pass == "warm_head_swap" {
                AssembleSlot::Head.set(&mut design, alt_head);
            } else if pass == "warm_weapon_swap" {
                AssembleSlot::ArmUnitR.set(&mut design, alt_wep);
            } else if pass == "warm_return_head" {
                AssembleSlot::Head.set(&mut design, orig_head);
            }
            let t_total = Instant::now();
            let built = crate::assemble::assemble(
                acvd_data::generated::assembly::AC_ASSEMBLY_DESIGN_ST_SLOTS,
                &design,
            );
            let t_flver = Instant::now();
            let mut loaded = Vec::new();
            let mut known = vec![None; built.placements.len()];
            for (index, p) in built.placements.iter().enumerate() {
                let places = crate::assemble::root_places(p, &built.placements, &known);
                let (meshes, rig) = flvers
                    .rigged_model(&disc, p.model.path, &|root| {
                        crate::assemble::place_of(&places, root)
                    })
                    .expect("rigged_model");
                known[index] = Some(crate::assemble::Stored {
                    places,
                    frames: rig.frames.clone(),
                });
                loaded.push(crate::pose::Loaded {
                    index,
                    placement: p,
                    meshes,
                    rig,
                });
            }
            let ms_flver = t_flver.elapsed().as_secs_f64() * 1000.0;

            let t_pose = Instant::now();
            let _rig = crate::pose::build(&disc, &loaded).expect("pose::build");
            let ms_pose = t_pose.elapsed().as_secs_f64() * 1000.0;

            let t_mats = Instant::now();
            let mut missing = 0;
            let mut mesh_count = 0;
            let img_before = images.len();
            for part in &loaded {
                for mesh in &part.meshes {
                    mesh_count += 1;
                    let base = acvd_render::material(&disc, mesh, &mut packs, &mut images, false, &mut missing);
                    if let Some(ref col) = accolor {
                        let _ = paint::material(&disc, &base, mesh, part.placement.column, col, None, &mut packs, &mut images);
                    }
                }
            }
            let new_imgs = images.len() - img_before;
            let ms_mats = t_mats.elapsed().as_secs_f64() * 1000.0;

            let t_preview = Instant::now();
            let head_part = ac_part(0, AssembleSlot::Head.get(&design)).unwrap();
            let preview_meshes = flvers.model(&disc, head_part.models[0].1).unwrap();
            for lm in &preview_meshes {
                let base = acvd_render::material(&disc, lm, &mut packs, &mut images, false, &mut missing);
                if let Some(ref col) = accolor {
                    let _ = paint::material(&disc, &base, lm, "head", col, None, &mut packs, &mut images);
                }
            }
            let ms_preview = t_preview.elapsed().as_secs_f64() * 1000.0;
            let ms_total = t_total.elapsed().as_secs_f64() * 1000.0;

            eprintln!(
                "PROFILE [{pass}] total={ms_total:.1}ms | flver({} parts)={ms_flver:.1}ms | pose={ms_pose:.1}ms | mats({mesh_count} meshes, +{new_imgs} imgs)={ms_mats:.1}ms | preview={ms_preview:.1}ms",
                loaded.len(),
            );
            if pass == "warm_return_head" {
                assert_eq!(new_imgs, 0, "returning to an already-seen head must reuse all GPU texture handles");
            }
        }
    }

    #[test]
    fn test_step_list_page() {
        // 118 preset designs, 13 rows per page (10 pages: 0, 13, ..., 104, 117).
        assert_eq!(step_list_page(0, 118, 13, 1), 13);
        assert_eq!(step_list_page(104, 118, 13, 1), 117);
        assert_eq!(step_list_page(117, 118, 13, 1), 0);
        assert_eq!(step_list_page(0, 118, 13, -1), 117);
        assert_eq!(step_list_page(117, 118, 13, -1), 104);
        assert_eq!(step_list_page(13, 118, 13, -1), 0);
        // Unaligned scroll (after arrow-key stepping) steps cleanly.
        assert_eq!(step_list_page(5, 118, 13, -1), 0);
        assert_eq!(step_list_page(105, 118, 13, 1), 0);
        // Exact multiple (26 items, 2 pages: 0 and 13).
        assert_eq!(step_list_page(0, 26, 13, -1), 13);
        assert_eq!(step_list_page(13, 26, 13, 1), 0);
        // Fits on one page -> stays at 0.
        assert_eq!(step_list_page(0, 10, 13, 1), 0);
        assert_eq!(step_list_page(0, 10, 13, -1), 0);
    }

    #[test]
    fn test_right_stick_orbit() {
        let (mut yaw, mut pitch) = (0.0, 0.2);
        // Inside deadzone: no change.
        assert!(!apply_right_stick_orbit(Vec2::new(0.10, -0.05), 0.1, &mut yaw, &mut pitch));
        assert_eq!((yaw, pitch), (0.0, 0.2));

        // Full right stick right -> yaw decreases (matches mouse drag right / control.rs).
        assert!(apply_right_stick_orbit(Vec2::new(1.0, 0.0), 0.5, &mut yaw, &mut pitch));
        assert!((yaw - (-1.1)).abs() < 1e-5);
        assert!((pitch - 0.2).abs() < 1e-5);

        // Full right stick down -> pitch increases and clamps at 1.15.
        assert!(apply_right_stick_orbit(Vec2::new(0.0, -1.0), 2.0, &mut yaw, &mut pitch));
        assert!((pitch - 1.15).abs() < 1e-5);

        // Full right stick up -> pitch decreases and clamps at -0.35.
        assert!(apply_right_stick_orbit(Vec2::new(0.0, 1.0), 2.0, &mut yaw, &mut pitch));
        assert!((pitch - (-0.35)).abs() < 1e-5);
    }

    #[test]
    fn test_garage_top_uikit_descriptions_fit_panel() {
        let iso = acvd_formats::vfs::repo_root().join(acvd_formats::vfs::X360_ISO);
        if !iso.is_file() {
            return;
        }
        let disc = Disc::open(&iso).unwrap();
        let mut images = Assets::<Image>::default();
        let kit = AcvdUiKit::load(&disc, &mut images).unwrap();
        // Left panel width 336 - 28 (outer pad) - 20 (help card pad) = 288px inner budget.
        let max_inner_w = 288.0;
        for &(title, tag, desc) in TOP_ITEMS {
            assert!(!title.is_empty() && !tag.is_empty());
            let sz = kit.measure_text(FontRole::Small, desc, 0.86);
            assert!(
                sz.x <= max_inner_w,
                "TOP_ITEMS[{title}] description overflows helpline box: {:.1}px > {max_inner_w}px ({desc:?})",
                sz.x
            );
        }
        for (i, desc) in OPTIONS_ROW_DESCS.iter().enumerate() {
            let sz = kit.measure_text(FontRole::Small, desc, 0.86);
            assert!(
                sz.x <= max_inner_w,
                "OPTIONS_ROW_DESCS[{i}] overflows helpline box: {:.1}px > {max_inner_w}px ({desc:?})",
                sz.x
            );
        }
    }

    #[test]
    fn test_escape_toggles_garage_options() {
        assert_eq!(
            escape_target_screen(GarageScreen::Top),
            GarageScreen::Options
        );
        assert_eq!(
            escape_target_screen(GarageScreen::Options),
            GarageScreen::Top
        );
        assert_eq!(
            escape_target_screen(GarageScreen::Paint),
            GarageScreen::Top
        );
        assert_eq!(
            escape_target_screen(GarageScreen::ViewMode),
            GarageScreen::Top
        );
    }

    #[test]
    fn test_wheel_adjust_mappings_and_direction() {
        assert_eq!(wheel_step_direction(1.0), 1);
        assert_eq!(wheel_step_direction(0.25), 1);
        assert_eq!(wheel_step_direction(-1.0), -1);
        assert_eq!(wheel_step_direction(-0.25), -1);
        assert_eq!(wheel_step_direction(0.0), 0);

        assert_eq!(
            UiAction::SelectOptionRow(1).wheel_adjust(),
            Some(WheelAdjust::UiScale)
        );
        assert_eq!(
            UiAction::UiScaleStep(-1).wheel_adjust(),
            Some(WheelAdjust::UiScale)
        );
        assert_eq!(
            UiAction::SelectOptionRow(2).wheel_adjust(),
            Some(WheelAdjust::HudSway)
        );
        assert_eq!(
            UiAction::HudSwayStep(-1).wheel_adjust(),
            Some(WheelAdjust::HudSway)
        );
        assert_eq!(
            UiAction::SelectOptionRow(3).wheel_adjust(),
            Some(WheelAdjust::Volume(AudioChannel::Master))
        );
        assert_eq!(
            UiAction::VolumeStep(AudioChannel::Music, 1).wheel_adjust(),
            Some(WheelAdjust::Volume(AudioChannel::Music))
        );
        assert_eq!(
            UiAction::PaintRgbDelta(0, 16).wheel_adjust(),
            Some(WheelAdjust::PaintRgb(0))
        );
        assert_eq!(
            UiAction::PaintCamoDelta(-1).wheel_adjust(),
            Some(WheelAdjust::PaintCamo)
        );
        assert_eq!(
            UiAction::DecalScaleStep(1).wheel_adjust(),
            Some(WheelAdjust::DecalScale)
        );
        assert_eq!(
            UiAction::DecalRotateStep(-1).wheel_adjust(),
            Some(WheelAdjust::DecalRotate)
        );
        assert_eq!(
            UiAction::DecalOpacityStep(1).wheel_adjust(),
            Some(WheelAdjust::DecalOpacity)
        );
        assert_eq!(
            UiAction::SortieMapStep(1).wheel_adjust(),
            Some(WheelAdjust::SortieMap)
        );
        assert_eq!(
            UiAction::SortieTargetStep(-1).wheel_adjust(),
            Some(WheelAdjust::SortieTarget)
        );
        assert_eq!(UiAction::ToggleFullscreen.wheel_adjust(), None);
        assert_eq!(UiAction::Back.wheel_adjust(), None);
    }

    #[test]
    fn test_slider_track_set_from_fill() {
        let mut s = GameSettings::default();
        s.set_ui_scale_from_fill(0.0);
        assert_eq!(s.ui_scale_percent(), 75);
        s.set_ui_scale_from_fill(1.0);
        assert_eq!(s.ui_scale_percent(), 150);
        s.set_ui_scale_from_fill(GameSettings::ui_scale_default_mark());
        assert_eq!(s.ui_scale_percent(), 100);

        s.set_hud_sway_from_fill(0.0);
        assert_eq!(s.hud_sway_percent(), 0);
        s.set_hud_sway_from_fill(0.5);
        assert_eq!(s.hud_sway_percent(), 50);
        s.set_hud_sway_from_fill(1.0);
        assert_eq!(s.hud_sway_percent(), 100);

        s.set_channel_from_fill(AudioChannel::Music, 0.37);
        assert_eq!(s.channel_percent(AudioChannel::Music), 37);

        let mut camos = paint::CustomCamoLibrary::default();
        camos.set_custom_scale_from_fill(0.0);
        assert!((camos.custom_scale_value() - paint::CustomCamoLibrary::SCALE_MIN).abs() < 1e-4);
        camos.set_custom_scale_from_fill(1.0);
        assert!((camos.custom_scale_value() - paint::CustomCamoLibrary::SCALE_MAX).abs() < 1e-4);
        camos.set_custom_scale_from_fill(paint::CustomCamoLibrary::custom_scale_default_mark());
        assert!((camos.custom_scale_value() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn test_paint_undo_history_lifecycle() {
        let mk = |coloring_id: u16| PaintSnapshot {
            coloring_id,
            custom_color: None,
            decal_layers: Vec::new(),
            selected_decal_id: None,
            ac_emblem_key: None,
            emblem_override: false,
        };
        let mut hist = PaintUndoHistory::default();
        assert_eq!(hist.undo(), None);
        assert_eq!(hist.redo(), None);

        hist.begin(mk(1));
        hist.record(mk(1)); // duplicate is ignored
        assert_eq!(hist.undo(), None);

        hist.record(mk(2));
        hist.record(mk(3));
        assert_eq!(hist.undo(), Some(&mk(2)));
        assert_eq!(hist.undo(), Some(&mk(1)));
        assert_eq!(hist.undo(), None);
        assert_eq!(hist.redo(), Some(&mk(2)));

        // Recording after undo truncates the redo tail.
        hist.record(mk(4));
        assert_eq!(hist.redo(), None);
        assert_eq!(hist.undo(), Some(&mk(2)));

        // Closing Paint resets history.
        hist.reset();
        assert_eq!(hist.undo(), None);
        assert_eq!(hist.redo(), None);
    }

    #[test]
    fn test_color_picker_track_normalization_and_textures() {
        // Center = (200, 100), Size = (100, 80) -> rect x=[150, 250], y=[60, 140]
        let (uv, inside) = normalize_track_point(Vec2::new(200.0, 100.0), Vec2::new(100.0, 80.0), Vec2::new(200.0, 100.0));
        assert!(inside);
        assert!((uv.x - 0.5).abs() < 1e-5 && (uv.y - 0.5).abs() < 1e-5);

        // Top-left corner -> (0, 0) inside
        let (uv, inside) = normalize_track_point(Vec2::new(200.0, 100.0), Vec2::new(100.0, 80.0), Vec2::new(150.0, 60.0));
        assert!(inside);
        assert!((uv.x - 0.0).abs() < 1e-5 && (uv.y - 0.0).abs() < 1e-5);

        // Dragged outside right/bottom -> clamped to (1, 1), inside = false
        let (uv, inside) = normalize_track_point(Vec2::new(200.0, 100.0), Vec2::new(100.0, 80.0), Vec2::new(320.0, 200.0));
        assert!(!inside);
        assert!((uv.x - 1.0).abs() < 1e-5 && (uv.y - 1.0).abs() < 1e-5);

        // PaintPickerAssets builds both SV square and Hue bar textures and updates SV on hue change.
        let mut images = Assets::<Image>::default();
        let mut assets = PaintPickerAssets::default();
        assets.ensure_loaded(0.0, &mut images);
        let sv_h = assets.sv_image.clone().expect("sv_image created");
        let hue_h = assets.hue_image.clone().expect("hue_image created");

        // Top-right corner of SV square at Hue=0° (red) should be pure red [255, 0, 0, 255].
        let sv_img = images.get(&sv_h).unwrap();
        let sv_data = sv_img.data.as_ref().unwrap();
        let top_right = ((PaintPickerAssets::SV_W - 1) * 4) as usize;
        assert_eq!(&sv_data[top_right..top_right + 4], &[255, 0, 0, 255]);
        // Bottom-left corner (S=0, V=0) should be black [0, 0, 0, 255].
        let bot_left = ((PaintPickerAssets::SV_H - 1) * PaintPickerAssets::SV_W * 4) as usize;
        assert_eq!(&sv_data[bot_left..bot_left + 4], &[0, 0, 0, 255]);

        // Hue bar starts at red (0°).
        let hue_img = images.get(&hue_h).unwrap();
        let hue_data = hue_img.data.as_ref().unwrap();
        assert_eq!(&hue_data[0..4], &[255, 0, 0, 255]);

        // Switching Hue to 120° updates top-right of SV square to pure green [0, 255, 0, 255].
        assets.ensure_loaded(120.0, &mut images);
        let sv_data = images.get(&sv_h).unwrap().data.as_ref().unwrap();
        assert_eq!(&sv_data[top_right..top_right + 4], &[0, 255, 0, 255]);
    }
}
