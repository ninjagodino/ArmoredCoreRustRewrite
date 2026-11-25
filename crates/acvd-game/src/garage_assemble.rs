//! Authentic ACVD Garage Assemble UI (`lang/en/menu/assemble.drb.dcx` + real disc thumbnails +
//! live 3D part/weapon preview).
//!
//! Traced directly from the Xbox 360 retail executable and decompiled Lua scripts:
//! - Layout: `lang/en/menu/assemble.drb.dcx` + `lang/en/menu/assemble.tpf.dcx`
//!   (`private/lua360/script/assemble/assemblescene.lua` -> `AssembleAndShopSub__CreateDialog`):
//!   - `@ModelBackImage`: left/right tactical corner-reticle frames (`汎用ベース_左` `[173,220,686,567]`,
//!     `汎用ベース_右` `[705,109,1213,567]`).
//!   - `@PartsSelect`: horizontal slot strip `@PartsCategoryList` `[86,148,686,202]` (7 × `84×54`)
//!     and vertical part thumbnail strip `@PartsList` `[86,220,170,562]` (6 × `84×54`).
//!   - `@PartsNormal`: category header `@CategoryName`, `@PartsFrameName` (`@SubCategory`,
//!     `@PartsName`, `@Signature`), `@PartsLayout` `[178,231,676,556]` (3D part preview),
//!     `@Param2..@Param5` + `@Param0..@Param1` (6 stat comparison rows), `@MachineInfoFrame`
//!     (Load/Energy capacity bars + `@WeightError`/`@EnergyError` hazard stripes), and
//!     `@2Line_Helpline` (`@Text_1`/`@Text_2`).
//!   - `@PartsDetail` + `@SpecDetail`: expanded 20-row part spec + 11-row AC spec view
//!     (`AssembleAndShopSub__OnBtnAccel_ChangeView`, toggled with `Tab`).
//! - Part thumbnails (`0x824c1330`, `image/partsthumbnail/<dir>/<prefix><model_id:04>.tpf.dcx`,
//!   128×64 DXT1 per part, `0000` for empty).
//! - Localized strings:
//!   - `lang/en/text/partsname_en.fmg` (`category * 10_000 + part_id`)
//!   - `lang/en/text/partssubcategory_en.fmg` (`category * 100 + subcategory`)
//!   - `lang/en/text/partsexplain_en.fmg` (`category * 10_000 + part_id`)

use std::collections::{BTreeSet, HashMap};

use acvd_data::ac_part;
use acvd_data::generated::ac_unit::AcAssemblyDesignSt;
use acvd_data::generated::assembly::AC_ASSEMBLY_DESIGN_ST_SLOTS;
use acvd_formats::accolor::AcColor;
use acvd_formats::fmg::Fmg;
use acvd_formats::vfs::Disc;
use acvd_render::menu::{self, Layout, MenuObject, MenuRoot, MenuText};
use acvd_render::ui_kit::{palette, AcvdUiKit, ButtonState, FontRole, PanelStyle, TraceStyle};
use acvd_render::Packs;
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::camera::RenderTarget;
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit};
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::ui::px;

use crate::assemble;
use crate::blur::ActiveMenuBackdrop;
use crate::decal::{self, DecalSlot, DecalState};
use crate::garage::{
    part_name, parts_for_slot, AcSpecs, AssembleSlot, GarageScreen, GarageState, GarageUi, Mode,
    UiAction, UiRow,
};
use crate::input::InputDevice;
use crate::paint;
use crate::pose;

/// Dedicated Bevy render layer for the offscreen 3D part/weapon preview studio (`@PartsLayout`).
pub const PART_PREVIEW_LAYER: usize = 4;
/// Dedicated Bevy render layer for the offscreen 3D full-AC preview studio (`@AcLayout` / `ASSEMBLY VIEW`).
pub const AC_PREVIEW_LAYER: usize = 5;

const ASSEMBLE_DRB: &str = "lang/en/menu/assemble.drb.dcx";
const SCREEN_W: f32 = 1280.0;
const SCREEN_H: f32 = 720.0;
/// Offscreen render target dimensions for `@PartsLayout` upper preview window (`[178,231,676,412]` = 498×181).
const PREVIEW_TEX_W: u32 = 512;
const PREVIEW_TEX_H: u32 = 186;
/// Offscreen render target dimensions for `@AcLayout` (`ASSEMBLY VIEW`, `[705,109,1213,567]` = 508×458).
const AC_PREVIEW_TEX_W: u32 = 768;
const AC_PREVIEW_TEX_H: u32 = 692;
/// Number of visible part thumbnails in `@PartsList` (`[86,220,170,562]`, 6 × 57px).
pub const ASSEMBLE_THUMB_WINDOW: usize = 6;
/// Number of visible slot tabs in `@PartsCategoryList` (`[86,148,686,202]`, 7 × 86px).
pub const ASSEMBLE_CAT_WINDOW: usize = 7;

/// Which scrollable thumbnail strip in `AssembleParts` the mouse cursor is hovering over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssembleScrollStrip {
    /// Top horizontal slot tab bar `@PartsCategoryList` (including `<` / `>` arrows).
    CategoryList,
    /// Left vertical part thumbnail strip `@PartsList` (including `^` / `v` arrows).
    PartsList,
}

/// Default camera position for the offscreen 3D part/weapon preview studio (`zoom == 1.0`).
pub const PREVIEW_CAM_BASE: Vec3 = Vec3::new(0.0, 0.55, 3.15);
/// Minimum camera zoom multiplier (closest inspection distance) for the 3D part preview.
pub const PREVIEW_ZOOM_MIN: f32 = 0.45;
/// Maximum camera zoom multiplier (farthest pull-back distance) for the 3D part preview.
pub const PREVIEW_ZOOM_MAX: f32 = 2.20;
/// Maximum vertical pitch tilt (radians) when orbiting the 3D part preview with the mouse.
pub const PREVIEW_PITCH_LIMIT: f32 = 1.25;

/// Converts a window-space cursor coordinate into the virtual `1280×720` DRB canvas space,
/// using the exact same letterbox transform as [`spawn_assemble_ui`].
fn window_to_virtual(window_size: Vec2, cursor: Vec2) -> Vec2 {
    let scale = (window_size.x / SCREEN_W)
        .min(window_size.y / SCREEN_H)
        .max(0.5);
    let origin = Vec2::new(
        ((window_size.x - SCREEN_W * scale) * 0.5).round(),
        ((window_size.y - SCREEN_H * scale) * 0.5).round(),
    );
    (cursor - origin) / scale
}

/// Maps a window-space cursor position to the hovered scrollable strip in `AssembleParts`,
/// using the exact same virtual `1280×720` layout transform as [`spawn_assemble_ui`].
pub fn hovered_scroll_strip(window_size: Vec2, cursor: Vec2) -> Option<AssembleScrollStrip> {
    let v = window_to_virtual(window_size, cursor);
    if (58.0..=714.0).contains(&v.x) && (148.0..=202.0).contains(&v.y) {
        Some(AssembleScrollStrip::CategoryList)
    } else if (86.0..=170.0).contains(&v.x) && (202.0..=584.0).contains(&v.y) {
        Some(AssembleScrollStrip::PartsList)
    } else {
        None
    }
}

/// True when the cursor is hovering over the left `PARTS VIEW` panel (`[173,220,686,567]` in
/// virtual `1280×720` coordinates), where mouse drag rotates and mouse wheel zooms the 3D part.
pub fn cursor_in_part_preview_panel(window_size: Vec2, cursor: Vec2) -> bool {
    let v = window_to_virtual(window_size, cursor);
    (173.0..=686.0).contains(&v.x) && (220.0..=567.0).contains(&v.y)
}

/// True when the cursor is hovering over the right `ASSEMBLY VIEW` panel (`[705,109,1213,567]` in
/// virtual `1280×720` coordinates), where mouse drag orbits and mouse wheel zooms the preview AC.
pub fn cursor_in_ac_preview_panel(window_size: Vec2, cursor: Vec2) -> bool {
    let v = window_to_virtual(window_size, cursor);
    (705.0..=1213.0).contains(&v.x) && (109.0..=567.0).contains(&v.y)
}

/// Default hero orbit yaw (radians) for the right-side `ASSEMBLY VIEW` preview AC per slot.
pub fn ac_preview_default_yaw(slot: AssembleSlot) -> f32 {
    match slot {
        AssembleSlot::Head
        | AssembleSlot::Core
        | AssembleSlot::Fcs
        | AssembleSlot::Generator
        | AssembleSlot::Recon => 0.28,
        AssembleSlot::Arms | AssembleSlot::Legs => 0.36,
        AssembleSlot::ArmUnitR | AssembleSlot::HangerR => 0.46,
        AssembleSlot::ArmUnitL | AssembleSlot::HangerL => -0.50,
        AssembleSlot::Shoulder => -0.38,
        AssembleSlot::Booster | AssembleSlot::OveredWeapon => 2.55,
    }
}

/// Computes the offscreen `ASSEMBLY VIEW` studio camera transform for a given AC height and zoom.
pub fn ac_preview_camera_transform(ac_height: f32, zoom: f32) -> Transform {
    let h = ac_height.clamp(4.0, 10.0);
    let z = zoom.clamp(0.55, 1.85);
    let dist = h * 2.30 * z;
    // Slight downward look target nudges the AC slightly up above the Load/Energy bars.
    let target = Vec3::new(0.0, -h * 0.05, 0.0);
    Transform::from_translation(Vec3::new(0.0, h * 0.14, dist)).looking_at(target, Vec3::Y)
}

/// Applies mouse-drag delta (pixels) to the 3D part preview yaw & pitch.
pub fn apply_part_preview_drag(yaw: &mut f32, pitch: &mut f32, delta: Vec2) {
    *yaw = (*yaw + delta.x * 0.01).rem_euclid(std::f32::consts::TAU);
    *pitch = (*pitch + delta.y * 0.01).clamp(-PREVIEW_PITCH_LIMIT, PREVIEW_PITCH_LIMIT);
}

/// Applies mouse-wheel scroll delta to the 3D part preview zoom distance multiplier.
pub fn apply_part_preview_zoom(zoom: &mut f32, scroll_y: f32, unit: MouseScrollUnit) {
    let step = match unit {
        MouseScrollUnit::Line => scroll_y,
        MouseScrollUnit::Pixel => scroll_y * 0.02,
    };
    *zoom = (*zoom * (1.0 - step * 0.12)).clamp(PREVIEW_ZOOM_MIN, PREVIEW_ZOOM_MAX);
}

/// Computes the offscreen studio camera transform for a given preview zoom multiplier.
pub fn part_preview_camera_transform(zoom: f32) -> Transform {
    let z = zoom.clamp(PREVIEW_ZOOM_MIN, PREVIEW_ZOOM_MAX);
    Transform::from_translation(PREVIEW_CAM_BASE * z).looking_at(Vec3::ZERO, Vec3::Y)
}

/// Shared marker on offscreen Assemble preview cameras (both `PARTS VIEW` and `ASSEMBLY VIEW`)
/// so main-camera systems (`env`, `lens_flare`, `garage::update_camera`, `main`) exclude them.
#[derive(Component)]
pub struct PartPreviewCamera;

/// Marker specific to the left `PARTS VIEW` offscreen camera.
#[derive(Component)]
pub struct PartOnlyPreviewCamera;

/// Marker on studio entities (camera + lights) for the 3D part preview.
#[derive(Component)]
pub struct PartPreviewStudio;

/// Marker on the rotating pivot holding the currently previewed 3D part/weapon FLVER.
#[derive(Component)]
pub struct PartPreviewPivot;

/// Marker on individual submesh entities inside the 3D part preview.
#[derive(Component)]
pub struct PartPreviewMesh;

/// Marker on studio entities (camera + lights) for the right-side `ASSEMBLY VIEW` full-AC preview.
#[derive(Component)]
pub struct AcPreviewStudio;

/// Marker specific to the right-side `ASSEMBLY VIEW` offscreen camera.
#[derive(Component)]
pub struct AcPreviewCamera;

/// Marker on the rotating pivot holding the full preview AC in `ASSEMBLY VIEW`.
#[derive(Component)]
pub struct AcPreviewPivot;

/// Marker on individual submesh entities inside the `ASSEMBLY VIEW` full preview AC.
/// [`sync_part_hologram`] targets ONLY entities with this marker so the main garage mech stays
/// solid and untouched in the blurred background.
#[derive(Component)]
pub struct AcPreviewMesh;

type PreviewKey = (u8, u16, bool, u64, u64);
/// Key uniquely identifying the visible 3D state of the `ASSEMBLY VIEW` preview AC.
type AcPreviewKey = ([u16; 11], u16, u64);

fn ac_preview_key(design: &AcAssemblyDesignSt, color_hash: u64) -> AcPreviewKey {
    (
        [
            AssembleSlot::Head.get(design),
            AssembleSlot::Core.get(design),
            AssembleSlot::Arms.get(design),
            AssembleSlot::Legs.get(design),
            AssembleSlot::Booster.get(design),
            AssembleSlot::ArmUnitR.get(design),
            AssembleSlot::ArmUnitL.get(design),
            AssembleSlot::Shoulder.get(design),
            AssembleSlot::HangerR.get(design),
            AssembleSlot::HangerL.get(design),
            AssembleSlot::OveredWeapon.get(design),
        ],
        design.coloring_id,
        color_hash,
    )
}

#[derive(Clone)]
pub enum PreviewMat {
    Painted(Handle<paint::AcPaintMaterial>),
    Std(Handle<StandardMaterial>),
}

#[derive(Clone)]
pub struct CachedPreviewMesh {
    pub mesh: Handle<Mesh>,
    pub mat: PreviewMat,
    pub transform: Transform,
}

/// Cached retail DRB layout, FMG text banks, per-part thumbnail handles, and 3D preview target.
#[derive(Resource)]
pub struct AssembleUiAssets {
    pub initialized: bool,
    pub layout: Option<Layout>,
    pub kit: AcvdUiKit,
    pub subcat_fmg: Option<Fmg>,
    pub explain_fmg: Option<Fmg>,
    /// Cached `image/partsthumbnail/<dir>/<prefix><model_id:04>.tpf.dcx` textures.
    pub thumbnails: HashMap<(u8, u16), Handle<Image>>,
    /// Offscreen RGBA8 render target for the 3D part preview camera.
    pub preview_image: Option<Handle<Image>>,
    /// Currently spawned `(category, part_id, slot_is_left, paint_rev)` in the 3D preview studio.
    pub preview_loaded: Option<PreviewKey>,
    /// Short debounce before loading an uncached 3D FLVER from disc while rapidly scrolling.
    pub preview_pending: Option<(PreviewKey, f32)>,
    /// Cached 3D preview meshes & materials per `(category, part_id, is_left, paint_rev)`.
    pub preview_cache: HashMap<PreviewKey, Vec<CachedPreviewMesh>>,
    pub preview_yaw: f32,
    pub preview_pitch: f32,
    pub preview_zoom: f32,
    pub preview_dragging: bool,
    pub preview_manual: bool,
    /// Offscreen RGBA8 render target for the right-side `ASSEMBLY VIEW` full-AC preview camera.
    pub ac_preview_image: Option<Handle<Image>>,
    /// Currently spawned preview AC loadout key in `AC_PREVIEW_LAYER`.
    pub ac_preview_loaded: Option<AcPreviewKey>,
    /// Short debounce before rebuilding the preview AC while rapidly scrolling parts.
    pub ac_preview_pending: Option<(AcPreviewKey, f32)>,
    /// Last active slot in `AssembleParts`, used to smoothly re-angle the preview AC on slot switch.
    pub ac_preview_last_slot: Option<AssembleSlot>,
    pub ac_preview_yaw: f32,
    pub ac_preview_pitch: f32,
    pub ac_preview_zoom: f32,
    pub ac_preview_height: f32,
    pub ac_preview_dragging: bool,
    pub ac_preview_manual: bool,
    /// Dedicated TPF pack cache for 128×64 part thumbnails.
    pub packs: Packs,
}

