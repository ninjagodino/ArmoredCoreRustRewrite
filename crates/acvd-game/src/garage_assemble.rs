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
use acvd_formats::accolor::AcColor;
use acvd_formats::fmg::Fmg;
use acvd_formats::vfs::Disc;
use acvd_render::menu::{self, Layout, MenuObject, MenuRoot, MenuText};
use acvd_render::Packs;
use bevy::camera::visibility::RenderLayers;
use bevy::camera::RenderTarget;
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::ui::px;

use crate::garage::{
    part_name, parts_for_slot, AcSpecs, AssembleSlot, GarageScreen, GarageState, GarageUi, Mode,
    UiAction, UiRow,
};
use crate::paint;

/// Dedicated Bevy render layer for the offscreen 3D part/weapon preview studio (`@PartsLayout`).
pub const PART_PREVIEW_LAYER: usize = 4;

const ASSEMBLE_DRB: &str = "lang/en/menu/assemble.drb.dcx";
const SCREEN_W: f32 = 1280.0;
const SCREEN_H: f32 = 720.0;
/// Offscreen render target dimensions for `@PartsLayout` upper preview window (`[178,231,676,412]` = 498×181).
const PREVIEW_TEX_W: u32 = 512;
const PREVIEW_TEX_H: u32 = 186;
/// Number of visible part thumbnails in `@PartsList` (`[86,220,170,562]`, 6 × 57px).
pub const ASSEMBLE_THUMB_WINDOW: usize = 6;
/// Number of visible slot tabs in `@PartsCategoryList` (`[86,148,686,202]`, 7 × 86px).
pub const ASSEMBLE_CAT_WINDOW: usize = 7;

/// Marker on the offscreen 3D camera & lights that render the hovered part/weapon into `@PartsLayout`.
#[derive(Component)]
pub struct PartPreviewCamera;

/// Marker on studio entities (camera + lights) for the 3D part preview.
#[derive(Component)]
pub struct PartPreviewStudio;

/// Marker on the rotating pivot holding the currently previewed 3D part/weapon FLVER.
#[derive(Component)]
pub struct PartPreviewPivot;

/// Marker on individual submesh entities inside the 3D part preview.
#[derive(Component)]
pub struct PartPreviewMesh;

type PreviewKey = (u8, u16, bool, u64);

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
#[derive(Resource, Default)]
pub struct AssembleUiAssets {
    pub initialized: bool,
    pub layout: Option<Layout>,
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
    /// Dedicated TPF pack cache for 128×64 part thumbnails.
    pub packs: Packs,
}