impl Default for AssembleUiAssets {
    fn default() -> Self {
        Self {
            initialized: false,
            layout: None,
            kit: AcvdUiKit::default(),
            subcat_fmg: None,
            explain_fmg: None,
            thumbnails: HashMap::new(),
            preview_image: None,
            preview_loaded: None,
            preview_pending: None,
            preview_cache: HashMap::new(),
            preview_yaw: 0.0,
            preview_pitch: 0.0,
            preview_zoom: 1.0,
            preview_dragging: false,
            preview_manual: false,
            ac_preview_image: None,
            ac_preview_loaded: None,
            ac_preview_pending: None,
            ac_preview_last_slot: None,
            ac_preview_yaw: 0.30,
            ac_preview_pitch: 0.08,
            ac_preview_zoom: 1.0,
            ac_preview_height: 6.8,
            ac_preview_dragging: false,
            ac_preview_manual: false,
            packs: Packs::default(),
        }
    }
}

impl AssembleUiAssets {
    /// Resets manual orbit/zoom back to the default turntable framing.
    pub fn reset_preview_view(&mut self) {
        self.preview_pitch = 0.0;
        self.preview_zoom = 1.0;
        self.preview_dragging = false;
        self.preview_manual = false;
    }
    pub fn ensure_loaded(&mut self, disc: &Disc, images: &mut Assets<Image>) {
        if self.initialized {
            return;
        }
        self.initialized = true;
        match menu::load(disc, ASSEMBLE_DRB, images) {
            Ok(layout) => {
                self.kit = AcvdUiKit::from_layout(&layout);
                self.kit.load_icons(disc, images);
                self.layout = Some(layout);
            }
            Err(e) => warn!("assemble UI: failed to load {ASSEMBLE_DRB}: {e:#}"),
        }
        self.subcat_fmg = disc
            .asset("lang/en/text/partssubcategory_en.fmg")
            .ok()
            .and_then(|b| acvd_formats::fmg::read(&b).ok());
        self.explain_fmg = disc
            .asset("lang/en/text/partsexplain_en.fmg")
            .ok()
            .and_then(|b| acvd_formats::fmg::read(&b).ok());

        // Allocate transparent RGBA8 offscreen render target for the 3D part preview.
        let mut target = Image::new_target_texture(
            PREVIEW_TEX_W,
            PREVIEW_TEX_H,
            TextureFormat::Rgba8UnormSrgb,
            None,
        );
        target.sampler = bevy::image::ImageSampler::linear();
        self.preview_image = Some(images.add(target));

        // Allocate transparent RGBA8 offscreen render target for the right-side `ASSEMBLY VIEW` AC preview.
        let mut ac_target = Image::new_target_texture(
            AC_PREVIEW_TEX_W,
            AC_PREVIEW_TEX_H,
            TextureFormat::Rgba8UnormSrgb,
            None,
        );
        ac_target.sampler = bevy::image::ImageSampler::linear();
        self.ac_preview_image = Some(images.add(ac_target));
    }

    /// Loads (or returns cached) the authentic 128×64 disc thumbnail for `(category, part_id)`.
    /// `part_id == 0` loads the category's `0000` empty-slot icon (`hd0000`, `hu0000`, etc.).
    pub fn thumbnail(
        &mut self,
        disc: &Disc,
        images: &mut Assets<Image>,
        category: u8,
        part_id: u16,
    ) -> Option<Handle<Image>> {
        let model_id = if part_id == 0 {
            0
        } else {
            ac_part(category, part_id)
                .map(|p| p.model_id)
                .unwrap_or(part_id)
        };
        let key = (category, model_id);
        if let Some(h) = self.thumbnails.get(&key) {
            return Some(h.clone());
        }
        let (dir, prefix) = thumbnail_dir_prefix(category)?;
        let stem = format!("{prefix}{model_id:04}");
        let pack = format!("image/partsthumbnail/{dir}/{stem}.tpf.dcx");
        let tref = acvd_data::textures_named(&stem)
            .find(|r| r.pack.eq_ignore_ascii_case(&pack))
            .or_else(|| {
                // Fallback to empty icon if a special/variant model_id lacks a dedicated thumbnail.
                let zstem = format!("{prefix}0000");
                let zpack = format!("image/partsthumbnail/{dir}/{zstem}.tpf.dcx");
                let found =
                    acvd_data::textures_named(&zstem).find(|r| r.pack.eq_ignore_ascii_case(&zpack));
                found
            })?;
        let mut img = self.packs.texture(disc, tref).ok()?;
        img.sampler = bevy::image::ImageSampler::linear();
        let handle = images.add(img);
        self.thumbnails.insert(key, handle.clone());
        Some(handle)
    }
}

/// Disc path components for `0x824c1330` (`image:/partsthumbnail/%s/%s%04d.tpf.dcx`).
pub fn thumbnail_dir_prefix(category: u8) -> Option<(&'static str, &'static str)> {
    Some(match category {
        0 => ("head", "hd"),
        1 => ("core", "cr"),
        2 => ("arm", "am"),
        3 => ("leg", "lg"),
        4 => ("fcs", "fs"),
        5 => ("gene", "gn"),
        6 => ("boost", "bs"),
        10 => ("hand", "hu"),
        11 => ("ow", "ow"),
        12 => ("shoul", "sh"),
        13 => ("recon", "rc"),
        _ => return None,
    })
}

/// Returns the `acvparts.bin` byte `+0x0f` subcategory ID for a part (0 for empty / OW).
pub fn part_subcategory(category: u8, part_id: u16) -> u8 {
    if part_id == 0 {
        return 0;
    }
    let field = match category {
        10 => "weapon_kind",
        13 => "recon_subcategory",
        _ => "subcategory",
    };
    acvd_data::part_field(part_id as i64, category, field) as u8
}

/// Resolves the localized subcategory label from `lang/en/text/partssubcategory_en.fmg`.
pub fn subcategory_name(fmg: Option<&Fmg>, category: u8, subcat: u8) -> String {
    if subcat == 0 {
        return match category {
            11 => "OVERED WEAPON".to_string(),
            _ => "STANDARD".to_string(),
        };
    }
    let key = i32::from(category) * 100 + i32::from(subcat);
    if let Some(s) = fmg.and_then(|f| f.get(key)) {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    format!("TYPE {subcat}")
}

/// Compact label for subcategory filter chips so weapon-type strips stay clean and readable.
fn short_subcat_label(full: &str) -> String {
    let s = full
        .replace("-RESISTANT TYPE", "")
        .replace(" TYPE", "")
        .replace("REVERSE JOINTED-", "REV-")
        .replace("BIPEDAL-", "BIPED-")
        .replace("H.E.A.T. ", "HEAT ")
        .replace("MACHINE GUN", "MG")
        .replace("SNIPER ", "SNIPER ")
        .replace("PHYSICAL ", "PHYS ");
    if s.chars().count() > 14 {
        let cut: String = s.chars().take(13).collect();
        format!("{cut}.")
    } else {
        s
    }
}

/// Resolves the part's English description from `lang/en/text/partsexplain_en.fmg`.
pub fn part_description(fmg: Option<&Fmg>, category: u8, part_id: u16) -> String {
    if part_id == 0 {
        return "Unequip this slot to reduce AC weight and energy consumption.".to_string();
    }
    let key = i32::from(category) * 10_000 + i32::from(part_id);
    if let Some(s) = fmg.and_then(|f| f.get(key)) {
        let cleaned = s.replace('\r', "").replace('\n', " ").trim().to_string();
        if !cleaned.is_empty() {
            return cleaned;
        }
    }
    String::new()
}

/// Returns `(damage_type_code, short_tag, color)` for weapon categories (10, 11, 12).
/// `0 = KE`, `6 = CE`, `1 = TE`, `4 = OW`.
pub fn part_damage_badge(category: u8, part_id: u16) -> Option<(u8, &'static str, Color)> {
    if part_id == 0 || !matches!(category, 10 | 11 | 12) {
        return None;
    }
    let dt = acvd_data::part_field(part_id as i64, category, "damage_type") as u8;
    let power = acvd_data::part_field(part_id as i64, category, "damage_power");
    if power <= 0.0 && category == 10 {
        return None;
    }
    Some(match dt {
        0 => (0, "KE", Color::srgb(0.95, 0.72, 0.24)),
        6 => (6, "CE", Color::srgb(0.32, 0.88, 0.45)),
        1 => (1, "TE", Color::srgb(0.32, 0.76, 0.98)),
        4 => (4, "OW", Color::srgb(0.98, 0.35, 0.28)),
        _ => return None,
    })
}

/// True when `slot` holds weapons that support KE / CE / TE damage-type filtering.
pub fn slot_has_damage_filter(slot: AssembleSlot) -> bool {
    matches!(
        slot,
        AssembleSlot::ArmUnitR
            | AssembleSlot::ArmUnitL
            | AssembleSlot::HangerR
            | AssembleSlot::HangerL
            | AssembleSlot::Shoulder
    )
}

/// Lists all distinct `(subcategory_id, localized_name)` tabs available for `slot`.
pub fn subcategories_for_slot(slot: AssembleSlot, subcat_fmg: Option<&Fmg>) -> Vec<(u8, String)> {
    let cat = slot.category();
    let mut seen = BTreeSet::new();
    for pid in parts_for_slot(slot) {
        if pid == 0 {
            continue;
        }
        let sub = part_subcategory(cat, pid);
        if sub > 0 {
            seen.insert(sub);
        }
    }
    seen.into_iter()
        .map(|s| (s, subcategory_name(subcat_fmg, cat, s)))
        .collect()
}

/// Returns the part IDs for `slot` filtered by optional subcategory and damage type.
pub fn filtered_parts_for_slot(
    slot: AssembleSlot,
    subcat: Option<u8>,
    dmg_filter: Option<u8>,
) -> Vec<u16> {
    let cat = slot.category();
    let all = parts_for_slot(slot);
    if subcat.is_none() && dmg_filter.is_none() {
        return all;
    }
    let mut out = Vec::with_capacity(all.len());
    for pid in all {
        if pid == 0 {
            // Keep (EMPTY) at the top only when no filter is active.
            continue;
        }
        if let Some(want_sub) = subcat {
            if part_subcategory(cat, pid) != want_sub {
                continue;
            }
        }
        if let Some(want_dmg) = dmg_filter {
            let matches_dmg = part_damage_badge(cat, pid)
                .map(|(dt, _, _)| dt == want_dmg)
                .unwrap_or(false);
            if !matches_dmg {
                continue;
            }
        }
        out.push(pid);
    }
    out
}

/// One stat comparison row (`Name | Cur > New`) with retail color direction.
#[derive(Debug, Clone)]
pub struct StatRow {
    pub label: &'static str,
    pub cur: String,
    pub new: String,
    /// `+1` = better (blue `#44bbff`), `-1` = worse (red `#ff4a44`), `0` = equal (white).
    pub cmp: i8,
}

fn fmt_comma_i32(v: i32) -> String {
    let neg = v < 0;
    let digits = v.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + 3);
    if neg {
        out.push('-');
    }
    let len = digits.len();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn stat_i32(label: &'static str, cur: f64, new: f64, higher_is_better: bool) -> StatRow {
    let c = cur.round() as i32;
    let n = new.round() as i32;
    let cmp = if n == c {
        0
    } else if (n > c) == higher_is_better {
        1
    } else {
        -1
    };
    StatRow {
        label,
        cur: fmt_comma_i32(c),
        new: fmt_comma_i32(n),
        cmp,
    }
}

fn stat_f32(label: &'static str, cur: f64, new: f64, higher_is_better: bool) -> StatRow {
    let diff = new - cur;
    let cmp = if diff.abs() < 0.05 {
        0
    } else if (diff > 0.0) == higher_is_better {
        1
    } else {
        -1
    };
    StatRow {
        label,
        cur: format!("{cur:.1}"),
        new: format!("{new:.1}"),
        cmp,
    }
}

fn pf(part_id: u16, cat: u8, field: &str) -> f64 {
    if part_id == 0 {
        0.0
    } else {
        acvd_data::part_field(part_id as i64, cat, field) as f64
    }
}

/// Builds the authentic ACVD part stat comparison rows for `slot`.
/// - When `detail == false` (`@PartsNormal`): returns 6 rows: `[Param2, Param3, Param4, Param5, Weight(Param0), EN(Param1)]`.
/// - When `detail == true` (`@PartsDetail`): returns up to 20 rows (all detailed stats + Weight/EN at the end).
pub fn part_stat_rows(slot: AssembleSlot, cur_id: u16, sel_id: u16, detail: bool) -> Vec<StatRow> {
    let c = slot.category();
    let mut top = Vec::new();
    match c {
        0 => {
            // HEAD
            top.push(stat_i32(
                "AP",
                pf(cur_id, c, "ap"),
                pf(sel_id, c, "ap"),
                true,
            ));
            top.push(stat_i32(
                "KE Defense",
                pf(cur_id, c, "ke_def"),
                pf(sel_id, c, "ke_def"),
                true,
            ));
            top.push(stat_i32(
                "CE Defense",
                pf(cur_id, c, "ce_def"),
                pf(sel_id, c, "ce_def"),
                true,
            ));
            top.push(stat_i32(
                "TE Defense",
                pf(cur_id, c, "te_def"),
                pf(sel_id, c, "te_def"),
                true,
            ));
            if detail {
                top.push(stat_i32(
                    "Stability",
                    pf(cur_id, c, "balance"),
                    pf(sel_id, c, "balance"),
                    true,
                ));
                top.push(stat_i32(
                    "Camera Perf.",
                    pf(cur_id, c, "camera_ability"),
                    pf(sel_id, c, "camera_ability"),
                    true,
                ));
                top.push(stat_i32(
                    "Scan Perf.",
                    pf(cur_id, c, "scan_performance"),
                    pf(sel_id, c, "scan_performance"),
                    true,
                ));
            }
        }
        1 => {
            // CORE
            top.push(stat_i32(
                "AP",
                pf(cur_id, c, "ap"),
                pf(sel_id, c, "ap"),
                true,
            ));
            top.push(stat_i32(
                "KE Defense",
                pf(cur_id, c, "ke_def"),
                pf(sel_id, c, "ke_def"),
                true,
            ));
            top.push(stat_i32(
                "CE Defense",
                pf(cur_id, c, "ce_def"),
                pf(sel_id, c, "ce_def"),
                true,
            ));
            top.push(stat_i32(
                "TE Defense",
                pf(cur_id, c, "te_def"),
                pf(sel_id, c, "te_def"),
                true,
            ));
            if detail {
                top.push(stat_i32(
                    "EN Conductivity",
                    pf(cur_id, c, "en_conductivity"),
                    pf(sel_id, c, "en_conductivity"),
                    true,
                ));
                top.push(stat_i32(
                    "Recon Storage",
                    pf(cur_id, c, "recon_capacity"),
                    pf(sel_id, c, "recon_capacity"),
                    true,
                ));
                top.push(stat_i32(
                    "Booster Output",
                    pf(cur_id, c, "vs_hi_en"),
                    pf(sel_id, c, "vs_hi_en"),
                    true,
                ));
            }
        }
        2 => {
            // ARMS (matches user's reference screenshot!)
            top.push(stat_i32(
                "AP",
                pf(cur_id, c, "ap"),
                pf(sel_id, c, "ap"),
                true,
            ));
            top.push(stat_i32(
                "KE Defense",
                pf(cur_id, c, "ke_def"),
                pf(sel_id, c, "ke_def"),
                true,
            ));
            top.push(stat_i32(
                "CE Defense",
                pf(cur_id, c, "ce_def"),
                pf(sel_id, c, "ce_def"),
                true,
            ));
            top.push(stat_i32(
                "TE Defense",
                pf(cur_id, c, "te_def"),
                pf(sel_id, c, "te_def"),
                true,
            ));
            if detail {
                top.push(stat_i32(
                    "Firing Stability",
                    pf(cur_id, c, "shoot_stability"),
                    pf(sel_id, c, "shoot_stability"),
                    true,
                ));
                top.push(stat_i32(
                    "Shoulder Mounts",
                    pf(cur_id, c, "shoulder_mount_count"),
                    pf(sel_id, c, "shoulder_mount_count"),
                    true,
                ));
            }
        }
        3 => {
            // LEGS
            top.push(stat_i32(
                "AP",
                pf(cur_id, c, "ap"),
                pf(sel_id, c, "ap"),
                true,
            ));
            top.push(stat_i32(
                "KE Defense",
                pf(cur_id, c, "ke_def"),
                pf(sel_id, c, "ke_def"),
                true,
            ));
            top.push(stat_i32(
                "CE Defense",
                pf(cur_id, c, "ce_def"),
                pf(sel_id, c, "ce_def"),
                true,
            ));
            top.push(stat_i32(
                "TE Defense",
                pf(cur_id, c, "te_def"),
                pf(sel_id, c, "te_def"),
                true,
            ));
            if detail {
                top.push(stat_i32(
                    "Load Capacity",
                    pf(cur_id, c, "payload"),
                    pf(sel_id, c, "payload"),
                    true,
                ));
                top.push(stat_i32(
                    "Movement Perf.",
                    pf(cur_id, c, "move_ability"),
                    pf(sel_id, c, "move_ability"),
                    true,
                ));
                top.push(stat_i32(
                    "Jump Perf.",
                    pf(cur_id, c, "jump_ability"),
                    pf(sel_id, c, "jump_ability"),
                    true,
                ));
                top.push(stat_i32(
                    "Turning Perf.",
                    pf(cur_id, c, "turn_ability"),
                    pf(sel_id, c, "turn_ability"),
                    true,
                ));
            }
        }
        4 => {
            // FCS
            top.push(stat_i32(
                "Lock-On Range",
                pf(cur_id, c, "lock_range"),
                pf(sel_id, c, "lock_range"),
                true,
            ));
            top.push(stat_i32(
                "Lock Compute",
                pf(cur_id, c, "lock_calc_speed"),
                pf(sel_id, c, "lock_calc_speed"),
                true,
            ));
            top.push(stat_i32(
                "Missile Lock",
                pf(cur_id, c, "missile_lock_calc_speed"),
                pf(sel_id, c, "missile_lock_calc_speed"),
                true,
            ));
            top.push(stat_i32(
                "Max Locks",
                pf(cur_id, c, "missile_lock_max"),
                pf(sel_id, c, "missile_lock_max"),
                true,
            ));
            if detail {
                top.push(stat_i32(
                    "Sight Width",
                    pf(cur_id, c, "lock_sight_x"),
                    pf(sel_id, c, "lock_sight_x"),
                    true,
                ));
                top.push(stat_i32(
                    "Sight Height",
                    pf(cur_id, c, "lock_sight_y"),
                    pf(sel_id, c, "lock_sight_y"),
                    true,
                ));
            }
        }
        5 => {
            // GENERATOR
            top.push(stat_i32(
                "EN Capacity",
                pf(cur_id, c, "en_capacity"),
                pf(sel_id, c, "en_capacity"),
                true,
            ));
            top.push(stat_i32(
                "EN Output",
                pf(cur_id, c, "en_output"),
                pf(sel_id, c, "en_output"),
                true,
            ));
            top.push(stat_i32(
                "OW EN Output",
                pf(cur_id, c, "ow_en_output"),
                pf(sel_id, c, "ow_en_output"),
                true,
            ));
        }
        6 => {
            // BOOSTER
            top.push(stat_i32(
                "Boost Output",
                pf(cur_id, c, "power"),
                pf(sel_id, c, "power"),
                true,
            ));
            top.push(stat_i32(
                "HB Output",
                pf(cur_id, c, "hi_output"),
                pf(sel_id, c, "hi_output"),
                true,
            ));
            top.push(stat_i32(
                "HB EN Drain",
                pf(cur_id, c, "hi_en"),
                pf(sel_id, c, "hi_en"),
                false,
            ));
            top.push(stat_f32(
                "Burn Eff.",
                pf(cur_id, c, "std_burn_eff"),
                pf(sel_id, c, "std_burn_eff"),
                true,
            ));
            if detail {
                top.push(stat_f32(
                    "HB Burn Eff.",
                    pf(cur_id, c, "hi_burn_eff"),
                    pf(sel_id, c, "hi_burn_eff"),
                    true,
                ));
                top.push(stat_i32(
                    "Charge Frames",
                    pf(cur_id, c, "qb_charge_max_frames"),
                    pf(sel_id, c, "qb_charge_max_frames"),
                    false,
                ));
            }
        }
        10 | 11 | 12 => {
            // ARM UNITS / BAY UNITS / OVERED WEAPONS / SHOULDER UNITS
            top.push(stat_i32(
                "Attack Power",
                pf(cur_id, c, "damage_power"),
                pf(sel_id, c, "damage_power"),
                true,
            ));
            top.push(stat_i32(
                "Impact Force",
                pf(cur_id, c, "damage_recoil"),
                pf(sel_id, c, "damage_recoil"),
                true,
            ));
            top.push(stat_i32(
                "Reload Time",
                pf(cur_id, c, "reload_time"),
                pf(sel_id, c, "reload_time"),
                false,
            ));
            top.push(stat_i32(
                "Ammo",
                pf(cur_id, c, "magazine"),
                pf(sel_id, c, "magazine"),
                true,
            ));
            if detail {
                top.push(stat_i32(
                    "Opt. Range",
                    pf(cur_id, c, "no_decay_range"),
                    pf(sel_id, c, "no_decay_range"),
                    true,
                ));
                top.push(stat_i32(
                    "Atten. Range",
                    pf(cur_id, c, "damp_range"),
                    pf(sel_id, c, "damp_range"),
                    true,
                ));
                top.push(stat_i32(
                    "Muzzle Vel.",
                    pf(cur_id, c, "init_speed"),
                    pf(sel_id, c, "init_speed"),
                    true,
                ));
                top.push(stat_i32(
                    "Firing EN",
                    pf(cur_id, c, "en_drain"),
                    pf(sel_id, c, "en_drain"),
                    false,
                ));
                if c == 10 {
                    top.push(stat_i32(
                        "Charge Time",
                        pf(cur_id, c, "charge_time"),
                        pf(sel_id, c, "charge_time"),
                        false,
                    ));
                }
                top.push(stat_i32(
                    "Burst Count",
                    pf(cur_id, c, "burst_count"),
                    pf(sel_id, c, "burst_count"),
                    true,
                ));
            }
        }
        13 => {
            // RECON
            top.push(stat_i32(
                "Recon Type",
                pf(cur_id, c, "recon_subcategory"),
                pf(sel_id, c, "recon_subcategory"),
                true,
            ));
        }
        _ => {}
    }

    // Weight & Energy Consumption always sit at the bottom (`@Param0` / `@Param1` in `@PartsNormal` and `@PartsDetail`).
    top.push(stat_i32(
        "Weight",
        pf(cur_id, c, "weight"),
        pf(sel_id, c, "weight"),
        false,
    ));
    top.push(stat_i32(
        "Energy Consumption",
        pf(cur_id, c, "en_consumption"),
        pf(sel_id, c, "en_consumption"),
        false,
    ));
    top
}

/// Builds the 11 AC-wide spec comparison rows for `@SpecDetail` (`@Param0..@Param10`).
pub fn ac_spec_detail_rows(cur: &AcSpecs, preview: &AcSpecs) -> Vec<StatRow> {
    vec![
        stat_i32("AP", cur.ap as f64, preview.ap as f64, true),
        stat_i32("KE Defense", cur.ke_def as f64, preview.ke_def as f64, true),
        stat_i32("CE Defense", cur.ce_def as f64, preview.ce_def as f64, true),
        stat_i32("TE Defense", cur.te_def as f64, preview.te_def as f64, true),
        stat_i32(
            "Total Weight",
            cur.total_weight as f64,
            preview.total_weight as f64,
            false,
        ),
        stat_i32(
            "Load Capacity",
            cur.leg_payload as f64,
            preview.leg_payload as f64,
            true,
        ),
        stat_i32(
            "EN Output",
            cur.en_output as f64,
            preview.en_output as f64,
            true,
        ),
        stat_i32(
            "EN Recovery",
            cur.en_recovery as f64,
            preview.en_recovery as f64,
            true,
        ),
        stat_i32(
            "Boost Speed",
            cur.boost_speed as f64,
            preview.boost_speed as f64,
            true,
        ),
        stat_i32(
            "HB Speed",
            cur.hb_speed as f64,
            preview.hb_speed as f64,
            true,
        ),
        stat_i32(
            "Glide Speed",
            cur.glide_speed as f64,
            preview.glide_speed as f64,
            true,
        ),
    ]
}

fn cmp_color(cmp: i8) -> Color {
    match cmp {
        1 => Color::srgb(0.32, 0.78, 1.0), // ACVD color index 1003 (blue = better)
        -1 => Color::srgb(1.0, 0.30, 0.28), // ACVD color index 1004 (red = worse)
        _ => Color::srgb(0.92, 0.94, 0.96), // ACVD color index 1000 (neutral)
    }
}

/// Spawns the authentic retail `assemble.drb.dcx` Assemble Parts UI inside `root`.
/// Returns `true` if the DRB UI was spawned (so `garage::sync_ui` skips the legacy text panel).
#[allow(clippy::too_many_arguments)]
pub fn spawn_assemble_ui(
    commands: &mut Commands,
    assets: &mut AssembleUiAssets,
    state: &GarageState,
    garage: &crate::Garage,
    slot: AssembleSlot,
    window_size: Vec2,
    images: &mut Assets<Image>,
    device: InputDevice,
) -> bool {
    assets.ensure_loaded(&garage.disc, images);

    let scale = (window_size.x / SCREEN_W)
        .min(window_size.y / SCREEN_H)
        .max(0.5);
    let origin = Vec2::new(
        ((window_size.x - SCREEN_W * scale) * 0.5).round(),
        ((window_size.y - SCREEN_H * scale) * 0.5).round(),
    );

    let parts = filtered_parts_for_slot(slot, state.assemble_subcat, state.assemble_dmg_filter);
    let sel_id = parts.get(state.cursor).copied().unwrap_or(0);
    let orig_design = state.saved_design.unwrap_or(garage.active_design);
    let cur_id = slot.get(&orig_design);
    let preview_specs = AcSpecs::calculate(&garage.active_design);
    let base_specs = AcSpecs::calculate(&orig_design);
    let cat = slot.category();
    let subcat_id = part_subcategory(cat, sel_id);
    let subcat_label = if sel_id == 0 {
        "EMPTY SLOT".to_string()
    } else {
        subcategory_name(assets.subcat_fmg.as_ref(), cat, subcat_id)
    };
    let sel_name = part_name(state.parts_fmg.as_ref(), cat, sel_id);
    let explain = part_description(assets.explain_fmg.as_ref(), cat, sel_id);
    let (help_line1, help_line2) = wrap_two_lines_px(&assets.kit, &explain, 1085.0);

    // Full-screen container anchored at `origin` in 1280×720 virtual space.
    let screen_root = commands
        .spawn((
            GarageUi,
            Node {
                position_type: PositionType::Absolute,
                left: px(origin.x),
                top: px(origin.y),
                width: px(SCREEN_W * scale),
                height: px(SCREEN_H * scale),
                ..default()
            },
        ))
        .id();

    let px_rect = |x0: f32, y0: f32, x1: f32, y1: f32| Node {
        position_type: PositionType::Absolute,
        left: px(x0 * scale),
        top: px(y0 * scale),
        width: px((x1 - x0) * scale),
        height: px((y1 - y0) * scale),
        ..default()
    };

    // 1. Real `@ModelBackImage` DRB dialog (authentic left/right corner-reticle frames +
    //    9-slice translucent `汎用ベース1` panels spanning `[173,220,686,567]` and `[705,109,1213,567]`).
    if let Some(layout) = assets.layout.as_ref() {
        if let Some(idx) = layout
            .drb
            .dialogs
            .iter()
            .position(|d| d.name == "@ModelBackImage")
        {
            let dlg = menu::spawn(commands, layout, idx, Vec2::ZERO, scale);
            commands
                .entity(dlg)
                .remove::<MenuRoot>()
                .insert(ChildOf(screen_root));
        }
    }

    // 2. Normal View vs Detail View (`Tab` toggles `state.assemble_detail_view`).
    if !state.assemble_detail_view {
        // 2a-right. Live 3D full-AC preview inside `@AcLayout` (`ASSEMBLY VIEW`, `[705,109,1213,567]`),
        //           spawned before `@PartsNormal` so the Load/Energy bars draw cleanly on top.
        if let Some(ref ac_img) = assets.ac_preview_image {
            commands.spawn((
                px_rect(705.0, 109.0, 1213.0, 567.0),
                ImageNode::new(ac_img.clone()),
                ChildOf(screen_root),
            ));
        }

        // 2a-left. Live 3D Part/Weapon Preview in the upper clear area of `@PartsLayout`
        //          (`[178,231,676,412]`), sitting seamlessly inside `base_window_L`'s DRB frame.
        if let Some(ref preview_img) = assets.preview_image {
            if sel_id > 0 {
                commands.spawn((
                    px_rect(178.0, 231.0, 676.0, 412.0),
                    ImageNode::new(preview_img.clone()),
                    ChildOf(screen_root),
                ));
            }
        }

        // 3b. Real `@PartsNormal` DRB dialog from `lang/en/menu/assemble.drb.dcx`!
        // We spawn it as a child of `screen_root`, then immediately patch its runtime `MenuText`
        // values and dynamic gauges via `AssembleDrbSync` (or a one-shot marker)!
        if let Some(layout) = assets.layout.as_ref() {
            if let Some(idx) = layout
                .drb
                .dialogs
                .iter()
                .position(|d| d.name == "@PartsNormal")
            {
                let dlg = menu::spawn(commands, layout, idx, Vec2::ZERO, scale);
                commands.entity(dlg).remove::<MenuRoot>().insert((
                    ChildOf(screen_root),
                    AssembleNormalDialog {
                        category_name: slot.label().to_string(),
                        sub_category: subcat_label.clone(),
                        parts_name: sel_name.clone(),
                        stats: part_stat_rows(slot, cur_id, sel_id, false),
                        is_empty: sel_id == 0,
                        load_ratio: if preview_specs.leg_payload > 0.0 {
                            preview_specs.carried_load / preview_specs.leg_payload
                        } else {
                            0.0
                        },
                        en_ratio: if preview_specs.en_output > 0.0 {
                            preview_specs.en_drain / preview_specs.en_output
                        } else {
                            0.0
                        },
                        overweight: preview_specs.overweight,
                        en_shortage: preview_specs.en_shortage,
                        help1: help_line1.clone(),
                        help2: help_line2.clone(),
                        scale,
                    },
                ));
            }
        }

        // Damage-type badge pill on the right of `@PartsFrameName` when inspecting a weapon.
        if let Some((_, dtag, dcol)) = part_damage_badge(cat, sel_id) {
            let wrap = commands
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: px(460.0 * scale),
                        top: px(236.0 * scale),
                        ..default()
                    },
                    ChildOf(screen_root),
                ))
                .id();
            assets
                .kit
                .spawn_badge(commands, wrap, &format!("{dtag} DMG"), dcol, true, scale);
        }
    } else {
        // Detail View (`@PartsDetail` + `@SpecDetail` from `lang/en/menu/assemble.drb.dcx`).
        if let Some(layout) = assets.layout.as_ref() {
            if let Some(idx) = layout
                .drb
                .dialogs
                .iter()
                .position(|d| d.name == "@PartsDetail")
            {
                let dlg = menu::spawn(commands, layout, idx, Vec2::ZERO, scale);
                commands.entity(dlg).remove::<MenuRoot>().insert((
                    ChildOf(screen_root),
                    AssembleDetailDialog {
                        parts_name: sel_name.clone(),
                        sub_category: subcat_label.clone(),
                        stats: part_stat_rows(slot, cur_id, sel_id, true),
                    },
                ));
            }
            if let Some(idx) = layout
                .drb
                .dialogs
                .iter()
                .position(|d| d.name == "@SpecDetail")
            {
                let dlg = menu::spawn(commands, layout, idx, Vec2::ZERO, scale);
                commands.entity(dlg).remove::<MenuRoot>().insert((
                    ChildOf(screen_root),
                    AssembleSpecDetailDialog {
                        stats: ac_spec_detail_rows(&base_specs, &preview_specs),
                    },
                ));
            }
        }
    }

    // 4. Full-width ACVD Top Bar (`y = 0..42`) + retail stepped trace (`LinePatrs_UP`).
    let top_wrap = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(0.0),
                top: px(0.0),
                width: px(SCREEN_W * scale),
                flex_direction: FlexDirection::Column,
                ..default()
            },
            ChildOf(screen_root),
        ))
        .id();
    let top_bar = assets.kit.spawn_panel(
        commands,
        Some(top_wrap),
        PanelStyle::Bar,
        Node {
            width: Val::Percent(100.0),
            padding: UiRect::axes(px((22.0 * scale).round()), px((7.0 * scale).round())),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::SpaceBetween,
            ..default()
        },
        scale,
    );
    commands
        .entity(top_bar)
        .insert(BackgroundColor(Color::srgba(0.0, 0.03, 0.04, 0.86)));
    let title_left = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: px((8.0 * scale).round()),
                ..default()
            },
            ChildOf(top_bar),
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
        BackgroundColor(palette::TEXT_ALERT),
        ChildOf(title_left),
    ));
    assets.kit.spawn_text(
        commands,
        title_left,
        FontRole::Header,
        &format!("ASSEMBLE  //  {}", slot.label()),
        scale,
        palette::TEXT_ALERT,
    );

    let top_actions = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: px((8.0 * scale).round()),
                ..default()
            },
            ChildOf(top_bar),
        ))
        .id();
    let detail_wrap = commands
        .spawn((
            Node {
                width: px((164.0 * scale).round()),
                ..default()
            },
            ChildOf(top_actions),
        ))
        .id();
    let detail_btn = assets.kit.spawn_button(
        commands,
        detail_wrap,
        if state.assemble_detail_view {
            if crate::hints::use_icons(device) {
                "[X] Normal View"
            } else {
                "[Tab] Normal View"
            }
        } else if crate::hints::use_icons(device) {
            "[X] Detail Specs"
        } else {
            "[Tab] Detail Specs"
        },
        None,
        if state.assemble_detail_view {
            ButtonState::Active
        } else {
            ButtonState::Normal
        },
        scale,
    );
    commands
        .entity(detail_btn)
        .insert((Button, UiAction::AssembleToggleDetail));

    let back_wrap = commands
        .spawn((
            Node {
                width: px((108.0 * scale).round()),
                ..default()
            },
            ChildOf(top_actions),
        ))
        .id();
    let back_btn = assets.kit.spawn_button(
        commands,
        back_wrap,
        if crate::hints::use_icons(device) {
            "[B] Back"
        } else {
            "[Esc] Back"
        },
        None,
        ButtonState::Normal,
        scale,
    );
    commands.entity(back_btn).insert((Button, UiAction::Back));

    assets
        .kit
        .spawn_trace(commands, top_wrap, TraceStyle::Stepped, scale);

    // 5. Subcategory / Weapon-Type & Damage-Type Filter Tabs (`y = 62..104`, right above `@CategoryName`).
    let subcats = subcategories_for_slot(slot, assets.subcat_fmg.as_ref());
    let filter_row = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(84.0 * scale),
                top: px(60.0 * scale),
                width: px(1130.0 * scale),
                height: px(42.0 * scale),
                flex_direction: FlexDirection::Row,
                flex_wrap: FlexWrap::Wrap,
                align_items: AlignItems::Center,
                column_gap: px((5.0 * scale).round()),
                row_gap: px((4.0 * scale).round()),
                ..default()
            },
            ChildOf(screen_root),
        ))
        .id();

    // Damage type filter chips for weapons (`ALL DMG | KE | CE | TE`).
    if slot_has_damage_filter(slot) {
        let dmg_opts: [(Option<u8>, &str, Color); 4] = [
            (None, "ALL DMG", palette::ACCENT_TEAL),
            (Some(0), "KE", Color::srgb(0.95, 0.72, 0.24)),
            (Some(6), "CE", Color::srgb(0.32, 0.88, 0.45)),
            (Some(1), "TE", Color::srgb(0.32, 0.76, 0.98)),
        ];
        for (val, lbl, col) in dmg_opts {
            let active = state.assemble_dmg_filter == val;
            let chip = assets
                .kit
                .spawn_badge(commands, filter_row, lbl, col, active, scale);
            commands
                .entity(chip)
                .insert((Button, UiAction::AssembleDmgFilter(val)));
        }
    }

    // Subcategory / weapon-type tabs (from `partssubcategory_en.fmg`).
    if !subcats.is_empty() {
        let all_active = state.assemble_subcat.is_none();
        let all_chip = assets.kit.spawn_badge(
            commands,
            filter_row,
            "ALL TYPES",
            palette::ACCENT_TEAL,
            all_active,
            scale,
        );
        commands
            .entity(all_chip)
            .insert((Button, UiAction::AssembleSubcat(None)));

        // Show up to 11 subcategory chips (cycle any beyond with Q/E!).
        let max_chips = if slot_has_damage_filter(slot) { 10 } else { 12 };
        let active_pos = state
            .assemble_subcat
            .and_then(|s| subcats.iter().position(|(id, _)| *id == s))
            .unwrap_or(0);
        let start_chip = active_pos
            .saturating_sub(max_chips / 2)
            .min(subcats.len().saturating_sub(max_chips));
        let end_chip = (start_chip + max_chips).min(subcats.len());
        for &(sid, ref sname) in &subcats[start_chip..end_chip] {
            let active = state.assemble_subcat == Some(sid);
            let chip = assets.kit.spawn_badge(
                commands,
                filter_row,
                &short_subcat_label(sname),
                if active {
                    palette::ACCENT_TEAL
                } else {
                    palette::SQUARE_MARK
                },
                active,
                scale,
            );
            commands
                .entity(chip)
                .insert((Button, UiAction::AssembleSubcat(Some(sid))));
        }
    }

    // 6. Top Horizontal Slot Strip (`@PartsCategoryList` `[86, 148, 686, 202]`, 7 × `84×54` tabs).
    //    Shows the real disc thumbnail of each slot's currently equipped part!
    let slot_idx = AssembleSlot::ALL
        .iter()
        .position(|&s| s == slot)
        .unwrap_or(0);
    let cat_start = state
        .assemble_cat_scroll
        .min(AssembleSlot::ALL.len().saturating_sub(ASSEMBLE_CAT_WINDOW));
    let cat_end = (cat_start + ASSEMBLE_CAT_WINDOW).min(AssembleSlot::ALL.len());
    for (vis_i, &tab_slot) in AssembleSlot::ALL[cat_start..cat_end].iter().enumerate() {
        let x0 = 86.0 + vis_i as f32 * 86.0;
        let is_cur = cat_start + vis_i == slot_idx;
        let eq_id = tab_slot.get(&garage.active_design);
        let thumb = assets.thumbnail(&garage.disc, images, tab_slot.category(), eq_id);
        let tab = commands
            .spawn((
                Button,
                UiAction::AssembleSelectSlot(tab_slot),
                Node {
                    position_type: PositionType::Absolute,
                    left: px((x0 * scale).round()),
                    top: px((148.0 * scale).round()),
                    width: px((84.0 * scale).round()),
                    height: px((54.0 * scale).round()),
                    overflow: Overflow::clip(),
                    ..default()
                },
                BackgroundColor(if is_cur {
                    palette::ACCENT_TEAL_FILL_TOP
                } else {
                    palette::BG_BAR
                }),
                ChildOf(screen_root),
            ))
            .id();
        if let Some(img) = thumb {
            commands.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: px(0.0),
                    top: px(0.0),
                    width: px((84.0 * scale).round()),
                    height: px((54.0 * scale).round()),
                    ..default()
                },
                ImageNode {
                    image: img,
                    // Retail `assemble.drb.dcx` `@PartsCategoryItem/@CategoryPic` uses `uv=[0,0,84,54]`
                    // inside each 128×64 power-of-two thumbnail TPF (remaining pixels are PoT padding).
                    rect: Some(Rect::new(0.0, 0.0, 84.0, 54.0)),
                    color: if is_cur {
                        Color::srgb(0.82, 1.0, 0.92)
                    } else {
                        Color::srgb(0.86, 0.89, 0.90)
                    },
                    ..default()
                },
                ChildOf(tab),
            ));
        }
        // Non-shifting border overlay so the 84×54 thumbnail aspect ratio stays exact.
        let tab_bw = (if is_cur { 2.0 } else { 1.0 } * scale).round().max(1.0);
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(0.0),
                top: px(0.0),
                width: px((84.0 * scale).round()),
                height: px((54.0 * scale).round()),
                border: UiRect::all(px(tab_bw)),
                ..default()
            },
            BorderColor::all(if is_cur {
                palette::ACCENT_TEAL
            } else {
                palette::BORDER_BUTTON
            }),
            ChildOf(tab),
        ));
        // Compact disc-font slot tag at top-left of the tab.
        let tag_box = commands
            .spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: px((3.0 * scale).round()),
                    top: px((2.0 * scale).round()),
                    padding: UiRect::axes(px((2.0 * scale).round()), px(0.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.0, 0.03, 0.04, 0.72)),
                ChildOf(tab),
            ))
            .id();
        assets.kit.spawn_text(
            commands,
            tag_box,
            FontRole::Small,
            short_slot_tag(tab_slot),
            scale * 0.82,
            if is_cur {
                palette::ACCENT_TEAL
            } else {
                palette::TEXT_PRIMARY
            },
        );
    }

    // 7. Left Vertical Part Thumbnail Strip (`@PartsList` `[86, 220, 170, 562]`, 6 × `84×54`).
    //    Displays the real disc picture (`image/partsthumbnail/<dir>/<prefix><model_id:04>.tpf.dcx`)
    //    for every part in the filtered list!
    let list_start = state
        .scroll
        .min(parts.len().saturating_sub(ASSEMBLE_THUMB_WINDOW));
    let list_end = (list_start + ASSEMBLE_THUMB_WINDOW).min(parts.len());
    for (vis_i, idx) in (list_start..list_end).enumerate() {
        let pid = parts[idx];
        let y0 = 220.0 + vis_i as f32 * 57.0;
        let is_sel = state.cursor == idx;
        let is_eq = pid == cur_id;
        let thumb = assets.thumbnail(&garage.disc, images, cat, pid);

        let item = commands
            .spawn((
                Button,
                UiRow(idx),
                Node {
                    position_type: PositionType::Absolute,
                    left: px((86.0 * scale).round()),
                    top: px((y0 * scale).round()),
                    width: px((84.0 * scale).round()),
                    height: px((54.0 * scale).round()),
                    overflow: Overflow::clip(),
                    ..default()
                },
                BackgroundColor(if is_sel {
                    palette::ACCENT_AMBER_FILL_TOP
                } else {
                    palette::BG_BAR
                }),
                ChildOf(screen_root),
            ))
            .id();

        if let Some(img) = thumb {
            commands.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: px(0.0),
                    top: px(0.0),
                    width: px((84.0 * scale).round()),
                    height: px((54.0 * scale).round()),
                    ..default()
                },
                ImageNode {
                    image: img,
                    // Retail `assemble.drb.dcx` `@PartsItem/@PARTS_PIC` uses `uv=[0,0,84,54]`
                    // inside each 128×64 power-of-two thumbnail TPF.
                    rect: Some(Rect::new(0.0, 0.0, 84.0, 54.0)),
                    color: if is_sel {
                        Color::srgb(0.96, 1.0, 0.94)
                    } else {
                        Color::WHITE
                    },
                    ..default()
                },
                ChildOf(item),
            ));
        }

        // Non-shifting border overlay (`パーツ選択カーソル` amber highlight on selected part).
        let item_bw = (if is_sel { 2.0 } else { 1.0 } * scale).round().max(1.0);
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(0.0),
                top: px(0.0),
                width: px((84.0 * scale).round()),
                height: px((54.0 * scale).round()),
                border: UiRect::all(px(item_bw)),
                ..default()
            },
            BorderColor::all(if is_sel {
                palette::ACCENT_AMBER
            } else {
                palette::BORDER_BUTTON
            }),
            ChildOf(item),
        ));

        // Equipped badge (`E`, matching `@LogoEquip`).
        if is_eq {
            let eq_box = commands
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: px((3.0 * scale).round()),
                        top: px((2.0 * scale).round()),
                        padding: UiRect::axes(px((3.0 * scale).round()), px((1.0 * scale).round())),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.0, 0.06, 0.08, 0.88)),
                    ChildOf(item),
                ))
                .id();
            assets.kit.spawn_text(
                commands,
                eq_box,
                FontRole::Small,
                "E",
                scale * 0.85,
                palette::ACCENT_TEAL,
            );
        }

        // Damage-type pip in bottom-right corner for weapons.
        if let Some((_, dtag, dcol)) = part_damage_badge(cat, pid) {
            let dmg_box = commands
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        right: px((3.0 * scale).round()),
                        bottom: px((2.0 * scale).round()),
                        padding: UiRect::axes(px((3.0 * scale).round()), px((1.0 * scale).round())),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.0, 0.04, 0.06, 0.88)),
                    ChildOf(item),
                ))
                .id();
            assets
                .kit
                .spawn_text(commands, dmg_box, FontRole::Small, dtag, scale * 0.8, dcol);
        }
    }

    // 7b. Clickable mouse hitboxes over the 4 DRB navigation arrows (`<`, `>`, `^`, `v`).
    for (x0, y0, x1, y1, action) in [
        (58.0, 148.0, 86.0, 202.0, UiAction::AssembleCatStep(-1)),
        (686.0, 148.0, 714.0, 202.0, UiAction::AssembleCatStep(1)),
        (86.0, 202.0, 170.0, 220.0, UiAction::AssemblePartStep(-1)),
        (86.0, 562.0, 170.0, 584.0, UiAction::AssemblePartStep(1)),
    ] {
        commands.spawn((
            Button,
            action,
            px_rect(x0, y0, x1, y1),
            BackgroundColor(Color::NONE),
            ChildOf(screen_root),
        ));
    }

    // 8. Full-width ACVD Footer Controls Bar (`y = 674..720`).
    let footer = assets.kit.spawn_panel(
        commands,
        Some(screen_root),
        PanelStyle::Bar,
        Node {
            position_type: PositionType::Absolute,
            left: px(0.0),
            top: px((676.0 * scale).round()),
            width: px((SCREEN_W * scale).round()),
            height: px((44.0 * scale).round()),
            padding: UiRect::axes(px((22.0 * scale).round()), px((6.0 * scale).round())),
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::SpaceBetween,
            column_gap: px((12.0 * scale).round()),
            ..default()
        },
        scale,
    );
    commands
        .entity(footer)
        .insert(BackgroundColor(Color::srgba(0.0, 0.03, 0.04, 0.86)));
    let slot_prompt = match slot {
        AssembleSlot::Arms => "an arms part",
        AssembleSlot::ArmUnitR => "a right arm unit",
        AssembleSlot::ArmUnitL => "a left arm unit",
        AssembleSlot::HangerR => "a right bay unit",
        AssembleSlot::HangerL => "a left bay unit",
        AssembleSlot::Shoulder => "a shoulder unit",
        AssembleSlot::Fcs => "an FCS",
        AssembleSlot::OveredWeapon => "an overed weapon",
        AssembleSlot::Head => "a head part",
        AssembleSlot::Core => "a core part",
        AssembleSlot::Legs => "a legs part",
        AssembleSlot::Generator => "a generator",
        AssembleSlot::Booster => "a booster",
        AssembleSlot::Recon => "a recon unit",
    };
    let footer_left = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: px((16.0 * scale).round()),
                ..default()
            },
            ChildOf(footer),
        ))
        .id();
    assets.kit.spawn_text(
        commands,
        footer_left,
        FontRole::Body,
        &format!("Select {slot_prompt}."),
        scale,
        palette::TEXT_ALERT,
    );
    let hints = crate::hints::parts(crate::hints::ASSEMBLE);
    let shown = crate::hints::use_icons(device)
        && assets
            .kit
            .spawn_hints(commands, footer_left, &hints, scale, palette::TEXT_PRIMARY)
            .is_some();
    if !shown {
        assets.kit.spawn_text(
            commands,
            footer_left,
            FontRole::Small,
            "W/S: Part   A/D: Slot   Q/E: Filter   Tab: Specs   Enter: Equip   Right-Drag: Orbit",
            scale,
            palette::TEXT_PRIMARY,
        );
    }
    assets.kit.spawn_badge(
        commands,
        footer,
        &format!(
            "{} / {}",
            if parts.is_empty() {
                0
            } else {
                state.cursor + 1
            },
            parts.len()
        ),
        palette::SQUARE_MARK,
        false,
        scale,
    );

    // Transient banner (e.g. "EQUIPPED ...", "OVERWEIGHT!").
    if let Some((ref msg, col, _)) = state.banner {
        let banner = assets.kit.spawn_panel(
            commands,
            Some(screen_root),
            PanelStyle::Framed,
            Node {
                position_type: PositionType::Absolute,
                left: px((710.0 * scale).round()),
                top: px((62.0 * scale).round()),
                width: px((498.0 * scale).round()),
                padding: UiRect::axes(px((12.0 * scale).round()), px((6.0 * scale).round())),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            scale,
        );
        assets
            .kit
            .spawn_text(commands, banner, FontRole::Body, msg, scale, col);
    }

    true
}