impl AssembleUiAssets {
    pub fn ensure_loaded(&mut self, disc: &Disc, images: &mut Assets<Image>) {
        if self.initialized {
            return;
        }
        self.initialized = true;
        match menu::load(disc, ASSEMBLE_DRB, images) {
            Ok(layout) => self.layout = Some(layout),
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
                let found = acvd_data::textures_named(&zstem)
                    .find(|r| r.pack.eq_ignore_ascii_case(&zpack));
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
            top.push(stat_i32("AP", pf(cur_id, c, "ap"), pf(sel_id, c, "ap"), true));
            top.push(stat_i32("KE Defense", pf(cur_id, c, "ke_def"), pf(sel_id, c, "ke_def"), true));
            top.push(stat_i32("CE Defense", pf(cur_id, c, "ce_def"), pf(sel_id, c, "ce_def"), true));
            top.push(stat_i32("TE Defense", pf(cur_id, c, "te_def"), pf(sel_id, c, "te_def"), true));
            if detail {
                top.push(stat_i32("Stability", pf(cur_id, c, "balance"), pf(sel_id, c, "balance"), true));
                top.push(stat_i32("Camera Perf.", pf(cur_id, c, "camera_ability"), pf(sel_id, c, "camera_ability"), true));
                top.push(stat_i32("Scan Perf.", pf(cur_id, c, "scan_performance"), pf(sel_id, c, "scan_performance"), true));
            }
        }
        1 => {
            // CORE
            top.push(stat_i32("AP", pf(cur_id, c, "ap"), pf(sel_id, c, "ap"), true));
            top.push(stat_i32("KE Defense", pf(cur_id, c, "ke_def"), pf(sel_id, c, "ke_def"), true));
            top.push(stat_i32("CE Defense", pf(cur_id, c, "ce_def"), pf(sel_id, c, "ce_def"), true));
            top.push(stat_i32("TE Defense", pf(cur_id, c, "te_def"), pf(sel_id, c, "te_def"), true));
            if detail {
                top.push(stat_i32("EN Conductivity", pf(cur_id, c, "en_conductivity"), pf(sel_id, c, "en_conductivity"), true));
                top.push(stat_i32("Recon Storage", pf(cur_id, c, "recon_capacity"), pf(sel_id, c, "recon_capacity"), true));
                top.push(stat_i32("Booster Output", pf(cur_id, c, "vs_hi_en"), pf(sel_id, c, "vs_hi_en"), true));
            }
        }
        2 => {
            // ARMS (matches user's reference screenshot!)
            top.push(stat_i32("AP", pf(cur_id, c, "ap"), pf(sel_id, c, "ap"), true));
            top.push(stat_i32("KE Defense", pf(cur_id, c, "ke_def"), pf(sel_id, c, "ke_def"), true));
            top.push(stat_i32("CE Defense", pf(cur_id, c, "ce_def"), pf(sel_id, c, "ce_def"), true));
            top.push(stat_i32("TE Defense", pf(cur_id, c, "te_def"), pf(sel_id, c, "te_def"), true));
            if detail {
                top.push(stat_i32("Firing Stability", pf(cur_id, c, "shoot_stability"), pf(sel_id, c, "shoot_stability"), true));
                top.push(stat_i32("Shoulder Mounts", pf(cur_id, c, "shoulder_mount_count"), pf(sel_id, c, "shoulder_mount_count"), true));
            }
        }
        3 => {
            // LEGS
            top.push(stat_i32("AP", pf(cur_id, c, "ap"), pf(sel_id, c, "ap"), true));
            top.push(stat_i32("KE Defense", pf(cur_id, c, "ke_def"), pf(sel_id, c, "ke_def"), true));
            top.push(stat_i32("CE Defense", pf(cur_id, c, "ce_def"), pf(sel_id, c, "ce_def"), true));
            top.push(stat_i32("TE Defense", pf(cur_id, c, "te_def"), pf(sel_id, c, "te_def"), true));
            if detail {
                top.push(stat_i32("Load Capacity", pf(cur_id, c, "payload"), pf(sel_id, c, "payload"), true));
                top.push(stat_i32("Movement Perf.", pf(cur_id, c, "move_ability"), pf(sel_id, c, "move_ability"), true));
                top.push(stat_i32("Jump Perf.", pf(cur_id, c, "jump_ability"), pf(sel_id, c, "jump_ability"), true));
                top.push(stat_i32("Turning Perf.", pf(cur_id, c, "turn_ability"), pf(sel_id, c, "turn_ability"), true));
            }
        }
        4 => {
            // FCS
            top.push(stat_i32("Lock-On Range", pf(cur_id, c, "lock_range"), pf(sel_id, c, "lock_range"), true));
            top.push(stat_i32("Lock Compute", pf(cur_id, c, "lock_calc_speed"), pf(sel_id, c, "lock_calc_speed"), true));
            top.push(stat_i32("Missile Lock", pf(cur_id, c, "missile_lock_calc_speed"), pf(sel_id, c, "missile_lock_calc_speed"), true));
            top.push(stat_i32("Max Locks", pf(cur_id, c, "missile_lock_max"), pf(sel_id, c, "missile_lock_max"), true));
            if detail {
                top.push(stat_i32("Sight Width", pf(cur_id, c, "lock_sight_x"), pf(sel_id, c, "lock_sight_x"), true));
                top.push(stat_i32("Sight Height", pf(cur_id, c, "lock_sight_y"), pf(sel_id, c, "lock_sight_y"), true));
            }
        }
        5 => {
            // GENERATOR
            top.push(stat_i32("EN Capacity", pf(cur_id, c, "en_capacity"), pf(sel_id, c, "en_capacity"), true));
            top.push(stat_i32("EN Output", pf(cur_id, c, "en_output"), pf(sel_id, c, "en_output"), true));
            top.push(stat_i32("OW EN Output", pf(cur_id, c, "ow_en_output"), pf(sel_id, c, "ow_en_output"), true));
        }
        6 => {
            // BOOSTER
            top.push(stat_i32("Boost Output", pf(cur_id, c, "power"), pf(sel_id, c, "power"), true));
            top.push(stat_i32("HB Output", pf(cur_id, c, "hi_output"), pf(sel_id, c, "hi_output"), true));
            top.push(stat_i32("HB EN Drain", pf(cur_id, c, "hi_en"), pf(sel_id, c, "hi_en"), false));
            top.push(stat_f32("Burn Eff.", pf(cur_id, c, "std_burn_eff"), pf(sel_id, c, "std_burn_eff"), true));
            if detail {
                top.push(stat_f32("HB Burn Eff.", pf(cur_id, c, "hi_burn_eff"), pf(sel_id, c, "hi_burn_eff"), true));
                top.push(stat_i32("Charge Frames", pf(cur_id, c, "qb_charge_max_frames"), pf(sel_id, c, "qb_charge_max_frames"), false));
            }
        }
        10 | 11 | 12 => {
            // ARM UNITS / BAY UNITS / OVERED WEAPONS / SHOULDER UNITS
            top.push(stat_i32("Attack Power", pf(cur_id, c, "damage_power"), pf(sel_id, c, "damage_power"), true));
            top.push(stat_i32("Impact Force", pf(cur_id, c, "damage_recoil"), pf(sel_id, c, "damage_recoil"), true));
            top.push(stat_i32("Reload Time", pf(cur_id, c, "reload_time"), pf(sel_id, c, "reload_time"), false));
            top.push(stat_i32("Ammo", pf(cur_id, c, "magazine"), pf(sel_id, c, "magazine"), true));
            if detail {
                top.push(stat_i32("Opt. Range", pf(cur_id, c, "no_decay_range"), pf(sel_id, c, "no_decay_range"), true));
                top.push(stat_i32("Atten. Range", pf(cur_id, c, "damp_range"), pf(sel_id, c, "damp_range"), true));
                top.push(stat_i32("Muzzle Vel.", pf(cur_id, c, "init_speed"), pf(sel_id, c, "init_speed"), true));
                top.push(stat_i32("Firing EN", pf(cur_id, c, "en_drain"), pf(sel_id, c, "en_drain"), false));
                if c == 10 {
                    top.push(stat_i32("Charge Time", pf(cur_id, c, "charge_time"), pf(sel_id, c, "charge_time"), false));
                }
                top.push(stat_i32("Burst Count", pf(cur_id, c, "burst_count"), pf(sel_id, c, "burst_count"), true));
            }
        }
        13 => {
            // RECON
            top.push(stat_i32("Recon Type", pf(cur_id, c, "recon_subcategory"), pf(sel_id, c, "recon_subcategory"), true));
        }
        _ => {}
    }