fn short_slot_tag(slot: AssembleSlot) -> &'static str {
    match slot {
        AssembleSlot::Head => "HEAD",
        AssembleSlot::Core => "CORE",
        AssembleSlot::Arms => "ARMS",
        AssembleSlot::Legs => "LEGS",
        AssembleSlot::Fcs => "FCS",
        AssembleSlot::Generator => "GEN",
        AssembleSlot::Booster => "BST",
        AssembleSlot::Recon => "RECON",
        AssembleSlot::ArmUnitR => "R-ARM",
        AssembleSlot::ArmUnitL => "L-ARM",
        AssembleSlot::Shoulder => "SHLD",
        AssembleSlot::HangerR => "R-BAY",
        AssembleSlot::HangerL => "L-BAY",
        AssembleSlot::OveredWeapon => "OW",
    }
}

/// Word-wraps `s` into at most two lines using the real disc CCM body font metrics so
/// `@2Line_Helpline` (`[86, 4, 1193, 44]` = `1107px` wide) never clips mid-word.
fn wrap_two_lines_px(kit: &AcvdUiKit, s: &str, max_px: f32) -> (String, String) {
    let words: Vec<&str> = s.split_whitespace().collect();
    if words.is_empty() {
        return (String::new(), String::new());
    }
    let mut line1 = String::new();
    let mut idx = 0;
    while idx < words.len() {
        let cand = if line1.is_empty() {
            words[idx].to_string()
        } else {
            format!("{line1} {}", words[idx])
        };
        if !line1.is_empty() && kit.measure_text(FontRole::Body, &cand, 1.0).x > max_px {
            break;
        }
        line1 = cand;
        idx += 1;
    }
    let mut line2 = String::new();
    while idx < words.len() {
        let cand = if line2.is_empty() {
            words[idx].to_string()
        } else {
            format!("{line2} {}", words[idx])
        };
        let suffix = if idx + 1 < words.len() { "..." } else { "" };
        if !line2.is_empty()
            && kit
                .measure_text(FontRole::Body, &format!("{cand}{suffix}"), 1.0)
                .x
                > max_px
        {
            line2.push_str("...");
            break;
        }
        line2 = cand;
        idx += 1;
    }
    (line1, line2)
}