    // Weight & Energy Consumption always sit at the bottom (`@Param0` / `@Param1` in `@PartsNormal` and `@PartsDetail`).
    top.push(stat_i32("Weight", pf(cur_id, c, "weight"), pf(sel_id, c, "weight"), false));
    top.push(stat_i32("Energy Consumption", pf(cur_id, c, "en_consumption"), pf(sel_id, c, "en_consumption"), false));
    top
}

/// Builds the 11 AC-wide spec comparison rows for `@SpecDetail` (`@Param0..@Param10`).
pub fn ac_spec_detail_rows(cur: &AcSpecs, preview: &AcSpecs) -> Vec<StatRow> {
    vec![
        stat_i32("AP", cur.ap as f64, preview.ap as f64, true),
        stat_i32("KE Defense", cur.ke_def as f64, preview.ke_def as f64, true),
        stat_i32("CE Defense", cur.ce_def as f64, preview.ce_def as f64, true),
        stat_i32("TE Defense", cur.te_def as f64, preview.te_def as f64, true),
        stat_i32("Total Weight", cur.total_weight as f64, preview.total_weight as f64, false),
        stat_i32("Load Capacity", cur.leg_payload as f64, preview.leg_payload as f64, true),
        stat_i32("EN Output", cur.en_output as f64, preview.en_output as f64, true),
        stat_i32("EN Recovery", cur.en_recovery as f64, preview.en_recovery as f64, true),
        stat_i32("Boost Speed", cur.boost_speed as f64, preview.boost_speed as f64, true),
        stat_i32("HB Speed", cur.hb_speed as f64, preview.hb_speed as f64, true),
        stat_i32("Glide Speed", cur.glide_speed as f64, preview.glide_speed as f64, true),
    ]
}