/// Maps an [`AssembleSlot`] to the `AC_ASSEMBLY_DESIGN_ST_SLOTS` column name attached to its
/// 3D AC submeshes (`None` for internal non-drawn parts: FCS, Generator, Recon).
pub fn slot_column(slot: AssembleSlot) -> Option<&'static str> {
    match slot {
        AssembleSlot::Head => Some("head"),
        AssembleSlot::Core => Some("core"),
        AssembleSlot::Arms => Some("arms"),
        AssembleSlot::Legs => Some("legs"),
        AssembleSlot::Booster => Some("booster"),
        AssembleSlot::ArmUnitR => Some("armwep_r"),
        AssembleSlot::ArmUnitL => Some("armwep_l"),
        AssembleSlot::Shoulder => Some("shoulder"),
        AssembleSlot::HangerR => Some("hanger_r"),
        AssembleSlot::HangerL => Some("hanger_l"),
        AssembleSlot::OveredWeapon => Some("back_r"),
        AssembleSlot::Fcs | AssembleSlot::Generator | AssembleSlot::Recon => None,
    }
}

/// Stores the original opaque material of a main-AC submesh while [`sync_part_hologram`] renders
/// it as a translucent amber-orange Fresnel hologram during Garage Assemble part selection.
#[derive(Component)]
pub enum HoloSavedMat {
    Painted {
        original: Handle<paint::AcPaintMaterial>,
        holo: Handle<paint::AcPaintMaterial>,
    },
    Std {
        original: Handle<StandardMaterial>,
    },
}

/// Applies the ACVD translucent amber-orange hologram highlight to the currently selected
/// [`AssembleSlot`]'s submeshes on the right-side `ASSEMBLY VIEW` preview AC ([`AcPreviewMesh`])
/// while in `GarageScreen::AssembleParts`, leaving the main garage mech untouched and opaque.
#[allow(clippy::too_many_arguments)]
pub fn sync_part_hologram(
    mut commands: Commands,
    mode: Res<Mode>,
    state: Res<GarageState>,
    time: Res<Time>,
    mut ac_mats: ResMut<Assets<paint::AcPaintMaterial>>,
    mut std_mats: ResMut<Assets<StandardMaterial>>,
    mut holo_std: Local<Option<Handle<StandardMaterial>>>,
    meshes: Query<
        (
            Entity,
            &crate::AcPartColumn,
            Option<&MeshMaterial3d<paint::AcPaintMaterial>>,
            Option<&MeshMaterial3d<StandardMaterial>>,
            Option<&HoloSavedMat>,
        ),
        With<AcPreviewMesh>,
    >,
) {
    let target_col = if *mode == Mode::Garage {
        match state.screen {
            GarageScreen::AssembleParts { slot } => slot_column(slot),
            _ => None,
        }
    } else {
        None
    };

    let pulse = 0.78 + 0.12 * (time.elapsed_secs() * 3.2).sin();
    let holo_param = Vec4::new(1.0, 0.24, 0.02, pulse);

    for (entity, col, painted_mat, std_mat, saved) in &meshes {
        let should_holo = target_col == Some(col.0);
        match (should_holo, saved) {
            (true, Some(HoloSavedMat::Painted { holo, .. })) => {
                if let Some(mut m) = ac_mats.get_mut(holo) {
                    m.extension.params.highlight = holo_param;
                }
            }
            (true, Some(HoloSavedMat::Std { .. })) => {}
            (true, None) => {
                if let Some(pm) = painted_mat {
                    if let Some(mut cloned) = ac_mats.get(&pm.0).cloned() {
                        cloned.base.alpha_mode = AlphaMode::Blend;
                        cloned.extension.params.highlight = holo_param;
                        let holo = ac_mats.add(cloned);
                        commands.entity(entity).insert((
                            MeshMaterial3d(holo.clone()),
                            HoloSavedMat::Painted {
                                original: pm.0.clone(),
                                holo,
                            },
                        ));
                    }
                } else if let Some(sm) = std_mat {
                    let h = holo_std
                        .get_or_insert_with(|| {
                            std_mats.add(StandardMaterial {
                                base_color: Color::srgba(1.0, 0.50, 0.10, 0.44),
                                emissive: LinearRgba::new(2.4, 1.0, 0.16, 1.0),
                                alpha_mode: AlphaMode::Blend,
                                cull_mode: None,
                                unlit: false,
                                ..default()
                            })
                        })
                        .clone();
                    commands.entity(entity).insert((
                        MeshMaterial3d(h),
                        HoloSavedMat::Std {
                            original: sm.0.clone(),
                        },
                    ));
                }
            }
            (false, Some(HoloSavedMat::Painted { original, .. })) => {
                commands
                    .entity(entity)
                    .insert(MeshMaterial3d(original.clone()))
                    .remove::<HoloSavedMat>();
            }
            (false, Some(HoloSavedMat::Std { original })) => {
                commands
                    .entity(entity)
                    .insert(MeshMaterial3d(original.clone()))
                    .remove::<HoloSavedMat>();
            }
            (false, None) => {}
        }
    }
}

/// Marker attached to a spawned `@PartsNormal` DRB dialog root so [`populate_drb_dialogs`] fills
/// its runtime `MenuText` fields, gauges, and conditional badges on the same frame.
#[derive(Component)]
pub struct AssembleNormalDialog {
    pub category_name: String,
    pub sub_category: String,
    pub parts_name: String,
    pub stats: Vec<StatRow>,
    pub is_empty: bool,
    pub load_ratio: f32,
    pub en_ratio: f32,
    pub overweight: bool,
    pub en_shortage: bool,
    pub help1: String,
    pub help2: String,
    pub scale: f32,
}

#[derive(Component)]
pub struct AssembleDetailDialog {
    pub parts_name: String,
    pub sub_category: String,
    pub stats: Vec<StatRow>,
}

#[derive(Component)]
pub struct AssembleSpecDetailDialog {
    pub stats: Vec<StatRow>,
}

/// Populates the runtime `MenuText`, `Visibility`, and gauge widths inside freshly spawned
/// `@PartsNormal`, `@PartsDetail`, and `@SpecDetail` DRB dialog hierarchies.
pub fn populate_drb_dialogs(
    mut commands: Commands,
    normal_q: Query<(Entity, &AssembleNormalDialog)>,
    detail_q: Query<(Entity, &AssembleDetailDialog)>,
    spec_q: Query<(Entity, &AssembleSpecDetailDialog)>,
    children_q: Query<&Children>,
    names_q: Query<&MenuObject>,
    mut texts_q: Query<&mut MenuText>,
    mut vis_q: Query<&mut Visibility>,
    mut nodes_q: Query<&mut Node>,
) {
    for (root, data) in &normal_q {
        let Ok(top_children) = children_q.get(root) else {
            continue;
        };
        // Normal mode maps:
        // `@Param2..@Param5` = stats[0..n-2] (top 4 category stats)
        // `@Param0`, `@Param1` = stats[n-2], stats[n-1] (Weight, Energy Consumption)
        let n = data.stats.len();
        let top_stats = if n >= 2 {
            &data.stats[..n - 2]
        } else {
            &[][..]
        };
        let bottom_stats = if n >= 2 {
            &data.stats[n - 2..]
        } else {
            &data.stats[..]
        };

        for child in top_children.iter() {
            let Ok(obj) = names_q.get(child) else {
                continue;
            };
            match obj.0.as_str() {
                "@CategoryName" => {
                    if let Ok(mut mt) = texts_q.get_mut(child) {
                        mt.text = data.category_name.clone();
                    }
                }
                "@PartsFrameName" => {
                    if let Ok(sub) = children_q.get(child) {
                        for sc in sub.iter() {
                            if let Ok(so) = names_q.get(sc) {
                                match so.0.as_str() {
                                    "@SubCategory" => {
                                        if let Ok(mut mt) = texts_q.get_mut(sc) {
                                            mt.text = data.sub_category.clone();
                                        }
                                    }
                                    "@PartsName" => {
                                        if let Ok(mut mt) = texts_q.get_mut(sc) {
                                            mt.text = data.parts_name.clone();
                                        }
                                    }
                                    "@Signature" | "@Signature_plate" => {
                                        if let Ok(mut v) = vis_q.get_mut(sc) {
                                            *v = Visibility::Hidden;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                "@PartsTypeIcon" => {
                    // Hide overlapping tuning/DLC badges so only clean status shows.
                    if let Ok(mut v) = vis_q.get_mut(child) {
                        *v = Visibility::Hidden;
                    }
                }
                "@NoData_ObjDialog" => {
                    if let Ok(mut v) = vis_q.get_mut(child) {
                        *v = if data.is_empty {
                            Visibility::Inherited
                        } else {
                            Visibility::Hidden
                        };
                    }
                }
                "@WeightError" | "@WeightAlert" => {
                    if let Ok(mut v) = vis_q.get_mut(child) {
                        *v = if data.overweight {
                            Visibility::Inherited
                        } else {
                            Visibility::Hidden
                        };
                    }
                }
                "@EnergyError" | "@EnergyAlert" => {
                    if let Ok(mut v) = vis_q.get_mut(child) {
                        *v = if data.en_shortage {
                            Visibility::Inherited
                        } else {
                            Visibility::Hidden
                        };
                    }
                }
                "@WeightBar" => {
                    // `@WeightFrame` spans x=822..1198 (width 376px), marker at ~70% (`load == 1.0`).
                    if let Ok(mut node) = nodes_q.get_mut(child) {
                        let w = (data.load_ratio * 260.0).clamp(2.0, 374.0) * data.scale;
                        node.width = px(w);
                    }
                }
                "@WeightRecommend" => {
                    if let Ok(mut node) = nodes_q.get_mut(child) {
                        node.left = px((823.0 + 260.0) * data.scale);
                    }
                }
                "@EnergyBar" => {
                    if let Ok(mut node) = nodes_q.get_mut(child) {
                        let w = (data.en_ratio * 260.0).clamp(2.0, 374.0) * data.scale;
                        node.width = px(w);
                    }
                }
                "@EnergyRecommend" => {
                    if let Ok(mut node) = nodes_q.get_mut(child) {
                        node.left = px((823.0 + 260.0) * data.scale);
                    }
                }
                "@2Line_Helpline" => {
                    if let Ok(sub) = children_q.get(child) {
                        for sc in sub.iter() {
                            if let Ok(so) = names_q.get(sc) {
                                if so.0 == "@Text_1" {
                                    if let Ok(mut mt) = texts_q.get_mut(sc) {
                                        mt.text = data.help1.clone();
                                    }
                                } else if so.0 == "@Text_2" {
                                    if let Ok(mut mt) = texts_q.get_mut(sc) {
                                        mt.text = data.help2.clone();
                                    }
                                }
                            }
                        }
                    }
                }
                name if name.starts_with("@Param") => {
                    let row = match name {
                        "@Param2" => top_stats.first(),
                        "@Param3" => top_stats.get(1),
                        "@Param4" => top_stats.get(2),
                        "@Param5" => top_stats.get(3),
                        "@Param0" => bottom_stats.first(),
                        "@Param1" => bottom_stats.get(1),
                        _ => None,
                    };
                    fill_param_dialog(child, row, &children_q, &names_q, &mut texts_q);
                }
                _ => {}
            }
        }
        commands.entity(root).remove::<AssembleNormalDialog>();
    }

    for (root, data) in &detail_q {
        let Ok(top_children) = children_q.get(root) else {
            continue;
        };
        let n = data.stats.len();
        let top_stats = if n >= 2 {
            &data.stats[..n - 2]
        } else {
            &[][..]
        };
        let bottom_stats = if n >= 2 {
            &data.stats[n - 2..]
        } else {
            &data.stats[..]
        };
        for child in top_children.iter() {
            let Ok(obj) = names_q.get(child) else {
                continue;
            };
            match obj.0.as_str() {
                "@PartsName" => {
                    if let Ok(mut mt) = texts_q.get_mut(child) {
                        mt.text = data.parts_name.clone();
                    }
                }
                "Type" | "@MakerName" => {
                    if let Ok(mut mt) = texts_q.get_mut(child) {
                        mt.text = if obj.0 == "Type" {
                            data.sub_category.clone()
                        } else {
                            String::new()
                        };
                    }
                }
                "@Signature" | "@Signature_plate" => {
                    if let Ok(mut v) = vis_q.get_mut(child) {
                        *v = Visibility::Hidden;
                    }
                }
                name if name.starts_with("@Param") => {
                    let idx: usize = name[6..].parse().unwrap_or(99);
                    let row = if idx >= 2 {
                        top_stats.get(idx - 2)
                    } else {
                        bottom_stats.get(idx)
                    };
                    fill_param_dialog(child, row, &children_q, &names_q, &mut texts_q);
                }
                _ => {}
            }
        }
        commands.entity(root).remove::<AssembleDetailDialog>();
    }

    for (root, data) in &spec_q {
        let Ok(top_children) = children_q.get(root) else {
            continue;
        };
        for child in top_children.iter() {
            let Ok(obj) = names_q.get(child) else {
                continue;
            };
            if let Some(num) = obj.0.strip_prefix("@Param") {
                if let Ok(idx) = num.parse::<usize>() {
                    fill_param_dialog(
                        child,
                        data.stats.get(idx),
                        &children_q,
                        &names_q,
                        &mut texts_q,
                    );
                }
            }
        }
        commands.entity(root).remove::<AssembleSpecDetailDialog>();
    }
}

fn fill_param_dialog(
    param_entity: Entity,
    row: Option<&StatRow>,
    children_q: &Query<&Children>,
    names_q: &Query<&MenuObject>,
    texts_q: &mut Query<&mut MenuText>,
) {
    let Ok(sub) = children_q.get(param_entity) else {
        return;
    };
    for sc in sub.iter() {
        let Ok(so) = names_q.get(sc) else {
            continue;
        };
        let Ok(mut mt) = texts_q.get_mut(sc) else {
            continue;
        };
        match (so.0.as_str(), row) {
            ("@Name", Some(r)) => mt.text = r.label.to_string(),
            ("@Value0", Some(r)) => mt.text = r.cur.clone(),
            ("@Separator", Some(_)) => mt.text = ">".to_string(),
            ("@Value1", Some(r)) => {
                mt.text = r.new.clone();
                mt.color = cmp_color(r.cmp);
            }
            ("@Name" | "@Value0" | "@Separator" | "@Value1", None) => {
                mt.text.clear();
            }
            _ => {}
        }
    }
}

/// Synchronizes the offscreen 3D part/weapon preview studio (`PART_PREVIEW_LAYER`) whenever the
/// user enters/leaves `AssembleParts` or highlights a different part/weapon.
#[allow(clippy::too_many_arguments)]
pub fn sync_part_preview(
    mut commands: Commands,
    time: Res<Time>,
    mode: Res<Mode>,
    state: Res<GarageState>,
    garage: Res<crate::Garage>,
    decals: Res<DecalState>,
    mut assets: ResMut<AssembleUiAssets>,
    mut ac_packs: ResMut<crate::AcTexturePacks>,
    mut handles: AcPreviewAssetHandles,
    studio_q: Query<Entity, With<PartPreviewStudio>>,
    pivot_q: Query<Entity, With<PartPreviewPivot>>,
    mesh_q: Query<Entity, With<PartPreviewMesh>>,
) {
    let active_slot = if *mode == Mode::Garage && !state.assemble_detail_view {
        match state.screen {
            GarageScreen::AssembleParts { slot } => Some(slot),
            _ => None,
        }
    } else {
        None
    };

    let Some(slot) = active_slot else {
        // Despawn studio when leaving AssembleParts so zero GPU overhead occurs elsewhere.
        if !studio_q.is_empty() || !pivot_q.is_empty() || !mesh_q.is_empty() {
            for e in &mesh_q {
                commands.entity(e).despawn();
            }
            for e in &pivot_q {
                commands.entity(e).despawn();
            }
            for e in &studio_q {
                commands.entity(e).despawn();
            }
            assets.preview_loaded = None;
            assets.preview_pending = None;
            assets.reset_preview_view();
        }
        return;
    };

    assets.ensure_loaded(&garage.disc, &mut handles.images);

    // Spawn the isolated offscreen studio camera & 3-point lights once.
    if studio_q.is_empty() {
        if let Some(ref target_img) = assets.preview_image {
            let layer = RenderLayers::layer(PART_PREVIEW_LAYER);
            commands.spawn((
                PartPreviewStudio,
                PartPreviewCamera,
                PartOnlyPreviewCamera,
                Camera3d::default(),
                Camera {
                    order: -2,
                    clear_color: ClearColorConfig::Custom(Color::NONE),
                    ..default()
                },
                RenderTarget::Image(target_img.clone().into()),
                Projection::Perspective(PerspectiveProjection {
                    fov: 32.0f32.to_radians(),
                    near: 0.05,
                    far: 100.0,
                    ..default()
                }),
                part_preview_camera_transform(assets.preview_zoom),
                layer.clone(),
            ));
            // Studio key light (warm-white front-top-right).
            commands.spawn((
                PartPreviewStudio,
                DirectionalLight {
                    color: Color::srgb(1.0, 0.98, 0.94),
                    illuminance: 14_000.0,
                    shadow_maps_enabled: false,
                    ..default()
                },
                Transform::from_xyz(3.0, 4.0, 4.0).looking_at(Vec3::ZERO, Vec3::Y),
                layer.clone(),
            ));
            // Cool rim light from behind-left for crisp silhouette separation.
            commands.spawn((
                PartPreviewStudio,
                DirectionalLight {
                    color: Color::srgb(0.55, 0.78, 1.0),
                    illuminance: 6_500.0,
                    shadow_maps_enabled: false,
                    ..default()
                },
                Transform::from_xyz(-3.5, 2.0, -3.0).looking_at(Vec3::ZERO, Vec3::Y),
                layer,
            ));
        }
    }

    let parts = filtered_parts_for_slot(slot, state.assemble_subcat, state.assemble_dmg_filter);
    let sel_id = parts.get(state.cursor).copied().unwrap_or(0);
    let cat = slot.category();
    let is_left = matches!(slot, AssembleSlot::ArmUnitL | AssembleSlot::HangerL);
    let color_key = garage
        .custom_color
        .as_ref()
        .map(|c| {
            let mut h = 0u64;
            for b in c.groups[0][0] {
                h = h.wrapping_mul(31).wrapping_add(b as u64);
            }
            h
        })
        .unwrap_or(0);
    let emblem_hash = if slot == AssembleSlot::Arms {
        let mut h = (garage.active_design.legs as u64) << 16 | (garage.active_design.core as u64);
        if let Some(emblem_key) = decals.ac_emblem_key.as_deref() {
            if decals.active_emblem_asset().is_some() {
                h = h.wrapping_mul(31).wrapping_add(1);
                for b in emblem_key.bytes() {
                    h = h.wrapping_mul(31).wrapping_add(b as u64);
                }
            }
        }
        h
    } else {
        0
    };
    let want = (cat, sel_id, is_left, color_key, emblem_hash);
    if assets.preview_loaded == Some(want) {
        assets.preview_pending = None;
        return;
    }

    // If not already cached (and not the very first preview frame or empty slot), debounce
    // disc FLVER loading by 90 ms so rapid W/S scrolling stays 60 FPS.
    let is_cached =
        sel_id == 0 || (slot != AssembleSlot::Arms && assets.preview_cache.contains_key(&want));
    if !is_cached && assets.preview_loaded.is_some() {
        match assets.preview_pending {
            Some((k, ref mut rem)) if k == want => {
                *rem -= time.delta_secs();
                if *rem > 0.0 {
                    return;
                }
            }
            _ => {
                assets.preview_pending = Some((want, 0.09));
                return;
            }
        }
    }
    assets.preview_pending = None;
    assets.preview_loaded = Some(want);
    assets.reset_preview_view();
    if slot == AssembleSlot::Arms {
        // Start Arms preview in a 3/4 front-left angle so the left shoulder & emblem face the viewer.
        assets.preview_yaw = -0.35;
    }

    for e in &mesh_q {
        commands.entity(e).despawn();
    }
    for e in &pivot_q {
        commands.entity(e).despawn();
    }
    if sel_id == 0 {
        return;
    }

    let layer = RenderLayers::layer(PART_PREVIEW_LAYER);

    // Arms preview: pose the highlighted arm part in its authentic standing/garage pose via
    // `pose::build` + `pose::spawn_static` and bake the active AC shoulder emblem onto its
    // Left Arm submeshes (matching retail 360 `PARTS VIEW`).
    if slot == AssembleSlot::Arms {
        let mut preview_design = garage.active_design;
        preview_design.arms = sel_id as i16;
        let built = assemble::assemble(AC_ASSEMBLY_DESIGN_ST_SLOTS, &preview_design);
        let color: Option<AcColor> = if garage.flat {
            None
        } else {
            garage
                .custom_color
                .clone()
                .or_else(|| paint::load_accolor(&garage.disc, preview_design.coloring_id))
        };

        let ac_cache = &mut *ac_packs;
        let (packs, flvers, mut missing) = (&mut ac_cache.packs, &mut ac_cache.flvers, 0usize);
        let mut loaded = Vec::new();
        let mut known = vec![None; built.placements.len()];
        for (index, p) in built.placements.iter().enumerate() {
            if !matches!(p.column, "legs" | "core" | "arms") {
                continue;
            }
            let places = assemble::root_places(p, &built.placements, &known);
            if let Ok((meshes, rig)) = flvers.rigged_model(&garage.disc, p.model.path, &|root| {
                assemble::place_of(&places, root)
            }) {
                known[index] = Some(assemble::Stored {
                    places,
                    frames: rig.frames.clone(),
                });
                loaded.push(pose::Loaded {
                    index,
                    placement: p,
                    meshes,
                    rig,
                });
            }
        }
        if let (Some(arm_idx), Ok(rig)) = (
            loaded.iter().position(|p| p.placement.column == "arms"),
            pose::build(&garage.disc, &loaded),
        ) {
            let (bmin, bmax) = rig
                .static_part_bounds(arm_idx, &loaded[arm_idx])
                .unwrap_or_else(|| {
                    let mut lo = Vec3::splat(f32::INFINITY);
                    let mut hi = Vec3::splat(f32::NEG_INFINITY);
                    for m in &loaded[arm_idx].meshes {
                        lo = lo.min(Vec3::from_array(m.min));
                        hi = hi.max(Vec3::from_array(m.max));
                    }
                    (lo, hi)
                });
            let center = (bmin + bmax) * 0.5;
            let extent = (bmax - bmin).max(Vec3::splat(0.05));
            let radius = (extent.length() * 0.5).max(0.1);
            let fit_scale = 1.32 / radius;

            let pivot = commands
                .spawn((
                    PartPreviewPivot,
                    Transform::from_rotation(
                        Quat::from_rotation_x(assets.preview_pitch)
                            * Quat::from_rotation_y(assets.preview_yaw),
                    ),
                    Visibility::default(),
                    layer.clone(),
                ))
                .id();

            let face_front = Quat::from_rotation_y(std::f32::consts::PI);
            let arm_root = commands
                .spawn((
                    Transform {
                        translation: -(face_front * center * fit_scale),
                        rotation: face_front,
                        scale: Vec3::splat(fit_scale),
                    },
                    Visibility::default(),
                    layer.clone(),
                    ChildOf(pivot),
                ))
                .id();

            let (_joint_entities, skins) =
                pose::spawn_static(&mut commands, arm_root, &rig, &loaded);
            let mut skins = skins;
            let arm_part = loaded.swap_remove(arm_idx);
            let (joints, binds) = skins.swap_remove(arm_idx);

            // Bake the active AC shoulder emblem onto this previewed arm's Left Arm UV1.
            let l_kata_rest = arm_part
                .rig
                .bones
                .iter()
                .position(|b| b.name.eq_ignore_ascii_case("l_kata"))
                .and_then(|i| binds.get(i))
                .map(|m| m.inverse());
            let left_emblem_tex: Option<Handle<Image>> = l_kata_rest
                .zip(decals.active_emblem_asset())
                .and_then(|(rest, emblem_asset)| {
                    let mut left_cache = decal::DecalMeshCache::default();
                    for lm in &arm_part.meshes {
                        let cx = 0.5 * (lm.min[0] + lm.max[0]);
                        if DecalSlot::from_mesh_bounds("arms", lm.root.as_deref(), Some(cx))
                            == Some(DecalSlot::LeftArm)
                        {
                            if let Some(cm) = decal::CachedPartMesh::from_mesh(
                                DecalSlot::LeftArm,
                                &lm.mesh,
                                joints.clone(),
                                binds.clone(),
                            ) {
                                left_cache.meshes.push(cm);
                            }
                        }
                    }
                    let left_refs: Vec<&decal::CachedPartMesh> = left_cache.meshes.iter().collect();
                    let proj =
                        decal::build_shoulder_emblem_projector(sel_id as i16, rest, &left_refs)?;
                    let pixels = decal::bake_slot_pixels(
                        DecalSlot::LeftArm,
                        512,
                        &left_cache,
                        &[],
                        Some((&proj, emblem_asset)),
                        &mut |_| None,
                    );
                    let mut img = Image::new(
                        bevy::render::render_resource::Extent3d {
                            width: 512,
                            height: 512,
                            depth_or_array_layers: 1,
                        },
                        bevy::render::render_resource::TextureDimension::D2,
                        pixels,
                        TextureFormat::Rgba8UnormSrgb,
                        bevy::asset::RenderAssetUsages::RENDER_WORLD,
                    );
                    img.sampler = bevy::image::ImageSampler::linear();
                    Some(handles.images.add(img))
                });

            let inverse_bindposes = handles
                .bindposes
                .add(SkinnedMeshInverseBindposes::from(binds));
            for lm in arm_part.meshes {
                let cx = 0.5 * (lm.min[0] + lm.max[0]);
                let decal_slot = DecalSlot::from_mesh_bounds("arms", lm.root.as_deref(), Some(cx));
                let decal_tex = if decal_slot == Some(DecalSlot::LeftArm) {
                    left_emblem_tex.clone()
                } else {
                    None
                };
                let base = acvd_render::material(
                    &garage.disc,
                    &lm,
                    packs,
                    &mut handles.images,
                    garage.flat,
                    &mut missing,
                );
                let painted = color.as_ref().and_then(|col| {
                    paint::material(
                        &garage.disc,
                        &base,
                        &lm,
                        "arms",
                        col,
                        decal_tex,
                        packs,
                        &mut handles.images,
                    )
                });
                let mut e = commands.spawn((
                    PartPreviewMesh,
                    Mesh3d(handles.meshes.add(lm.mesh)),
                    SkinnedMesh {
                        inverse_bindposes: inverse_bindposes.clone(),
                        joints: joints.clone(),
                    },
                    NoFrustumCulling,
                    Transform::default(),
                    layer.clone(),
                    ChildOf(arm_root),
                ));
                match painted {
                    Some(mat) => {
                        e.insert(MeshMaterial3d(handles.ac_mats.add(mat)));
                    }
                    None => {
                        e.insert(MeshMaterial3d(handles.std_mats.add(base)));
                    }
                }
            }
            return;
        }
    }

    let cached_meshes = if let Some(cached) = assets.preview_cache.get(&want) {
        cached.clone()
    } else {
        let Some(part) = ac_part(cat, sel_id) else {
            return;
        };
        let pref = slot.model_prefix();
        let model_asset = pref
            .and_then(|p| part.models.iter().find(|(k, _)| *k == p).map(|(_, a)| *a))
            .or_else(|| part.models.first().map(|(_, a)| *a));
        let Some(asset_path) = model_asset else {
            return;
        };

        let Ok(loaded) = ac_packs.flvers.model(&garage.disc, asset_path) else {
            return;
        };
        if loaded.is_empty() {
            return;
        }

        // Compute combined AABB across all submeshes so the part centers and scales uniformly.
        let mut bmin = Vec3::splat(f32::INFINITY);
        let mut bmax = Vec3::splat(f32::NEG_INFINITY);
        for lm in &loaded {
            bmin = bmin.min(Vec3::from_array(lm.min));
            bmax = bmax.max(Vec3::from_array(lm.max));
        }
        let center = (bmin + bmax) * 0.5;
        let extent = (bmax - bmin).max(Vec3::splat(0.05));
        let radius = (extent.length() * 0.5).max(0.1);
        let fit_scale = 1.05 / radius;

        let color: Option<AcColor> = garage
            .custom_color
            .clone()
            .or_else(|| paint::load_accolor(&garage.disc, garage.active_design.coloring_id));
        let column = match slot {
            AssembleSlot::Head => "head",
            AssembleSlot::Core => "core",
            AssembleSlot::Arms => "arms",
            AssembleSlot::Legs => "legs",
            AssembleSlot::Booster => "booster",
            AssembleSlot::ArmUnitR | AssembleSlot::HangerR => "arm_r",
            AssembleSlot::ArmUnitL | AssembleSlot::HangerL => "arm_l",
            AssembleSlot::Shoulder => "shoulder",
            AssembleSlot::OveredWeapon => "back_r",
            _ => "core",
        };
        let mut missing = 0usize;
        let mut built = Vec::with_capacity(loaded.len());
        for lm in loaded {
            let base = acvd_render::material(
                &garage.disc,
                &lm,
                &mut ac_packs.packs,
                &mut handles.images,
                garage.flat,
                &mut missing,
            );
            let painted = color.as_ref().and_then(|col| {
                paint::material(
                    &garage.disc,
                    &base,
                    &lm,
                    column,
                    col,
                    None,
                    &mut ac_packs.packs,
                    &mut handles.images,
                )
            });
            let mesh_h = handles.meshes.add(lm.mesh);
            let child_xform = Transform {
                translation: -center * fit_scale,
                rotation: Quat::IDENTITY,
                scale: Vec3::splat(fit_scale),
            };
            let mat = if let Some(mat) = painted {
                PreviewMat::Painted(handles.ac_mats.add(mat))
            } else {
                PreviewMat::Std(handles.std_mats.add(base))
            };
            built.push(CachedPreviewMesh {
                mesh: mesh_h,
                mat,
                transform: child_xform,
            });
        }
        assets.preview_cache.insert(want, built.clone());
        built
    };

    let pivot = commands
        .spawn((
            PartPreviewPivot,
            Transform::from_rotation(
                Quat::from_rotation_x(assets.preview_pitch)
                    * Quat::from_rotation_y(assets.preview_yaw),
            ),
            Visibility::default(),
            layer.clone(),
        ))
        .id();

    for item in cached_meshes {
        match item.mat {
            PreviewMat::Painted(mat_h) => {
                commands.spawn((
                    PartPreviewMesh,
                    Mesh3d(item.mesh),
                    MeshMaterial3d(mat_h),
                    item.transform,
                    layer.clone(),
                    ChildOf(pivot),
                ));
            }
            PreviewMat::Std(mat_h) => {
                commands.spawn((
                    PartPreviewMesh,
                    Mesh3d(item.mesh),
                    MeshMaterial3d(mat_h),
                    item.transform,
                    layer.clone(),
                    ChildOf(pivot),
                ));
            }
        }
    }
}

/// Gently rotates the 3D part/weapon preview on its studio turntable, and supports
/// interactive mouse-drag rotation and scroll-wheel zoom when hovering the `PARTS VIEW` panel.
#[allow(clippy::too_many_arguments)]
pub fn rotate_part_preview(
    time: Res<Time>,
    mode: Res<Mode>,
    state: Res<GarageState>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    mut assets: ResMut<AssembleUiAssets>,
    mut pivots: Query<&mut Transform, (With<PartPreviewPivot>, Without<PartOnlyPreviewCamera>)>,
    mut cams: Query<&mut Transform, (With<PartOnlyPreviewCamera>, Without<PartPreviewPivot>)>,
) {
    let active = *mode == Mode::Garage
        && matches!(state.screen, GarageScreen::AssembleParts { .. })
        && !state.assemble_detail_view;
    if !active || pivots.is_empty() {
        assets.preview_dragging = false;
        return;
    }

    let over_panel = windows
        .single()
        .ok()
        .and_then(|w| {
            w.cursor_position()
                .map(|c| cursor_in_part_preview_panel(Vec2::new(w.width(), w.height()), c))
        })
        .unwrap_or(false);

    if buttons.just_pressed(MouseButton::Left) && over_panel {
        assets.preview_dragging = true;
        assets.preview_manual = true;
    }
    if !buttons.pressed(MouseButton::Left) {
        assets.preview_dragging = false;
    }

    if assets.preview_dragging {
        if motion.delta.length_squared() > 0.0 {
            let (mut yaw, mut pitch) = (assets.preview_yaw, assets.preview_pitch);
            apply_part_preview_drag(&mut yaw, &mut pitch, motion.delta);
            assets.preview_yaw = yaw;
            assets.preview_pitch = pitch;
        }
    } else if !assets.preview_manual {
        assets.preview_yaw =
            (assets.preview_yaw + time.delta_secs() * 0.55) % std::f32::consts::TAU;
    }

    if over_panel && scroll.delta.y.abs() > 0.0 {
        let mut zoom = assets.preview_zoom;
        apply_part_preview_zoom(&mut zoom, scroll.delta.y, scroll.unit);
        assets.preview_zoom = zoom;
    }

    let rot =
        Quat::from_rotation_x(assets.preview_pitch) * Quat::from_rotation_y(assets.preview_yaw);
    for mut tf in &mut pivots {
        tf.rotation = rot;
    }
    let cam_tf = part_preview_camera_transform(assets.preview_zoom);
    for mut tf in &mut cams {
        *tf = cam_tf;
    }
}

/// Asset storage bundle for [`sync_ac_preview`] so its system parameters stay concise.
#[derive(bevy::ecs::system::SystemParam)]
pub struct AcPreviewAssetHandles<'w> {
    meshes: ResMut<'w, Assets<Mesh>>,
    std_mats: ResMut<'w, Assets<StandardMaterial>>,
    ac_mats: ResMut<'w, Assets<paint::AcPaintMaterial>>,
    images: ResMut<'w, Assets<Image>>,
    bindposes: ResMut<'w, Assets<SkinnedMeshInverseBindposes>>,
}

/// Synchronizes the Garage Assemble blurred bokeh + `+` grid backdrop (`ActiveMenuBackdrop`)
/// and the isolated right-side `ASSEMBLY VIEW` full-AC preview studio (`AC_PREVIEW_LAYER`).
///
/// The preview AC is a separate 3D model from the main garage mech so [`sync_part_hologram`]
/// highlights only the preview AC's selected slot while the main garage mech remains solid and
/// untouched in the blurred background.
#[allow(clippy::too_many_arguments)]
pub fn sync_ac_preview(
    mut commands: Commands,
    time: Res<Time>,
    mode: Res<Mode>,
    state: Res<GarageState>,
    garage: Res<crate::Garage>,
    decals: Res<DecalState>,
    mut backdrop: ResMut<ActiveMenuBackdrop>,
    mut assets: ResMut<AssembleUiAssets>,
    mut ac_packs: ResMut<crate::AcTexturePacks>,
    mut handles: AcPreviewAssetHandles,
    studio_q: Query<Entity, With<AcPreviewStudio>>,
    pivot_q: Query<Entity, With<AcPreviewPivot>>,
) {
    let in_assemble =
        *mode == Mode::Garage && matches!(state.screen, GarageScreen::AssembleParts { .. });
    // Blurred bokeh garage + crisp `+` reticle grid backdrop (active in both Normal and Detail Specs).
    backdrop.0 = if in_assemble {
        Vec4::new(1.0, 0.48, 0.36, 92.0)
    } else {
        Vec4::ZERO
    };

    let active_slot = if in_assemble && !state.assemble_detail_view {
        match state.screen {
            GarageScreen::AssembleParts { slot } => Some(slot),
            _ => None,
        }
    } else {
        None
    };

    let Some(slot) = active_slot else {
        if !studio_q.is_empty() || !pivot_q.is_empty() {
            for e in &pivot_q {
                commands.entity(e).despawn();
            }
            for e in &studio_q {
                commands.entity(e).despawn();
            }
            assets.ac_preview_loaded = None;
            assets.ac_preview_pending = None;
            assets.ac_preview_last_slot = None;
            assets.ac_preview_dragging = false;
            assets.ac_preview_manual = false;
        }
        return;
    };

    assets.ensure_loaded(&garage.disc, &mut handles.images);

    // Re-angle the preview AC toward the newly selected slot when switching slots.
    if assets.ac_preview_last_slot != Some(slot) {
        let first_open = assets.ac_preview_last_slot.is_none();
        assets.ac_preview_last_slot = Some(slot);
        assets.ac_preview_manual = false;
        if first_open {
            assets.ac_preview_yaw = ac_preview_default_yaw(slot);
            assets.ac_preview_pitch = 0.08;
            assets.ac_preview_zoom = 1.0;
        }
    }

    // Spawn the isolated offscreen studio camera & 3-point lights once.
    if studio_q.is_empty() {
        if let Some(ref target_img) = assets.ac_preview_image {
            let layer = RenderLayers::layer(AC_PREVIEW_LAYER);
            commands.spawn((
                AcPreviewStudio,
                PartPreviewCamera,
                AcPreviewCamera,
                Camera3d::default(),
                Camera {
                    order: -3,
                    clear_color: ClearColorConfig::Custom(Color::NONE),
                    ..default()
                },
                RenderTarget::Image(target_img.clone().into()),
                Projection::Perspective(PerspectiveProjection {
                    fov: 30.0f32.to_radians(),
                    near: 0.1,
                    far: 120.0,
                    ..default()
                }),
                ac_preview_camera_transform(assets.ac_preview_height, assets.ac_preview_zoom),
                layer.clone(),
            ));
            // Studio key light (warm-white front-top-right).
            commands.spawn((
                AcPreviewStudio,
                DirectionalLight {
                    color: Color::srgb(1.0, 0.97, 0.92),
                    illuminance: 15_000.0,
                    shadow_maps_enabled: false,
                    ..default()
                },
                Transform::from_xyz(4.0, 6.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
                layer.clone(),
            ));
            // Soft cool fill light from front-left to keep mechanical detail readable.
            commands.spawn((
                AcPreviewStudio,
                DirectionalLight {
                    color: Color::srgb(0.65, 0.78, 0.95),
                    illuminance: 4_500.0,
                    shadow_maps_enabled: false,
                    ..default()
                },
                Transform::from_xyz(-4.5, 2.5, 3.5).looking_at(Vec3::ZERO, Vec3::Y),
                layer.clone(),
            ));
            // Crisp cool-blue rim light from behind-left for clean silhouette separation.
            commands.spawn((
                AcPreviewStudio,
                DirectionalLight {
                    color: Color::srgb(0.50, 0.75, 1.0),
                    illuminance: 7_500.0,
                    shadow_maps_enabled: false,
                    ..default()
                },
                Transform::from_xyz(-3.5, 4.0, -5.0).looking_at(Vec3::ZERO, Vec3::Y),
                layer,
            ));
        }
    }

    let color_hash = garage
        .custom_color
        .as_ref()
        .map(|c| {
            let mut h = 0u64;
            for g in &c.groups {
                for ch in g {
                    for &b in ch {
                        h = h.wrapping_mul(31).wrapping_add(b as u64);
                    }
                }
            }
            h
        })
        .unwrap_or(0);
    let want = ac_preview_key(&garage.active_design, color_hash);
    if assets.ac_preview_loaded == Some(want) {
        assets.ac_preview_pending = None;
        return;
    }

    // Brief 60 ms debounce when scrolling rapidly through parts after initial spawn.
    if assets.ac_preview_loaded.is_some() {
        match assets.ac_preview_pending {
            Some((k, ref mut rem)) if k == want => {
                *rem -= time.delta_secs();
                if *rem > 0.0 {
                    return;
                }
            }
            _ => {
                assets.ac_preview_pending = Some((want, 0.06));
                return;
            }
        }
    }
    assets.ac_preview_pending = None;
    assets.ac_preview_loaded = Some(want);

    for e in &pivot_q {
        commands.entity(e).despawn();
    }

    // Assemble & pose the preview AC on `AC_PREVIEW_LAYER` using cached FLVERs/TPFs.
    let design_data = garage.active_design;
    let built = assemble::assemble(AC_ASSEMBLY_DESIGN_ST_SLOTS, &design_data);
    let accolor = if garage.flat {
        None
    } else {
        garage
            .custom_color
            .or_else(|| paint::load_accolor(&garage.disc, design_data.coloring_id))
    };

    let ac_cache = &mut *ac_packs;
    let (packs, flvers, mut missing) = (&mut ac_cache.packs, &mut ac_cache.flvers, 0usize);
    let mut loaded = Vec::new();
    let mut known = vec![None; built.placements.len()];
    for (index, p) in built.placements.iter().enumerate() {
        let places = assemble::root_places(p, &built.placements, &known);
        if let Ok((meshes, rig)) = flvers.rigged_model(&garage.disc, p.model.path, &|root| {
            assemble::place_of(&places, root)
        }) {
            known[index] = Some(assemble::Stored {
                places,
                frames: rig.frames.clone(),
            });
            loaded.push(pose::Loaded {
                index,
                placement: p,
                meshes,
                rig,
            });
        }
    }
    if loaded.is_empty() {
        return;
    }
    let Ok(rig) = pose::build(&garage.disc, &loaded) else {
        return;
    };

    let mut bmin = Vec3::splat(f32::MAX);
    let mut bmax = Vec3::splat(f32::MIN);
    for part in &loaded {
        for mesh in &part.meshes {
            bmin = bmin.min(Vec3::from(mesh.min));
            bmax = bmax.max(Vec3::from(mesh.max));
        }
    }
    let center = if bmin.x.is_finite() && bmax.x.is_finite() {
        (bmin + bmax) * 0.5
    } else {
        Vec3::new(0.0, 3.2, 0.0)
    };
    let ac_height = if bmin.y.is_finite() && bmax.y.is_finite() {
        (bmax.y - bmin.y).clamp(4.0, 10.0)
    } else {
        6.8
    };
    assets.ac_preview_height = ac_height;

    let layer = RenderLayers::layer(AC_PREVIEW_LAYER);
    let pivot = commands
        .spawn((
            AcPreviewPivot,
            Transform::from_rotation(
                Quat::from_rotation_x(assets.ac_preview_pitch)
                    * Quat::from_rotation_y(assets.ac_preview_yaw),
            ),
            Visibility::default(),
            layer.clone(),
        ))
        .id();

    let face_front = Quat::from_rotation_y(std::f32::consts::PI);
    let ac_root = commands
        .spawn((
            Transform {
                translation: -(face_front * center),
                rotation: face_front,
                scale: Vec3::ONE,
            },
            Visibility::default(),
            layer.clone(),
            ChildOf(pivot),
        ))
        .id();

    let (_joint_entities, skins) = pose::spawn_static(&mut commands, ac_root, &rig, &loaded);
    for (part, (joints, binds)) in loaded.into_iter().zip(skins) {
        let inverse_bindposes = handles
            .bindposes
            .add(SkinnedMeshInverseBindposes::from(binds));
        for mesh in part.meshes {
            let center_x = 0.5 * (mesh.min[0] + mesh.max[0]);
            let decal_slot = DecalSlot::from_mesh_bounds(
                part.placement.column,
                mesh.root.as_deref(),
                Some(center_x),
            );
            let base = acvd_render::material(
                &garage.disc,
                &mesh,
                packs,
                &mut handles.images,
                garage.flat,
                &mut missing,
            );
            let painted = accolor.as_ref().and_then(|col| {
                paint::material(
                    &garage.disc,
                    &base,
                    &mesh,
                    part.placement.column,
                    col,
                    decal_slot.map(|s| decals.slot_handle(s)),
                    packs,
                    &mut handles.images,
                )
            });
            let mut mesh_entity = commands.spawn((
                AcPreviewMesh,
                crate::AcPartColumn(part.placement.column),
                Mesh3d(handles.meshes.add(mesh.mesh)),
                SkinnedMesh {
                    inverse_bindposes: inverse_bindposes.clone(),
                    joints: joints.clone(),
                },
                NoFrustumCulling,
                Transform::default(),
                layer.clone(),
                ChildOf(ac_root),
            ));
            match painted {
                Some(mat) => {
                    mesh_entity.insert(MeshMaterial3d(handles.ac_mats.add(mat)));
                }
                None => {
                    mesh_entity.insert(MeshMaterial3d(handles.std_mats.add(base)));
                }
            }
        }
    }
}

/// Interactive orbit and scroll-zoom controller for the right-side `ASSEMBLY VIEW` preview AC.
#[allow(clippy::too_many_arguments)]
pub fn rotate_ac_preview(
    time: Res<Time>,
    mode: Res<Mode>,
    state: Res<GarageState>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    gamepads: Query<&Gamepad>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    mut assets: ResMut<AssembleUiAssets>,
    mut pivots: Query<&mut Transform, (With<AcPreviewPivot>, Without<AcPreviewCamera>)>,
    mut cams: Query<&mut Transform, (With<AcPreviewCamera>, Without<AcPreviewPivot>)>,
) {
    let active_slot = if *mode == Mode::Garage && !state.assemble_detail_view {
        match state.screen {
            GarageScreen::AssembleParts { slot } => Some(slot),
            _ => None,
        }
    } else {
        None
    };
    let Some(slot) = active_slot else {
        assets.ac_preview_dragging = false;
        return;
    };
    if pivots.is_empty() {
        assets.ac_preview_dragging = false;
        return;
    }

    let cursor_win = windows.single().ok().and_then(|w| {
        w.cursor_position()
            .map(|c| (Vec2::new(w.width(), w.height()), c))
    });
    let over_ac = cursor_win
        .map(|(ws, c)| cursor_in_ac_preview_panel(ws, c))
        .unwrap_or(false);
    let over_part = cursor_win
        .map(|(ws, c)| cursor_in_part_preview_panel(ws, c))
        .unwrap_or(false);

    let start_drag = (buttons.just_pressed(MouseButton::Left) && over_ac)
        || (buttons.just_pressed(MouseButton::Right) && !over_part);
    if start_drag {
        assets.ac_preview_dragging = true;
        assets.ac_preview_manual = true;
    }
    if !buttons.pressed(MouseButton::Left) && !buttons.pressed(MouseButton::Right) {
        assets.ac_preview_dragging = false;
    }

    if assets.ac_preview_dragging && motion.delta.length_squared() > 0.0 {
        assets.ac_preview_yaw =
            (assets.ac_preview_yaw + motion.delta.x * 0.008).rem_euclid(std::f32::consts::TAU);
        assets.ac_preview_pitch =
            (assets.ac_preview_pitch + motion.delta.y * 0.006).clamp(-0.65, 0.85);
    }

    let mut right_stick = Vec2::ZERO;
    for pad in &gamepads {
        right_stick += pad.right_stick();
    }
    if right_stick.length() > 0.15 {
        assets.ac_preview_manual = true;
        assets.ac_preview_yaw = (assets.ac_preview_yaw + right_stick.x * 2.0 * time.delta_secs())
            .rem_euclid(std::f32::consts::TAU);
        assets.ac_preview_pitch =
            (assets.ac_preview_pitch - right_stick.y * 1.3 * time.delta_secs()).clamp(-0.65, 0.85);
    }

    if !assets.ac_preview_manual {
        let target_yaw = ac_preview_default_yaw(slot);
        let mut dyaw = (target_yaw - assets.ac_preview_yaw) % std::f32::consts::TAU;
        if dyaw > std::f32::consts::PI {
            dyaw -= std::f32::consts::TAU;
        } else if dyaw < -std::f32::consts::PI {
            dyaw += std::f32::consts::TAU;
        }
        let t = 1.0 - (-8.0 * time.delta_secs()).exp();
        assets.ac_preview_yaw += dyaw * t;
        assets.ac_preview_pitch += (0.08 - assets.ac_preview_pitch) * t;
    }

    if over_ac && scroll.delta.y.abs() > 0.0 {
        let step = match scroll.unit {
            MouseScrollUnit::Line => scroll.delta.y,
            MouseScrollUnit::Pixel => scroll.delta.y * 0.02,
        };
        assets.ac_preview_zoom = (assets.ac_preview_zoom * (1.0 - step * 0.10)).clamp(0.55, 1.85);
    }

    let rot = Quat::from_rotation_x(assets.ac_preview_pitch)
        * Quat::from_rotation_y(assets.ac_preview_yaw);
    for mut tf in &mut pivots {
        tf.rotation = rot;
    }
    let cam_tf = ac_preview_camera_transform(assets.ac_preview_height, assets.ac_preview_zoom);
    for mut tf in &mut cams {
        *tf = cam_tf;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acvd_formats::vfs;
    use std::path::PathBuf;

    #[test]
    fn hovered_scroll_strip_maps_category_and_parts_rects() {
        let win = Vec2::new(SCREEN_W, SCREEN_H);
        assert_eq!(
            hovered_scroll_strip(win, Vec2::new(400.0, 170.0)),
            Some(AssembleScrollStrip::CategoryList)
        );
        assert_eq!(
            hovered_scroll_strip(win, Vec2::new(120.0, 300.0)),
            Some(AssembleScrollStrip::PartsList)
        );
        assert_eq!(hovered_scroll_strip(win, Vec2::new(900.0, 400.0)), None);
        // Outside the letterboxed UI origin on a taller window should miss.
        assert_eq!(
            hovered_scroll_strip(Vec2::new(1280.0, 900.0), Vec2::new(120.0, 10.0)),
            None
        );
    }

    #[test]
    fn part_preview_panel_hit_test_and_drag_zoom_controls() {
        let win = Vec2::new(SCREEN_W, SCREEN_H);
        // Inside 3D preview box and lower stat rows of `PARTS VIEW` panel.
        assert!(cursor_in_part_preview_panel(win, Vec2::new(420.0, 320.0)));
        assert!(cursor_in_part_preview_panel(win, Vec2::new(420.0, 500.0)));
        // Outside: `@PartsList` thumbnail strip, `@PartsCategoryList`, right `ASSEMBLY VIEW`.
        assert!(!cursor_in_part_preview_panel(win, Vec2::new(120.0, 320.0)));
        assert!(!cursor_in_part_preview_panel(win, Vec2::new(420.0, 170.0)));
        assert!(!cursor_in_part_preview_panel(win, Vec2::new(900.0, 360.0)));

        // Right-side `ASSEMBLY VIEW` hit test (`[705,109,1213,567]`).
        assert!(cursor_in_ac_preview_panel(win, Vec2::new(950.0, 340.0)));
        assert!(!cursor_in_ac_preview_panel(win, Vec2::new(420.0, 320.0)));
        assert!(!cursor_in_ac_preview_panel(win, Vec2::new(950.0, 40.0)));

        // `ASSEMBLY VIEW` camera framing clamps height and zoom cleanly.
        let near = ac_preview_camera_transform(6.8, 0.1);
        let mid = ac_preview_camera_transform(6.8, 1.0);
        let far = ac_preview_camera_transform(6.8, 10.0);
        assert!(near.translation.z < mid.translation.z);
        assert!(mid.translation.z < far.translation.z);
        assert!(ac_preview_default_yaw(AssembleSlot::ArmUnitR) > 0.0);
        assert!(ac_preview_default_yaw(AssembleSlot::ArmUnitL) < 0.0);

        // Dragging updates yaw and clamps pitch within ±PREVIEW_PITCH_LIMIT.
        let mut yaw = 0.0f32;
        let mut pitch = 0.0f32;
        apply_part_preview_drag(&mut yaw, &mut pitch, Vec2::new(100.0, 50.0));
        assert!((yaw - 1.0).abs() < 1e-5);
        assert!((pitch - 0.5).abs() < 1e-5);
        apply_part_preview_drag(&mut yaw, &mut pitch, Vec2::new(0.0, 500.0));
        assert_eq!(pitch, PREVIEW_PITCH_LIMIT);
        apply_part_preview_drag(&mut yaw, &mut pitch, Vec2::new(0.0, -900.0));
        assert_eq!(pitch, -PREVIEW_PITCH_LIMIT);

        // Scroll up zooms in (smaller multiplier), scroll down zooms out (larger multiplier).
        let mut zoom = AssembleUiAssets::default().preview_zoom;
        assert_eq!(zoom, 1.0);
        apply_part_preview_zoom(&mut zoom, 1.0, MouseScrollUnit::Line);
        assert!(zoom < 1.0 && zoom >= PREVIEW_ZOOM_MIN);
        let close_dist = part_preview_camera_transform(zoom).translation.length();
        assert!(close_dist < PREVIEW_CAM_BASE.length());
        for _ in 0..30 {
            apply_part_preview_zoom(&mut zoom, 1.0, MouseScrollUnit::Line);
        }
        assert_eq!(zoom, PREVIEW_ZOOM_MIN);
        for _ in 0..40 {
            apply_part_preview_zoom(&mut zoom, -1.0, MouseScrollUnit::Line);
        }
        assert_eq!(zoom, PREVIEW_ZOOM_MAX);
    }

    #[test]
    fn part_thumbnails_subcategories_and_descriptions_resolve_from_disc() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let disc = Disc::open(&vfs::default_disc(&root)).expect("open 360 disc");
        let drb = acvd_formats::drb::read(&disc.asset(ASSEMBLE_DRB).expect("assemble.drb"))
            .expect("parse assemble.drb");
        for dlg in &drb.dialogs {
            if dlg.name == "@PartsItem" || dlg.name == "@PartsCategoryItem" {
                assert_eq!(dlg.size, [84, 54]);
                for obj in &dlg.objects {
                    if let acvd_formats::drb::Shape::Sprite { uv, .. } = obj.shape {
                        assert_eq!(uv, [0, 0, 84, 54], "thumbnail UV in {}", dlg.name);
                    }
                }
            }
        }
        let sub_fmg = acvd_formats::fmg::read(
            &disc
                .asset("lang/en/text/partssubcategory_en.fmg")
                .expect("partssubcategory_en.fmg"),
        )
        .expect("read partssubcategory_en.fmg");
        let exp_fmg = acvd_formats::fmg::read(
            &disc
                .asset("lang/en/text/partsexplain_en.fmg")
                .expect("partsexplain_en.fmg"),
        )
        .expect("read partsexplain_en.fmg");

        // Arms subcategories (KE / CE / TE / WEAPON ARMS).
        let arm_subs = subcategories_for_slot(AssembleSlot::Arms, Some(&sub_fmg));
        assert!(
            arm_subs.len() >= 4,
            "arms should have 4 subcategories: {arm_subs:?}"
        );
        assert_eq!(subcategory_name(Some(&sub_fmg), 2, 1), "KE-RESISTANT TYPE");
        assert_eq!(subcategory_name(Some(&sub_fmg), 2, 4), "WEAPON ARMS");

        // Arm Unit weapon types & KE/CE/TE damage filtering.
        let wep_subs = subcategories_for_slot(AssembleSlot::ArmUnitR, Some(&sub_fmg));
        assert!(
            wep_subs.len() >= 20,
            "arm units should have 20+ weapon types"
        );
        let ke_weps = filtered_parts_for_slot(AssembleSlot::ArmUnitR, None, Some(0));
        let ce_weps = filtered_parts_for_slot(AssembleSlot::ArmUnitR, None, Some(6));
        let te_weps = filtered_parts_for_slot(AssembleSlot::ArmUnitR, None, Some(1));
        assert!(!ke_weps.is_empty() && !ce_weps.is_empty() && !te_weps.is_empty());

        // Real part description resolves from partsexplain_en.fmg.
        let desc = part_description(Some(&exp_fmg), 2, 110);
        assert!(
            !desc.is_empty(),
            "arms part 110 should have an English description"
        );

        // Every category has valid thumbnail packs on disc.
        let mut packs = Packs::default();
        for slot in AssembleSlot::ALL {
            let cat = slot.category();
            let (dir, prefix) = thumbnail_dir_prefix(cat).expect("dir/prefix");
            let first = parts_for_slot(slot)
                .into_iter()
                .find(|&id| id > 0)
                .expect("part");
            let model_id = ac_part(cat, first).unwrap().model_id;
            let stem = format!("{prefix}{model_id:04}");
            let pack = format!("image/partsthumbnail/{dir}/{stem}.tpf.dcx");
            let tref = acvd_data::textures_named(&stem)
                .find(|r| r.pack.eq_ignore_ascii_case(&pack))
                .unwrap_or_else(|| panic!("missing thumbnail {pack}"));
            let img = packs.texture(&disc, tref).expect("decode thumbnail");
            assert_eq!((img.width(), img.height()), (128, 64));
        }
    }

    #[test]
    fn part_stat_rows_compare_higher_and_lower_is_better() {
        // Arms: AP higher is better, Weight lower is better.
        let rows = part_stat_rows(AssembleSlot::Arms, 111, 110, false);
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[0].label, "AP");
        assert_eq!(rows[0].cmp, 1); // 4564 > 3377 -> blue
        assert_eq!(rows[4].label, "Weight");
        assert_eq!(rows[4].cmp, -1); // 498 > 455 -> red (heavier is worse)
    }

    #[test]
    fn posed_arm_preview_bounds_are_upright_and_finite() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let disc = Disc::open(&vfs::default_disc(&root)).expect("open 360 disc");
        let design = acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find(|r| r.id == 6007)
            .expect("design 6007")
            .data;
        let built = assemble::assemble(AC_ASSEMBLY_DESIGN_ST_SLOTS, &design);
        let mut flvers = acvd_render::FlverCache::default();
        let mut loaded = Vec::new();
        let mut known = vec![None; built.placements.len()];
        for (index, p) in built.placements.iter().enumerate() {
            if !matches!(p.column, "legs" | "core" | "arms") {
                continue;
            }
            let places = assemble::root_places(p, &built.placements, &known);
            let (meshes, rig) = flvers
                .rigged_model(&disc, p.model.path, &|root| {
                    assemble::place_of(&places, root)
                })
                .expect("rigged model");
            known[index] = Some(assemble::Stored {
                places,
                frames: rig.frames.clone(),
            });
            loaded.push(pose::Loaded {
                index,
                placement: p,
                meshes,
                rig,
            });
        }
        let arm_idx = loaded
            .iter()
            .position(|p| p.placement.column == "arms")
            .expect("arms");
        let rig = pose::build(&disc, &loaded).expect("pose build");
        let (bmin, bmax) = rig
            .static_part_bounds(arm_idx, &loaded[arm_idx])
            .expect("posed bounds");
        let ext = bmax - bmin;
        assert!(ext.x > 1.0 && ext.y > 1.0 && ext.z > 0.5, "ext={ext:?}");
    }
}