fn cmp_color(cmp: i8) -> Color {
    match cmp {
        1 => Color::srgb(0.32, 0.78, 1.0),  // ACVD color index 1003 (blue = better)
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
) -> bool {
    assets.ensure_loaded(&garage.disc, images);

    let scale = (window_size.x / SCREEN_W).min(window_size.y / SCREEN_H).max(0.5);
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
    let (help_line1, help_line2) = split_two_lines(&explain, 92);

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

    // 1. Tactical dark backdrop behind the left Assemble pane (`x = 0..695`) so the UI is crisp
    //    while keeping the right-side 3D AC Assembly View (`x = 705..1213`) open to the workshop.
    commands.spawn((
        px_rect(0.0, 0.0, 702.0, SCREEN_H),
        BackgroundColor(Color::srgba(0.03, 0.05, 0.07, 0.72)),
        ChildOf(screen_root),
    ));

    // 2. Real `@ModelBackImage` DRB dialog (authentic left/right corner-reticle frames).
    if let Some(layout) = assets.layout.as_ref() {
        if let Some(idx) = layout.drb.dialogs.iter().position(|d| d.name == "@ModelBackImage") {
            let dlg = menu::spawn(commands, layout, idx, Vec2::ZERO, scale);
            commands.entity(dlg).remove::<MenuRoot>().insert(ChildOf(screen_root));
        }
    }

    // 3. Normal View vs Detail View (`Tab` toggles `state.assemble_detail_view`).
    if !state.assemble_detail_view {
        // 3a. Dark blueprint backing behind the left `@PartsNormal` panel (`[173,220,686,567]`)
        //     so stat text and the 3D part preview stand out cleanly over the 3D workshop.
        commands.spawn((
            px_rect(173.0, 220.0, 686.0, 567.0),
            BackgroundColor(Color::srgba(0.03, 0.05, 0.08, 0.78)),
            ChildOf(screen_root),
        ));
        // Live 3D Part/Weapon Preview in the upper clear area of `@PartsLayout` (`[178,231,676,412]`).
        let preview_box = commands
            .spawn((
                px_rect(178.0, 231.0, 676.0, 412.0),
                BackgroundColor(Color::srgba(0.05, 0.08, 0.12, 0.45)),
                BorderColor::all(Color::srgba(0.32, 0.45, 0.52, 0.45)),
                ChildOf(screen_root),
            ))
            .id();
        if let Some(ref preview_img) = assets.preview_image {
            if sel_id > 0 {
                commands.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: px(0.0),
                        top: px(0.0),
                        width: px(498.0 * scale),
                        height: px(181.0 * scale),
                        ..default()
                    },
                    ImageNode::new(preview_img.clone()),
                    ChildOf(preview_box),
                ));
            }
        }

        // 3b. Real `@PartsNormal` DRB dialog from `lang/en/menu/assemble.drb.dcx`!
        // We spawn it as a child of `screen_root`, then immediately patch its runtime `MenuText`
        // values and dynamic gauges via `AssembleDrbSync` (or a one-shot marker)!
        if let Some(layout) = assets.layout.as_ref() {
            if let Some(idx) = layout.drb.dialogs.iter().position(|d| d.name == "@PartsNormal") {
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
            commands
                .spawn((
                    px_rect(460.0, 236.0, 526.0, 260.0),
                    BackgroundColor(Color::srgba(0.06, 0.08, 0.11, 0.90)),
                    BorderColor::all(dcol),
                    ChildOf(screen_root),
                ))
                .with_children(|b| {
                    b.spawn((
                        Node {
                            margin: UiRect::all(Val::Auto),
                            ..default()
                        },
                        Text::new(format!("{dtag} DMG")),
                        TextFont {
                            font_size: FontSize::Px(11.0 * scale),
                            ..default()
                        },
                        TextColor(dcol),
                    ));
                });
        }
    } else {
        // Detail View (`@PartsDetail` + `@SpecDetail` from `lang/en/menu/assemble.drb.dcx`).
        if let Some(layout) = assets.layout.as_ref() {
            if let Some(idx) = layout.drb.dialogs.iter().position(|d| d.name == "@PartsDetail") {
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
            if let Some(idx) = layout.drb.dialogs.iter().position(|d| d.name == "@SpecDetail") {
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

    // 4. Top Bar (`y = 14..52`): Title + Retail-style button prompts.
    let top_bar = commands
        .spawn((
            px_rect(18.0, 14.0, 1262.0, 50.0),
            BackgroundColor(Color::srgba(0.04, 0.06, 0.09, 0.90)),
            BorderColor::all(Color::srgba(0.32, 0.44, 0.52, 0.55)),
            ChildOf(screen_root),
        ))
        .id();
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: px(16.0 * scale),
            top: px(8.0 * scale),
            ..default()
        },
        Text::new(format!("ASSEMBLE  //  {}", slot.label())),
        TextFont {
            font_size: FontSize::Px(16.0 * scale),
            ..default()
        },
        TextColor(Color::srgb(0.92, 0.36, 0.28)),
        ChildOf(top_bar),
    ));

    // Toggle Display button (`Tab` -> Normal / Detail Specs).
    commands
        .spawn((
            Button,
            UiAction::AssembleToggleDetail,
            Node {
                position_type: PositionType::Absolute,
                right: px(120.0 * scale),
                top: px(5.0 * scale),
                padding: UiRect::axes(px(10.0 * scale), px(4.0 * scale)),
                border: UiRect::all(px(1.0)),
                ..default()
            },
            BackgroundColor(if state.assemble_detail_view {
                Color::srgba(0.18, 0.44, 0.56, 0.92)
            } else {
                Color::srgba(0.08, 0.12, 0.17, 0.90)
            }),
            BorderColor::all(Color::srgba(0.45, 0.72, 0.84, 0.70)),
            ChildOf(top_bar),
        ))
        .with_children(|b| {
            b.spawn((
                Text::new(if state.assemble_detail_view {
                    "[Tab] Normal View"
                } else {
                    "[Tab] Detail Specs"
                }),
                TextFont {
                    font_size: FontSize::Px(12.0 * scale),
                    ..default()
                },
                TextColor(Color::srgb(0.92, 0.95, 0.98)),
            ));
        });

    // Back button (`Esc`).
    commands
        .spawn((
            Button,
            UiAction::Back,
            Node {
                position_type: PositionType::Absolute,
                right: px(12.0 * scale),
                top: px(5.0 * scale),
                padding: UiRect::axes(px(10.0 * scale), px(4.0 * scale)),
                border: UiRect::all(px(1.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.12, 0.10, 0.12, 0.90)),
            BorderColor::all(Color::srgba(0.75, 0.38, 0.32, 0.70)),
            ChildOf(top_bar),
        ))
        .with_children(|b| {
            b.spawn((
                Text::new("[Esc] Back"),
                TextFont {
                    font_size: FontSize::Px(12.0 * scale),
                    ..default()
                },
                TextColor(Color::srgb(0.96, 0.85, 0.80)),
            ));
        });

    // 5. Subcategory / Weapon-Type & Damage-Type Filter Tabs (`y = 66..106`, right above `@CategoryName`).
    let subcats = subcategories_for_slot(slot, assets.subcat_fmg.as_ref());
    let filter_row = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(84.0 * scale),
                top: px(64.0 * scale),
                width: px(1130.0 * scale),
                height: px(42.0 * scale),
                flex_direction: FlexDirection::Row,
                flex_wrap: FlexWrap::Wrap,
                align_items: AlignItems::Center,
                column_gap: px(4.0 * scale),
                row_gap: px(3.0 * scale),
                ..default()
            },
            ChildOf(screen_root),
        ))
        .id();

    // Damage type filter chips for weapons (`ALL | KE | CE | TE`).
    if slot_has_damage_filter(slot) {
        let dmg_opts: [(Option<u8>, &str, Color); 4] = [
            (None, "ALL DMG", Color::srgb(0.82, 0.88, 0.94)),
            (Some(0), "KE", Color::srgb(0.95, 0.72, 0.24)),
            (Some(6), "CE", Color::srgb(0.32, 0.88, 0.45)),
            (Some(1), "TE", Color::srgb(0.32, 0.76, 0.98)),
        ];
        for (val, lbl, col) in dmg_opts {
            let active = state.assemble_dmg_filter == val;
            commands
                .spawn((
                    Button,
                    UiAction::AssembleDmgFilter(val),
                    Node {
                        padding: UiRect::axes(px(7.0 * scale), px(2.0 * scale)),
                        border: UiRect::all(px(1.0)),
                        ..default()
                    },
                    BackgroundColor(if active {
                        Color::srgba(0.18, 0.34, 0.42, 0.96)
                    } else {
                        Color::srgba(0.06, 0.08, 0.12, 0.86)
                    }),
                    BorderColor::all(if active {
                        col
                    } else {
                        Color::srgba(0.32, 0.42, 0.48, 0.55)
                    }),
                    ChildOf(filter_row),
                ))
                .with_children(|b| {
                    b.spawn((
                        Text::new(lbl),
                        TextFont {
                            font_size: FontSize::Px(10.5 * scale),
                            ..default()
                        },
                        TextColor(if active { Color::WHITE } else { col }),
                    ));
                });
        }
    }

    // Subcategory / weapon-type tabs (from `partssubcategory_en.fmg`).
    if !subcats.is_empty() {
        let all_active = state.assemble_subcat.is_none();
        commands
            .spawn((
                Button,
                UiAction::AssembleSubcat(None),
                Node {
                    padding: UiRect::axes(px(7.0 * scale), px(2.0 * scale)),
                    border: UiRect::all(px(1.0)),
                    ..default()
                },
                BackgroundColor(if all_active {
                    Color::srgba(0.14, 0.56, 0.46, 0.94)
                } else {
                    Color::srgba(0.06, 0.09, 0.13, 0.86)
                }),
                BorderColor::all(if all_active {
                    Color::srgb(0.35, 0.95, 0.76)
                } else {
                    Color::srgba(0.30, 0.40, 0.46, 0.55)
                }),
                ChildOf(filter_row),
            ))
            .with_children(|b| {
                b.spawn((
                    Text::new("ALL TYPES"),
                    TextFont {
                        font_size: FontSize::Px(10.0 * scale),
                        ..default()
                    },
                    TextColor(if all_active {
                        Color::WHITE
                    } else {
                        Color::srgb(0.75, 0.82, 0.88)
                    }),
                ));
            });

        // Show up to 11 subcategory chips (cycle any beyond with Q/E!).
        let max_chips = if slot_has_damage_filter(slot) { 10 } else { 12 };
        let active_pos = state
            .assemble_subcat
            .and_then(|s| subcats.iter().position(|(id, _)| *id == s))
            .unwrap_or(0);
        let start_chip = active_pos.saturating_sub(max_chips / 2).min(subcats.len().saturating_sub(max_chips));
        let end_chip = (start_chip + max_chips).min(subcats.len());
        for &(sid, ref sname) in &subcats[start_chip..end_chip] {
            let active = state.assemble_subcat == Some(sid);
            commands
                .spawn((
                    Button,
                    UiAction::AssembleSubcat(Some(sid)),
                    Node {
                        padding: UiRect::axes(px(6.0 * scale), px(2.0 * scale)),
                        border: UiRect::all(px(1.0)),
                        ..default()
                    },
                    BackgroundColor(if active {
                        Color::srgba(0.14, 0.56, 0.46, 0.94)
                    } else {
                        Color::srgba(0.06, 0.09, 0.13, 0.86)
                    }),
                    BorderColor::all(if active {
                        Color::srgb(0.35, 0.95, 0.76)
                    } else {
                        Color::srgba(0.30, 0.40, 0.46, 0.55)
                    }),
                    ChildOf(filter_row),
                ))
                .with_children(|b| {
                    b.spawn((
                        Text::new(short_subcat_label(sname)),
                        TextFont {
                            font_size: FontSize::Px(10.0 * scale),
                            ..default()
                        },
                        TextColor(if active {
                            Color::WHITE
                        } else {
                            Color::srgb(0.75, 0.82, 0.88)
                        }),
                    ));
                });
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
                px_rect(x0, 148.0, x0 + 84.0, 202.0),
                BackgroundColor(if is_cur {
                    Color::srgba(0.12, 0.46, 0.36, 0.92)
                } else {
                    Color::srgba(0.05, 0.07, 0.10, 0.88)
                }),
                BorderColor::all(if is_cur {
                    Color::srgb(0.32, 0.98, 0.75)
                } else {
                    Color::srgba(0.45, 0.52, 0.58, 0.65)
                }),
                ChildOf(screen_root),
            ))
            .id();
        commands.entity(tab).insert(Node {
            position_type: PositionType::Absolute,
            left: px(x0 * scale),
            top: px(148.0 * scale),
            width: px(84.0 * scale),
            height: px(54.0 * scale),
            border: UiRect::all(px(if is_cur { 2.0 } else { 1.0 })),
            overflow: Overflow::clip(),
            ..default()
        });
        if let Some(img) = thumb {
            commands.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: px(-15.0 * scale),
                    top: px(-2.0 * scale),
                    width: px(112.0 * scale),
                    height: px(56.0 * scale),
                    ..default()
                },
                ImageNode {
                    image: img,
                    color: if is_cur {
                        Color::srgb(0.80, 1.0, 0.92)
                    } else {
                        Color::srgb(0.85, 0.88, 0.90)
                    },
                    ..default()
                },
                ChildOf(tab),
            ));
        }
        // Compact slot tag at top-left of the tab.
        commands.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(3.0 * scale),
                top: px(2.0 * scale),
                ..default()
            },
            Text::new(short_slot_tag(tab_slot)),
            TextFont {
                font_size: FontSize::Px(9.0 * scale),
                ..default()
            },
            TextColor(if is_cur {
                Color::srgb(0.55, 1.0, 0.84)
            } else {
                Color::srgba(0.85, 0.90, 0.95, 0.85)
            }),
            ChildOf(tab),
        ));
    }

    // 7. Left Vertical Part Thumbnail Strip (`@PartsList` `[86, 220, 170, 562]`, 6 × `84×54`).
    //    Displays the real disc picture (`image/partsthumbnail/<dir>/<prefix><model_id:04>.tpf.dcx`)
    //    for every part in the filtered list!
    let list_start = state.scroll.min(parts.len().saturating_sub(1));
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
                    left: px(86.0 * scale),
                    top: px(y0 * scale),
                    width: px(84.0 * scale),
                    height: px(54.0 * scale),
                    border: UiRect::all(px(if is_sel { 2.0 } else { 1.0 })),
                    overflow: Overflow::clip(),
                    ..default()
                },
                BackgroundColor(if is_sel {
                    Color::srgba(0.14, 0.44, 0.36, 0.92)
                } else {
                    Color::srgba(0.05, 0.07, 0.10, 0.88)
                }),
                BorderColor::all(if is_sel {
                    Color::srgb(0.95, 0.68, 0.22)
                } else {
                    Color::srgba(0.45, 0.52, 0.58, 0.65)
                }),
                ChildOf(screen_root),
            ))
            .id();

        if let Some(img) = thumb {
            commands.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: px(-15.0 * scale),
                    top: px(-2.0 * scale),
                    width: px(112.0 * scale),
                    height: px(56.0 * scale),
                    ..default()
                },
                ImageNode {
                    image: img,
                    color: if is_sel {
                        Color::srgb(0.88, 1.0, 0.94)
                    } else {
                        Color::WHITE
                    },
                    ..default()
                },
                ChildOf(item),
            ));
        }

        // Equipped badge (`E`, matching `@LogoEquip`).
        if is_eq {
            commands
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: px(3.0 * scale),
                        top: px(2.0 * scale),
                        padding: UiRect::axes(px(3.0 * scale), px(0.5 * scale)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.05, 0.12, 0.16, 0.88)),
                    ChildOf(item),
                ))
                .with_children(|b| {
                    b.spawn((
                        Text::new("E"),
                        TextFont {
                            font_size: FontSize::Px(10.0 * scale),
                            ..default()
                        },
                        TextColor(Color::srgb(0.32, 0.98, 0.72)),
                    ));
                });
        }

        // Damage-type pip in bottom-right corner for weapons.
        if let Some((_, dtag, dcol)) = part_damage_badge(cat, pid) {
            commands
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        right: px(3.0 * scale),
                        bottom: px(2.0 * scale),
                        padding: UiRect::axes(px(3.0 * scale), px(0.5 * scale)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.04, 0.06, 0.09, 0.88)),
                    ChildOf(item),
                ))
                .with_children(|b| {
                    b.spawn((
                        Text::new(dtag),
                        TextFont {
                            font_size: FontSize::Px(9.0 * scale),
                            ..default()
                        },
                        TextColor(dcol),
                    ));
                });
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

    // 8. Footer Controls Strip (`y = 668..706`).
    let footer = commands
        .spawn((
            px_rect(18.0, 668.0, 1262.0, 704.0),
            BackgroundColor(Color::srgba(0.04, 0.06, 0.09, 0.88)),
            BorderColor::all(Color::srgba(0.30, 0.42, 0.48, 0.45)),
            ChildOf(screen_root),
        ))
        .id();
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
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: px(16.0 * scale),
            top: px(9.0 * scale),
            ..default()
        },
        Text::new(format!(
            "Select {slot_prompt}.  ({} / {})   |   W/S: Part   A/D: Slot Tab   Q/E: Subcategory   Tab: Detail Specs   Enter: Equip   Right-Drag: Orbit AC   Esc: Back",
            if parts.is_empty() { 0 } else { state.cursor + 1 },
            parts.len()
        )),
        TextFont {
            font_size: FontSize::Px(12.0 * scale),
            ..default()
        },
        TextColor(Color::srgb(0.86, 0.56, 0.48)),
        ChildOf(footer),
    ));

    // Transient banner (e.g. "EQUIPPED ...", "OVERWEIGHT!").
    if let Some((ref msg, col, _)) = state.banner {
        commands
            .spawn((
                px_rect(710.0, 66.0, 1208.0, 96.0),
                BackgroundColor(Color::srgba(0.05, 0.07, 0.10, 0.92)),
                BorderColor::all(col),
                ChildOf(screen_root),
            ))
            .with_children(|b| {
                b.spawn((
                    Node {
                        margin: UiRect::all(Val::Auto),
                        ..default()
                    },
                    Text::new(msg.clone()),
                    TextFont {
                        font_size: FontSize::Px(13.0 * scale),
                        ..default()
                    },
                    TextColor(col),
                ));
            });
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

fn split_two_lines(s: &str, max_line: usize) -> (String, String) {
    if s.chars().count() <= max_line {
        return (s.to_string(), String::new());
    }
    let mut split_at = max_line;
    for (idx, ch) in s.char_indices() {
        if idx > max_line {
            break;
        }
        if ch == ' ' {
            split_at = idx;
        }
    }
    let l1 = s[..split_at].trim().to_string();
    let l2: String = s[split_at..].trim().chars().take(max_line).collect();
    (l1, l2)
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
        let top_stats = if n >= 2 { &data.stats[..n - 2] } else { &[][..] };
        let bottom_stats = if n >= 2 { &data.stats[n - 2..] } else { &data.stats[..] };

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
        let top_stats = if n >= 2 { &data.stats[..n - 2] } else { &[][..] };
        let bottom_stats = if n >= 2 { &data.stats[n - 2..] } else { &data.stats[..] };
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
                    fill_param_dialog(child, data.stats.get(idx), &children_q, &names_q, &mut texts_q);
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
    mut assets: ResMut<AssembleUiAssets>,
    mut ac_packs: ResMut<crate::AcTexturePacks>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut std_mats: ResMut<Assets<StandardMaterial>>,
    mut ac_mats: ResMut<Assets<paint::AcPaintMaterial>>,
    mut images: ResMut<Assets<Image>>,
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
        }
        return;
    };

    assets.ensure_loaded(&garage.disc, &mut images);

    // Spawn the isolated offscreen studio camera & 3-point lights once.
    if studio_q.is_empty() {
        if let Some(ref target_img) = assets.preview_image {
            let layer = RenderLayers::layer(PART_PREVIEW_LAYER);
            commands.spawn((
                PartPreviewStudio,
                PartPreviewCamera,
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
                Transform::from_xyz(0.0, 0.55, 3.15).looking_at(Vec3::ZERO, Vec3::Y),
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
    let want = (cat, sel_id, is_left, color_key);
    if assets.preview_loaded == Some(want) {
        assets.preview_pending = None;
        return;
    }

    // If not already cached (and not the very first preview frame or empty slot), debounce
    // disc FLVER loading by 90 ms so rapid W/S scrolling stays 60 FPS.
    let is_cached = sel_id == 0 || assets.preview_cache.contains_key(&want);
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

    for e in &mesh_q {
        commands.entity(e).despawn();
    }
    for e in &pivot_q {
        commands.entity(e).despawn();
    }
    if sel_id == 0 {
        return;
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

        let Ok(loaded) = acvd_render::model(&garage.disc, asset_path) else {
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
                &mut ac_packs.0,
                &mut images,
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
                    &mut ac_packs.0,
                    &mut images,
                )
            });
            let mesh_h = meshes.add(lm.mesh);
            let child_xform = Transform {
                translation: -center * fit_scale,
                rotation: Quat::IDENTITY,
                scale: Vec3::splat(fit_scale),
            };
            let mat = if let Some(mat) = painted {
                PreviewMat::Painted(ac_mats.add(mat))
            } else {
                PreviewMat::Std(std_mats.add(base))
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

    let layer = RenderLayers::layer(PART_PREVIEW_LAYER);
    let pivot = commands
        .spawn((
            PartPreviewPivot,
            Transform::from_rotation(Quat::from_rotation_y(assets.preview_yaw)),
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

/// Gently rotates the 3D part/weapon preview on its studio turntable.
pub fn rotate_part_preview(
    time: Res<Time>,
    mut assets: ResMut<AssembleUiAssets>,
    mut pivots: Query<&mut Transform, With<PartPreviewPivot>>,
) {
    if pivots.is_empty() {
        return;
    }
    assets.preview_yaw = (assets.preview_yaw + time.delta_secs() * 0.55) % std::f32::consts::TAU;
    for mut tf in &mut pivots {
        tf.rotation = Quat::from_rotation_y(assets.preview_yaw);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acvd_formats::vfs;
    use std::path::PathBuf;

    #[test]
    fn part_thumbnails_subcategories_and_descriptions_resolve_from_disc() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let disc = Disc::open(&vfs::default_disc(&root)).expect("open 360 disc");
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
        assert!(arm_subs.len() >= 4, "arms should have 4 subcategories: {arm_subs:?}");
        assert_eq!(subcategory_name(Some(&sub_fmg), 2, 1), "KE-RESISTANT TYPE");
        assert_eq!(subcategory_name(Some(&sub_fmg), 2, 4), "WEAPON ARMS");

        // Arm Unit weapon types & KE/CE/TE damage filtering.
        let wep_subs = subcategories_for_slot(AssembleSlot::ArmUnitR, Some(&sub_fmg));
        assert!(wep_subs.len() >= 20, "arm units should have 20+ weapon types");
        let ke_weps = filtered_parts_for_slot(AssembleSlot::ArmUnitR, None, Some(0));
        let ce_weps = filtered_parts_for_slot(AssembleSlot::ArmUnitR, None, Some(6));
        let te_weps = filtered_parts_for_slot(AssembleSlot::ArmUnitR, None, Some(1));
        assert!(!ke_weps.is_empty() && !ce_weps.is_empty() && !te_weps.is_empty());

        // Real part description resolves from partsexplain_en.fmg.
        let desc = part_description(Some(&exp_fmg), 2, 110);
        assert!(!desc.is_empty(), "arms part 110 should have an English description");

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
}
