//! 360-accurate AC Decal System (`g_AC_DecalMap` `s3` on UV1) with custom image upload and
//! direct 3D click-and-drag placement in the Garage.
//!
//! ## Xbox 360 (`ACV2.pe`) findings
//! - **5 Decal Slots (`0x82464ac8`, `0x82022678` / `0x82464a98`)**:
//!   - `0` Head (`512×512`)
//!   - `1` Core (`1024×1024`)
//!   - `2` Right Arm (`1024×1024`, `arms` split by `r_kata` root via `0x82b4d618`)
//!   - `3` Left Arm (`1024×1024`, `arms` split by `l_kata` root via `0x82b4d618`)
//!   - `4` Legs (`1024×1024`)
//! - **Per-slot base resolutions (`0x8202268c` via `0x82464b38`, allocated by `0x82b4d298`)**:
//!   `[512, 1024, 1024, 1024, 1024]`, with up to `32` (`0x20`) decals per slot (`0x82b51668`,
//!   `0x400` bytes = `32 × 32`-byte entries starting at `+0x4b60`).
//! - **3D Projection Baking (`Flver_ACPartsDecalPaint.vpo` / `.fpo`, `0x82b50b10` / `0x824658c0`)**:
//!   rasterizes each part's mesh into UV1 space (`[0, 1] × [0, 1]`), projects each surface point
//!   through the decal's 3D projector with depth & facing rejection, tints by `DL_FREG_181`, and
//!   blends into the part's `g_AC_DecalMap` (`0x82b4ca00`).
//! - **Paint Shader Composite (`Flver_ACColorDecalColDifSpcBumpIbl.fpo` instructions 107, 110,
//!   131–132)**: samples `g_AC_DecalMap` (`s3`) at UV1 (`r5.zw`) and composites premultiplied sRGB
//!   over the painted diffuse: `final_srgb = painted * (1.0 - decal.a) + decal.rgb`.
//! - **Built-in Retail Emblems (`image/emblem/im*.tpf`, `0x823861d0`)**: 306 retail 256×256 BC3
//!   emblems on the 360 disc, browsable alongside user-uploaded PNG/JPG/WebP/BMP/GIF images.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};

use acvd_formats::tpf;
use acvd_formats::vfs::Disc;
use anyhow::{bail, Context, Result};
use bevy::asset::RenderAssetUsages;
use bevy::ecs::system::SystemParam;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::mesh::{Indices, VertexAttributeValues};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::window::{FileDragAndDrop, PrimaryWindow};
use serde::{Deserialize, Serialize};

use crate::garage::{
    GarageScreen, GarageState, Mode, PaintTab, CUE_CANCEL, CUE_EQUIP, CUE_ERROR, CUE_MOVE, CUE_OK,
};
use crate::sound::{self, Cues};

/// Maximum decals per part slot (`0x82b51668`: `0x20` entries × `0x20` bytes = `0x400`).
pub const MAX_DECALS_PER_SLOT: usize = 32;

/// Interactive drag preview resolution for instant 60 FPS live dragging on CPU.
const DRAG_PREVIEW_RES: u32 = 256;

/// Maximum pixel dimension for uploaded user images (downscaled cleanly preserving aspect).
const MAX_IMPORT_DIM: u32 = 1024;

/// Local user directory (gitignored) for uploaded decal images and saved design layouts.
pub const USER_DECAL_DIR: &str = "decals";

/// Curated tint swatches for decals (`DL_FREG_181` in `Flver_ACPartsDecalPaint.fpo`).
pub const DECAL_TINTS: &[(&str, [u8; 4])] = &[
    ("ORIGINAL", [255, 255, 255, 255]),
    ("WHITE", [242, 244, 248, 255]),
    ("BLACK", [28, 30, 34, 255]),
    ("CRIMSON", [220, 48, 42, 255]),
    ("AMBER", [245, 178, 42, 255]),
    ("COBALT", [54, 132, 232, 255]),
    ("EMERALD", [58, 198, 118, 255]),
    ("CYAN", [72, 218, 235, 255]),
];

/// The 5 AC frame decal slots (`0x82464ac8`, `0x82022678`, `0x8202268c`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DecalSlot {
    Head = 0,
    Core = 1,
    RightArm = 2,
    LeftArm = 3,
    Legs = 4,
}

impl DecalSlot {
    pub const ALL: [Self; 5] = [
        Self::Head,
        Self::Core,
        Self::RightArm,
        Self::LeftArm,
        Self::Legs,
    ];

    pub const fn index(self) -> usize {
        self as usize
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Head => "HEAD",
            Self::Core => "CORE",
            Self::RightArm => "R-ARM",
            Self::LeftArm => "L-ARM",
            Self::Legs => "LEGS",
        }
    }

    /// Base 360 `g_AC_DecalMap` resolution (`0x8202268c` / `0x82464b38`).
    pub const fn full_resolution(self) -> u32 {
        match self {
            Self::Head => 512,
            Self::Core | Self::RightArm | Self::LeftArm | Self::Legs => 1024,
        }
    }

    /// Maps an assembled AC part column + mesh root bone to its 360 decal slot (`0x82464ac8` +
    /// `0x82b4d618` splitting shared `arms` FLVER by `r_kata` / `l_kata`).
    pub fn from_mesh(column: &str, root: Option<&str>) -> Option<Self> {
        match column {
            "head" => Some(Self::Head),
            "core" => Some(Self::Core),
            "arms" => {
                let is_left = root.is_some_and(|r| {
                    let lo = r.to_ascii_lowercase();
                    lo.starts_with("l_") || lo.contains("left")
                });
                if is_left {
                    Some(Self::LeftArm)
                } else {
                    Some(Self::RightArm)
                }
            }
            "legs" => Some(Self::Legs),
            _ => None,
        }
    }

    /// Opposite slot for symmetric `mirror_x` projection.
    pub const fn mirrored_slot(self) -> Self {
        match self {
            Self::RightArm => Self::LeftArm,
            Self::LeftArm => Self::RightArm,
            other => other,
        }
    }
}

/// Which image tab is active in the Decal library panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecalLibraryTab {
    Custom,
    Emblems,
}

/// Decoded RGBA8 decal image ready for CPU UV1 projection and UI thumbnail display.
#[derive(Clone)]
pub struct DecalAsset {
    /// Stable identifier persisted in saved layouts (`custom:<filename>` or `emblem:<name>`).
    pub key: String,
    pub label: String,
    pub width: u32,
    pub height: u32,
    /// Straight-alpha sRGB RGBA8 pixels (`width * height * 4`).
    pub rgba: Arc<Vec<u8>>,
    /// Thumbnail / preview `Image` handle for Bevy UI.
    pub thumb: Handle<Image>,
}

impl DecalAsset {
    pub fn aspect(&self) -> f32 {
        (self.width as f32 / self.height.max(1) as f32).clamp(0.15, 6.0)
    }
}

/// Reference to one of the 306 built-in 360 emblems (`image/emblem/im*.tpf`).
#[derive(Clone, Debug)]
pub struct DiscEmblemEntry {
    pub key: String,
    pub name: String,
    pub pack: &'static str,
}

/// One placed 3D projected decal layer on the AC (editable, draggable, serializable).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlacedDecal {
    pub id: u64,
    pub slot: DecalSlot,
    /// Key matching [`DecalAsset::key`] (`custom:<file>` or `emblem:<imXXXX>`).
    pub image_key: String,
    /// Rest-pose anchor point on the part surface.
    pub position: [f32; 3],
    /// Rest-pose outward projector normal (points away from the surface).
    pub normal: [f32; 3],
    /// Rest-pose base tangent (right axis before rotation).
    pub tangent: [f32; 3],
    /// World-space decal height in meters (width = `scale * aspect`).
    pub scale: f32,
    /// Rotation around `normal` in radians.
    pub rotation: f32,
    /// Projection half-depth along `normal` in meters (prevents bleeding onto far panels).
    pub depth: f32,
    /// Overall opacity `[0.0, 1.0]`.
    pub opacity: f32,
    /// Tint color RGBA (`DL_FREG_181`).
    pub tint: [u8; 4],
    /// When true, also projects a symmetric X-mirrored copy (`Head`/`Core`/`Legs` or opposite Arm).
    pub mirror_x: bool,
    /// Horizontal flip of the source image.
    pub flip_h: bool,
}

impl PlacedDecal {
    fn pos_vec(&self) -> Vec3 {
        Vec3::from(self.position)
    }
    fn norm_vec(&self) -> Vec3 {
        Vec3::from(self.normal).normalize_or(Vec3::Z)
    }
    fn tan_vec(&self) -> Vec3 {
        Vec3::from(self.tangent)
    }
}

#[derive(Serialize, Deserialize)]
struct SavedDesignDecals {
    version: u32,
    decals: Vec<PlacedDecal>,
}

/// Cached rest-pose + skinning data for one AC mesh so we can raycast against the live animated AC
/// and project decals cleanly into rest-pose UV1 space.
pub struct CachedPartMesh {
    pub slot: DecalSlot,
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub uv1: Vec<Vec2>,
    pub indices: Vec<u32>,
    pub joint_indices: Option<Vec<[u16; 4]>>,
    pub joint_weights: Option<Vec<[f32; 4]>>,
    pub joints: Vec<Entity>,
    pub inverse_bindposes: Vec<Mat4>,
}

impl CachedPartMesh {
    /// Extracts geometry and skinning info from a Bevy `Mesh` before it is added to `Assets<Mesh>`.
    pub fn from_mesh(
        slot: DecalSlot,
        mesh: &Mesh,
        joints: Vec<Entity>,
        inverse_bindposes: Vec<Mat4>,
    ) -> Option<Self> {
        let positions: Vec<Vec3> = match mesh.attribute(Mesh::ATTRIBUTE_POSITION)? {
            VertexAttributeValues::Float32x3(v) => v.iter().copied().map(Vec3::from).collect(),
            _ => return None,
        };
        let normals: Vec<Vec3> = match mesh.attribute(Mesh::ATTRIBUTE_NORMAL) {
            Some(VertexAttributeValues::Float32x3(v)) if v.len() == positions.len() => {
                v.iter().copied().map(Vec3::from).collect()
            }
            _ => vec![Vec3::Y; positions.len()],
        };
        // Exact 360 shader parity (`Flver_ACColorDecalColDifSpcBumpIbl.fpo` r5.zw = UV1), falling
        // back to UV0 identically to `ac_paint.wgsl` when a submesh lacks ATTRIBUTE_UV_1.
        let uv1: Vec<Vec2> = match mesh
            .attribute(Mesh::ATTRIBUTE_UV_1)
            .or_else(|| mesh.attribute(Mesh::ATTRIBUTE_UV_0))
        {
            Some(VertexAttributeValues::Float32x2(v)) if v.len() == positions.len() => {
                v.iter().copied().map(Vec2::from).collect()
            }
            _ => vec![Vec2::ZERO; positions.len()],
        };
        let indices: Vec<u32> = match mesh.indices()? {
            Indices::U32(v) => v.clone(),
            Indices::U16(v) => v.iter().map(|&i| i as u32).collect(),
        };
        let joint_indices = match mesh.attribute(Mesh::ATTRIBUTE_JOINT_INDEX) {
            Some(VertexAttributeValues::Uint16x4(v)) if v.len() == positions.len() => {
                Some(v.clone())
            }
            _ => None,
        };
        let joint_weights = match mesh.attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT) {
            Some(VertexAttributeValues::Float32x4(v)) if v.len() == positions.len() => {
                Some(v.clone())
            }
            _ => None,
        };
        Some(Self {
            slot,
            positions,
            normals,
            uv1,
            indices,
            joint_indices,
            joint_weights,
            joints,
            inverse_bindposes,
        })
    }
}

/// Cached frame meshes of the active player AC, rebuilt whenever `main::show` spawns the AC.
#[derive(Resource, Default)]
pub struct DecalMeshCache {
    pub meshes: Vec<CachedPartMesh>,
}

/// Persistent Decal Editor & Runtime State (shared seamlessly between Garage and AC TEST Sortie).
#[derive(Resource)]
pub struct DecalState {
    /// Per-slot GPU `g_AC_DecalMap` handles (`0..5`, bound into `AcPaint::decal_map`).
    pub slot_textures: [Handle<Image>; 5],
    /// Dirty flags per slot (`Some(res)` => needs rebake at `res × res`).
    pub dirty_slots: [Option<u32>; 5],
    /// Placed decal layers across the current AC (ordered bottom -> top).
    pub layers: Vec<PlacedDecal>,
    pub next_id: u64,
    /// Currently selected placed decal ID (for dragging / tweaking / deleting).
    pub selected_id: Option<u64>,
    /// Active drag state (`true` while left-dragging a decal across the AC surface).
    pub dragging: bool,
    /// When true, the next click on the AC always creates a new decal instead of picking an
    /// existing one under the cursor.
    pub place_new_mode: bool,
    /// Active brush image key (`custom:...` or `emblem:...`).
    pub active_image_key: Option<String>,
    /// Active library tab in the Garage Decal panel (`Custom` vs `360 Emblems`).
    pub library_tab: DecalLibraryTab,
    /// Scroll page in the emblem/custom thumbnail grid.
    pub library_page: usize,
    /// Loaded custom user images (`decals/` folder + uploaded files).
    pub custom_assets: Vec<DecalAsset>,
    /// Catalog of all 306 retail 360 emblems (`image/emblem/im*.tpf`).
    pub disc_emblems: Vec<DiscEmblemEntry>,
    /// On-demand decoded cache of retail 360 emblems (`key -> DecalAsset`).
    pub emblem_cache: HashMap<String, DecalAsset>,
    /// Default scale/mirror/flip/tint for newly stamped decals.
    pub brush_scale: f32,
    pub brush_mirror_x: bool,
    pub brush_flip_h: bool,
    pub brush_tint: [u8; 4],
    /// Currently loaded design ID (used for auto-saving/loading per-design decal layouts).
    pub loaded_design_id: Option<u32>,
    /// Optional CLI `--decal <path>` queued to auto-stamp on first AC spawn (for testing / `--shot`).
    pub cli_decal: Option<PathBuf>,
    /// True once GPU slot textures and user `decals/` folder have been initialized.
    initialized: bool,
    /// Pending async result from the native OS file picker thread.
    picker_rx: Option<Mutex<mpsc::Receiver<Option<PathBuf>>>>,
    /// Last hover hit on the AC for rendering the 3D surface gizmo.
    pub last_hover: Option<SurfaceHit>,
}

/// Raycast result against the live skinned AC mesh.
#[derive(Clone, Copy, Debug)]
pub struct SurfaceHit {
    pub slot: DecalSlot,
    pub world_pos: Vec3,
    pub world_normal: Vec3,
    pub world_tangent: Vec3,
    pub rest_pos: Vec3,
    pub rest_normal: Vec3,
    pub rest_tangent: Vec3,
    pub distance: f32,
}

impl DecalState {
    pub fn new(cli_decal: Option<PathBuf>) -> Self {
        let mut disc_emblems: Vec<DiscEmblemEntry> = acvd_data::generated::textures::IMAGE
            .iter()
            .filter(|t| t.pack.starts_with("image/emblem/"))
            .map(|t| DiscEmblemEntry {
                key: format!("emblem:{}", t.name),
                name: t.name.to_string(),
                pack: t.pack,
            })
            .collect();
        disc_emblems.sort_by(|a, b| a.name.cmp(&b.name));
        disc_emblems.dedup_by(|a, b| a.name == b.name);

        Self {
            slot_textures: std::array::from_fn(|_| Handle::default()),
            dirty_slots: [None; 5],
            layers: Vec::new(),
            next_id: 1,
            selected_id: None,
            dragging: false,
            place_new_mode: false,
            active_image_key: None,
            library_tab: DecalLibraryTab::Custom,
            library_page: 0,
            custom_assets: Vec::new(),
            disc_emblems,
            emblem_cache: HashMap::new(),
            brush_scale: 0.55,
            brush_mirror_x: false,
            brush_flip_h: false,
            brush_tint: [255, 255, 255, 255],
            loaded_design_id: None,
            cli_decal,
            initialized: false,
            picker_rx: None,
            last_hover: None,
        }
    }

    pub fn init_assets(&mut self, images: &mut Assets<Image>) {
        if self.initialized {
            return;
        }
        self.initialized = true;
        self.slot_textures =
            std::array::from_fn(|_| images.add(crate::paint::empty_decal_image()));
        self.scan_user_dir(images);
        if self.active_image_key.is_none() {
            if let Some(first) = self.custom_assets.first() {
                self.active_image_key = Some(first.key.clone());
            } else if let Some(first) = self.disc_emblems.first() {
                self.active_image_key = Some(first.key.clone());
                self.library_tab = DecalLibraryTab::Emblems;
            }
        }
    }

    pub fn slot_handle(&self, slot: DecalSlot) -> Handle<Image> {
        self.slot_textures[slot.index()].clone()
    }

    pub fn selected_layer(&self) -> Option<&PlacedDecal> {
        let id = self.selected_id?;
        self.layers.iter().find(|l| l.id == id)
    }

    pub fn selected_layer_mut(&mut self) -> Option<&mut PlacedDecal> {
        let id = self.selected_id?;
        self.layers.iter_mut().find(|l| l.id == id)
    }

    pub fn mark_slot_dirty(&mut self, slot: DecalSlot, full_res: bool) {
        let res = if full_res {
            slot.full_resolution()
        } else {
            DRAG_PREVIEW_RES.min(slot.full_resolution())
        };
        let entry = &mut self.dirty_slots[slot.index()];
        *entry = Some(entry.map_or(res, |cur| cur.max(res)));
    }

    pub fn mark_decal_slots_dirty(&mut self, slot: DecalSlot, mirror_x: bool, full_res: bool) {
        self.mark_slot_dirty(slot, full_res);
        if mirror_x {
            let other = slot.mirrored_slot();
            if other != slot {
                self.mark_slot_dirty(other, full_res);
            }
        }
    }

    pub fn mark_all_dirty(&mut self, full_res: bool) {
        for slot in DecalSlot::ALL {
            self.mark_slot_dirty(slot, full_res);
        }
    }

    /// Scans `decals/` for user-supplied image files on startup.
    pub fn scan_user_dir(&mut self, images: &mut Assets<Image>) {
        let dir = Path::new(USER_DECAL_DIR);
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && is_supported_image_ext(p))
            .collect();
        paths.sort();
        for p in paths {
            if let Err(e) = self.load_custom_file_internal(&p, false, images) {
                warn!("decals: skipping {}: {e:#}", p.display());
            }
        }
    }

    /// Imports an external image file (from file picker, drag-and-drop, or `--decal`), copies it
    /// into `decals/` so it persists across sessions, and selects it as the active brush.
    pub fn import_image_file(
        &mut self,
        path: &Path,
        images: &mut Assets<Image>,
    ) -> Result<String> {
        let key = self.load_custom_file_internal(path, true, images)?;
        self.active_image_key = Some(key.clone());
        self.library_tab = DecalLibraryTab::Custom;
        self.library_page = 0;
        Ok(key)
    }

    fn load_custom_file_internal(
        &mut self,
        path: &Path,
        copy_to_user_dir: bool,
        images: &mut Assets<Image>,
    ) -> Result<String> {
        let dyn_img = image::open(path)
            .with_context(|| format!("failed to decode image `{}`", path.display()))?;
        let mut rgba = dyn_img.to_rgba8();
        let (w, h) = rgba.dimensions();
        if w == 0 || h == 0 {
            bail!("image `{}` is empty", path.display());
        }
        if w > MAX_IMPORT_DIM || h > MAX_IMPORT_DIM {
            let scale = MAX_IMPORT_DIM as f32 / w.max(h) as f32;
            let nw = ((w as f32 * scale).round() as u32).max(1);
            let nh = ((h as f32 * scale).round() as u32).max(1);
            rgba = image::imageops::resize(
                &rgba,
                nw,
                nh,
                image::imageops::FilterType::Triangle,
            );
        }

        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("decal.png")
            .to_string();

        if copy_to_user_dir {
            let dir = Path::new(USER_DECAL_DIR);
            if std::fs::create_dir_all(dir).is_ok() {
                let dst = dir.join(&file_name);
                let same = std::fs::canonicalize(path)
                    .ok()
                    .zip(std::fs::canonicalize(&dst).ok())
                    .is_some_and(|(a, b)| a == b);
                if !same {
                    let _ = std::fs::copy(path, &dst);
                }
            }
        }

        let (fw, fh) = rgba.dimensions();
        let pixels = rgba.into_raw();
        let key = format!("custom:{file_name}");
        let label = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&file_name)
            .to_string();
        let thumb = images.add(rgba_to_ui_image(&pixels, fw, fh));
        let asset = DecalAsset {
            key: key.clone(),
            label,
            width: fw,
            height: fh,
            rgba: Arc::new(pixels),
            thumb,
        };
        if let Some(existing) = self.custom_assets.iter_mut().find(|a| a.key == key) {
            *existing = asset;
        } else {
            self.custom_assets.insert(0, asset);
        }
        Ok(key)
    }

    /// Ensures a retail 360 emblem (`emblem:imXXXX`) is decoded from the disc and cached.
    pub fn ensure_emblem(
        &mut self,
        disc: &Disc,
        key: &str,
        images: &mut Assets<Image>,
    ) -> Option<&DecalAsset> {
        if !self.emblem_cache.contains_key(key) {
            let entry = self.disc_emblems.iter().find(|e| e.key == key)?.clone();
            let asset = load_disc_emblem(disc, &entry, images).ok()?;
            self.emblem_cache.insert(key.to_string(), asset);
        }
        self.emblem_cache.get(key)
    }

    /// Resolves any decal key (`custom:...` or `emblem:...`) to its decoded [`DecalAsset`].
    pub fn resolve_asset(
        &mut self,
        disc: &Disc,
        key: &str,
        images: &mut Assets<Image>,
    ) -> Option<DecalAsset> {
        if let Some(a) = self.custom_assets.iter().find(|a| a.key == key) {
            return Some(a.clone());
        }
        if key.starts_with("emblem:") {
            return self.ensure_emblem(disc, key, images).cloned();
        }
        None
    }

    /// Looks up an already-loaded asset without touching the disc (for fast UI rendering).
    pub fn peek_asset(&self, key: &str) -> Option<&DecalAsset> {
        self.custom_assets
            .iter()
            .find(|a| a.key == key)
            .or_else(|| self.emblem_cache.get(key))
    }

    /// Pre-decodes the currently visible page of 360 emblems so the UI thumbnail grid shows live icons.
    pub fn warm_emblem_page(&mut self, disc: &Disc, images: &mut Assets<Image>, page_size: usize) {
        let start = self.library_page * page_size;
        let keys: Vec<String> = self
            .disc_emblems
            .iter()
            .skip(start)
            .take(page_size)
            .map(|e| e.key.clone())
            .collect();
        for key in keys {
            let _ = self.ensure_emblem(disc, &key, images);
        }
    }

    /// Opens the native OS file picker dialog on a background thread so Bevy never blocks.
    pub fn open_file_picker(&mut self) -> bool {
        if self.picker_rx.is_some() {
            return false;
        }
        let (tx, rx) = mpsc::channel();
        self.picker_rx = Some(Mutex::new(rx));
        std::thread::spawn(move || {
            let picked = pick_image_file_native();
            let _ = tx.send(picked);
        });
        true
    }

    /// Saves the current design's decal layers to `decals/layouts/design_{id}.json`.
    pub fn save_for_design(&self, design_id: u32) {
        let dir = Path::new(USER_DECAL_DIR).join("layouts");
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let path = dir.join(format!("design_{design_id}.json"));
        if self.layers.is_empty() {
            let _ = std::fs::remove_file(&path);
            return;
        }
        let payload = SavedDesignDecals {
            version: 1,
            decals: self.layers.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&payload) {
            let _ = std::fs::write(&path, json);
        }
    }

    /// Loads saved decal layers for `design_id` if present.
    pub fn load_for_design(&mut self, design_id: u32) {
        self.loaded_design_id = Some(design_id);
        self.layers.clear();
        self.selected_id = None;
        self.dragging = false;
        let path = Path::new(USER_DECAL_DIR)
            .join("layouts")
            .join(format!("design_{design_id}.json"));
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(saved) = serde_json::from_str::<SavedDesignDecals>(&text) {
                self.next_id = saved.decals.iter().map(|d| d.id).max().unwrap_or(0) + 1;
                self.layers = saved.decals;
                self.selected_id = self.layers.last().map(|d| d.id);
            }
        }
        self.mark_all_dirty(true);
    }

    pub fn slot_layer_count(&self, slot: DecalSlot) -> usize {
        self.layers.iter().filter(|l| l.slot == slot).count()
    }
}

fn is_supported_image_ext(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif"
    )
}

fn rgba_to_ui_image(pixels: &[u8], width: u32, height: u32) -> Image {
    let mut img = Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        pixels.to_vec(),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        ..default()
    });
    img
}

fn load_disc_emblem(
    disc: &Disc,
    entry: &DiscEmblemEntry,
    images: &mut Assets<Image>,
) -> Result<DecalAsset> {
    let bytes = disc
        .asset(entry.pack)
        .with_context(|| format!("read emblem pack `{}`", entry.pack))?;
    let header = tpf::read(&bytes)?;
    let tex = header
        .textures
        .first()
        .context("emblem TPF has no texture")?;
    let rgba = match tex.format {
        0 => {
            let lin = tex.linear(header.platform, &bytes, 8)?;
            tpf::decode_bc1(&lin, tex.width, tex.height)?
        }
        5 => {
            let lin = tex.linear(header.platform, &bytes, 16)?;
            tpf::decode_bc3(&lin, tex.width, tex.height)?
        }
        other => bail!("unsupported emblem TPF format {other} in {}", entry.pack),
    };
    let (w, h) = (tex.width as u32, tex.height as u32);
    let thumb = images.add(rgba_to_ui_image(&rgba, w, h));
    Ok(DecalAsset {
        key: entry.key.clone(),
        label: entry.name.to_ascii_uppercase(),
        width: w,
        height: h,
        rgba: Arc::new(rgba),
        thumb,
    })
}

/// Native Windows Explorer Open File dialog (`GetOpenFileNameW`, `OFN_NOCHANGEDIR`) with zero
/// extra crate dependencies.
#[cfg(windows)]
fn pick_image_file_native() -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    #[repr(C)]
    #[allow(non_snake_case)]
    struct OpenFileNameW {
        lStructSize: u32,
        hwndOwner: *mut core::ffi::c_void,
        hInstance: *mut core::ffi::c_void,
        lpstrFilter: *const u16,
        lpstrCustomFilter: *mut u16,
        nMaxCustFilter: u32,
        nFilterIndex: u32,
        lpstrFile: *mut u16,
        nMaxFile: u32,
        lpstrFileTitle: *mut u16,
        nMaxFileTitle: u32,
        lpstrInitialDir: *const u16,
        lpstrTitle: *const u16,
        Flags: u32,
        nFileOffset: u16,
        nFileExtension: u16,
        lpstrDefExt: *const u16,
        lCustData: isize,
        lpfnHook: *const core::ffi::c_void,
        lpTemplateName: *const u16,
        pvReserved: *mut core::ffi::c_void,
        dwReserved: u32,
        FlagsEx: u32,
    }

    #[link(name = "comdlg32")]
    extern "system" {
        fn GetOpenFileNameW(ofn: *mut OpenFileNameW) -> i32;
    }

    const OFN_PATHMUSTEXIST: u32 = 0x0000_0800;
    const OFN_FILEMUSTEXIST: u32 = 0x0000_1000;
    const OFN_NOCHANGEDIR: u32 = 0x0000_0008;

    let filter: Vec<u16> =
        "Image Files (*.png;*.jpg;*.jpeg;*.webp;*.bmp;*.gif)\0*.png;*.jpg;*.jpeg;*.webp;*.bmp;*.gif\0All Files (*.*)\0*.*\0\0"
            .encode_utf16()
            .collect();
    let title: Vec<u16> = "Upload Custom AC Decal Image\0".encode_utf16().collect();
    let mut file_buf = vec![0u16; 4096];

    let mut ofn = OpenFileNameW {
        lStructSize: std::mem::size_of::<OpenFileNameW>() as u32,
        hwndOwner: std::ptr::null_mut(),
        hInstance: std::ptr::null_mut(),
        lpstrFilter: filter.as_ptr(),
        lpstrCustomFilter: std::ptr::null_mut(),
        nMaxCustFilter: 0,
        nFilterIndex: 1,
        lpstrFile: file_buf.as_mut_ptr(),
        nMaxFile: file_buf.len() as u32,
        lpstrFileTitle: std::ptr::null_mut(),
        nMaxFileTitle: 0,
        lpstrInitialDir: std::ptr::null(),
        lpstrTitle: title.as_ptr(),
        Flags: OFN_PATHMUSTEXIST | OFN_FILEMUSTEXIST | OFN_NOCHANGEDIR,
        nFileOffset: 0,
        nFileExtension: 0,
        lpstrDefExt: std::ptr::null(),
        lCustData: 0,
        lpfnHook: std::ptr::null(),
        lpTemplateName: std::ptr::null(),
        pvReserved: std::ptr::null_mut(),
        dwReserved: 0,
        FlagsEx: 0,
    };

    // SAFETY: `ofn` points to valid stack/heap UTF-16 buffers alive for the duration of the call.
    let ok = unsafe { GetOpenFileNameW(&mut ofn) };
    if ok == 0 {
        return None;
    }
    let len = file_buf.iter().position(|&c| c == 0).unwrap_or(file_buf.len());
    if len == 0 {
        return None;
    }
    Some(PathBuf::from(OsString::from_wide(&file_buf[..len])))
}

#[cfg(not(windows))]
fn pick_image_file_native() -> Option<PathBuf> {
    None
}

/// Computes an orthonormal `(right, up, forward)` projector basis for `normal`, `tangent`, `rotation`.
fn projector_basis(normal: Vec3, tangent_hint: Vec3, rotation: f32) -> (Vec3, Vec3, Vec3) {
    let forward = normal.normalize_or(Vec3::Z);
    let mut right = tangent_hint - forward * tangent_hint.dot(forward);
    if right.length_squared() < 1e-6 {
        let up_ref = if forward.y.abs() < 0.92 {
            Vec3::Y
        } else {
            Vec3::X
        };
        right = up_ref.cross(forward);
    }
    right = right.normalize_or(Vec3::X);
    let up = forward.cross(right).normalize_or(Vec3::Y);

    let (sin_r, cos_r) = rotation.sin_cos();
    let rot_right = (right * cos_r + up * sin_r).normalize_or(right);
    let rot_up = forward.cross(rot_right).normalize_or(up);
    (rot_right, rot_up, forward)
}

/// Computes a smooth rest-pose tangent consistent with an optional previous tangent so dragging a
/// decal across curved armor never causes sudden orientation flips.
fn stable_tangent(normal: Vec3, prev_tangent: Option<Vec3>) -> Vec3 {
    let n = normal.normalize_or(Vec3::Z);
    if let Some(prev) = prev_tangent {
        let proj = prev - n * prev.dot(n);
        if proj.length_squared() > 1e-4 {
            return proj.normalize();
        }
    }
    let up_ref = if n.y.abs() < 0.92 { Vec3::Y } else { Vec3::Z };
    up_ref.cross(n).normalize_or(Vec3::X)
}

/// Builds the mirrored projector parameters across X = 0 for symmetric `mirror_x` decals.
fn mirrored_projector(
    origin: Vec3,
    right: Vec3,
    up: Vec3,
    forward: Vec3,
) -> (Vec3, Vec3, Vec3, Vec3) {
    let m_origin = Vec3::new(-origin.x, origin.y, origin.z);
    let m_forward = Vec3::new(-forward.x, forward.y, forward.z).normalize_or(Vec3::Z);
    let m_up = Vec3::new(-up.x, up.y, up.z).normalize_or(Vec3::Y);
    // Flip horizontal right direction so asymmetric logos mirror cleanly left <-> right.
    let m_right = Vec3::new(right.x, -right.y, -right.z).normalize_or(Vec3::X);
    (m_origin, m_right, m_up, m_forward)
}

/// Bilinearly samples straight-alpha RGBA8 pixels at UV `(u, v)` in `[0, 1]`.
#[inline]
fn sample_bilinear(rgba: &[u8], w: u32, h: u32, u: f32, v: f32) -> [f32; 4] {
    let fx = (u * w as f32 - 0.5).clamp(0.0, (w.saturating_sub(1)) as f32);
    let fy = (v * h as f32 - 0.5).clamp(0.0, (h.saturating_sub(1)) as f32);
    let x0 = fx.floor() as u32;
    let y0 = fy.floor() as u32;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let tx = fx - x0 as f32;
    let ty = fy - y0 as f32;

    let fetch = |x: u32, y: u32| -> [f32; 4] {
        let i = ((y * w + x) * 4) as usize;
        [
            rgba[i] as f32 / 255.0,
            rgba[i + 1] as f32 / 255.0,
            rgba[i + 2] as f32 / 255.0,
            rgba[i + 3] as f32 / 255.0,
        ]
    };
    let c00 = fetch(x0, y0);
    let c10 = fetch(x1, y0);
    let c01 = fetch(x0, y1);
    let c11 = fetch(x1, y1);

    let mut out = [0.0f32; 4];
    for ch in 0..4 {
        let top = c00[ch] + (c10[ch] - c00[ch]) * tx;
        let bot = c01[ch] + (c11[ch] - c01[ch]) * tx;
        out[ch] = top + (bot - top) * ty;
    }
    out
}

/// Projects one decal pass into premultiplied sRGB RGBA8 buffer `buf` (`size × size`), mirroring
/// `Flver_ACPartsDecalPaint.vpo/.fpo` (UV1 rasterization + 3D projector + facing/depth cutoff).
#[allow(clippy::too_many_arguments)]
fn rasterize_projector_into_uv1(
    buf: &mut [u8],
    stamped: &mut [u32],
    stamp_id: u32,
    size: u32,
    meshes: &[&CachedPartMesh],
    asset: &DecalAsset,
    origin: Vec3,
    right: Vec3,
    up: Vec3,
    forward: Vec3,
    half_w: f32,
    half_h: f32,
    depth: f32,
    opacity: f32,
    tint: [u8; 4],
    flip_h: bool,
) {
    let inv_w = 1.0 / (2.0 * half_w.max(1e-4));
    let inv_h = 1.0 / (2.0 * half_h.max(1e-4));
    let bound_r = (half_w * half_w + half_h * half_h + depth * depth).sqrt();
    let tint_f = [
        tint[0] as f32 / 255.0,
        tint[1] as f32 / 255.0,
        tint[2] as f32 / 255.0,
        (tint[3] as f32 / 255.0) * opacity.clamp(0.0, 1.0),
    ];
    if tint_f[3] <= 1e-4 {
        return;
    }
    let sf = size as f32;
    let max_i = (size - 1) as i32;

    for mesh in meshes {
        for tri in mesh.indices.chunks_exact(3) {
            let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
            let (p0, p1, p2) = (mesh.positions[i0], mesh.positions[i1], mesh.positions[i2]);

            // Fast 3D sphere/AABB cull before touching UV space.
            let tmin = p0.min(p1).min(p2);
            let tmax = p0.max(p1).max(p2);
            if origin.x + bound_r < tmin.x
                || origin.x - bound_r > tmax.x
                || origin.y + bound_r < tmin.y
                || origin.y - bound_r > tmax.y
                || origin.z + bound_r < tmin.z
                || origin.z - bound_r > tmax.z
            {
                continue;
            }

            let (n0, n1, n2) = (mesh.normals[i0], mesh.normals[i1], mesh.normals[i2]);
            // Skip triangles completely facing away from the projector.
            if n0.dot(forward) <= 0.02 && n1.dot(forward) <= 0.02 && n2.dot(forward) <= 0.02 {
                continue;
            }

            let (uv0, uv1, uv2) = (mesh.uv1[i0], mesh.uv1[i1], mesh.uv1[i2]);
            let (x0, y0) = (uv0.x * sf, uv0.y * sf);
            let (x1, y1) = (uv1.x * sf, uv1.y * sf);
            let (x2, y2) = (uv2.x * sf, uv2.y * sf);

            let area = (x1 - x0) * (y2 - y0) - (x2 - x0) * (y1 - y0);
            if area.abs() < 1e-5 {
                continue;
            }
            let inv_area = 1.0 / area;

            // Pad bounding box by 1 texel for conservative UV-island edge coverage (prevents dark seams).
            let min_x = (x0.min(x1).min(x2).floor() as i32 - 1).clamp(0, max_i);
            let max_x = (x0.max(x1).max(x2).ceil() as i32 + 1).clamp(0, max_i);
            let min_y = (y0.min(y1).min(y2).floor() as i32 - 1).clamp(0, max_i);
            let max_y = (y0.max(y1).max(y2).ceil() as i32 + 1).clamp(0, max_i);

            // Edge tolerance in barycentric space equivalent to ~0.75 texel outside each edge.
            let e0_len = Vec2::new(x2 - x1, y2 - y1).length();
            let e1_len = Vec2::new(x0 - x2, y0 - y2).length();
            let e2_len = Vec2::new(x1 - x0, y1 - y0).length();
            let eps0 = -(0.75 * e0_len * inv_area.abs()).min(0.25);
            let eps1 = -(0.75 * e1_len * inv_area.abs()).min(0.25);
            let eps2 = -(0.75 * e2_len * inv_area.abs()).min(0.25);

            for py in min_y..=max_y {
                let fy = py as f32 + 0.5;
                for px in min_x..=max_x {
                    let fx = px as f32 + 0.5;
                    let w0 = ((x1 - fx) * (y2 - fy) - (x2 - fx) * (y1 - fy)) * inv_area;
                    let w1 = ((x2 - fx) * (y0 - fy) - (x0 - fx) * (y2 - fy)) * inv_area;
                    let w2 = 1.0 - w0 - w1;
                    if w0 < eps0 || w1 < eps1 || w2 < eps2 {
                        continue;
                    }
                    // Clamp extrapolated padding texels back onto the triangle for clean 3D eval.
                    let (cw0, cw1, cw2) = if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                        let a = w0.max(0.0);
                        let b = w1.max(0.0);
                        let c = w2.max(0.0);
                        let s = (a + b + c).max(1e-6);
                        (a / s, b / s, c / s)
                    } else {
                        (w0, w1, w2)
                    };

                    let nrm = (n0 * cw0 + n1 * cw1 + n2 * cw2).normalize_or(forward);
                    let facing = nrm.dot(forward);
                    if facing <= 0.08 {
                        continue;
                    }

                    let pos = p0 * cw0 + p1 * cw1 + p2 * cw2;
                    let delta = pos - origin;
                    let dz = delta.dot(forward).abs();
                    if dz > depth {
                        continue;
                    }

                    let mut u = 0.5 + delta.dot(right) * inv_w;
                    let v = 0.5 - delta.dot(up) * inv_h;
                    if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                        continue;
                    }
                    if flip_h {
                        u = 1.0 - u;
                    }

                    let s = sample_bilinear(&asset.rgba, asset.width, asset.height, u, v);
                    // Smooth fade near grazing angles and projection depth cutoff.
                    let angle_fade = ((facing - 0.08) / 0.14).clamp(0.0, 1.0);
                    let depth_fade = (1.0 - ((dz / depth - 0.85) / 0.15).clamp(0.0, 1.0)).clamp(0.0, 1.0);
                    let sa = s[3] * tint_f[3] * angle_fade * depth_fade;
                    if sa <= 1e-4 {
                        continue;
                    }
                    let sr = (s[0] * tint_f[0]).clamp(0.0, 1.0) * sa;
                    let sg = (s[1] * tint_f[1]).clamp(0.0, 1.0) * sa;
                    let sb = (s[2] * tint_f[2]).clamp(0.0, 1.0) * sa;

                    // Prevent double-blending along shared triangle edges or 1-texel seam padding.
                    let px_idx = (py as u32 * size + px as u32) as usize;
                    if stamped[px_idx] == stamp_id {
                        continue;
                    }
                    stamped[px_idx] = stamp_id;

                    // Porter-Duff SRC_OVER in premultiplied sRGB RGBA8.
                    let at = px_idx * 4;
                    let inv_sa = 1.0 - sa;
                    let dr = buf[at] as f32 / 255.0;
                    let dg = buf[at + 1] as f32 / 255.0;
                    let db = buf[at + 2] as f32 / 255.0;
                    let da = buf[at + 3] as f32 / 255.0;

                    buf[at] = ((sr + dr * inv_sa).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                    buf[at + 1] = ((sg + dg * inv_sa).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                    buf[at + 2] = ((sb + db * inv_sa).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                    buf[at + 3] = ((sa + da * inv_sa).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                }
            }
        }
    }
}

/// Bakes all decals affecting `slot` into a premultiplied sRGB RGBA8 texture buffer (`size × size`).
pub fn bake_slot_pixels(
    slot: DecalSlot,
    size: u32,
    cache: &DecalMeshCache,
    layers: &[PlacedDecal],
    resolve: &mut dyn FnMut(&str) -> Option<DecalAsset>,
) -> Vec<u8> {
    let mut buf = vec![0u8; (size * size * 4) as usize];
    let slot_meshes: Vec<&CachedPartMesh> =
        cache.meshes.iter().filter(|m| m.slot == slot).collect();
    if slot_meshes.is_empty() {
        return buf;
    }
    let mut stamped = vec![0u32; (size * size) as usize];
    let mut stamp_id = 0u32;

    for decal in layers {
        let direct = decal.slot == slot;
        let mirrored_here = decal.mirror_x && decal.slot.mirrored_slot() == slot;
        if !direct && !mirrored_here {
            continue;
        }
        let Some(asset) = resolve(&decal.image_key) else {
            continue;
        };
        let aspect = asset.aspect();
        let half_h = (decal.scale * 0.5).max(0.02);
        let half_w = half_h * aspect;
        let origin = decal.pos_vec();
        let (right, up, forward) = projector_basis(decal.norm_vec(), decal.tan_vec(), decal.rotation);

        if direct {
            stamp_id = stamp_id.wrapping_add(1).max(1);
            rasterize_projector_into_uv1(
                &mut buf,
                &mut stamped,
                stamp_id,
                size,
                &slot_meshes,
                &asset,
                origin,
                right,
                up,
                forward,
                half_w,
                half_h,
                decal.depth.max(0.05),
                decal.opacity,
                decal.tint,
                decal.flip_h,
            );
        }
        if mirrored_here {
            stamp_id = stamp_id.wrapping_add(1).max(1);
            let (mo, mr, mu, mf) = mirrored_projector(origin, right, up, forward);
            rasterize_projector_into_uv1(
                &mut buf,
                &mut stamped,
                stamp_id,
                size,
                &slot_meshes,
                &asset,
                mo,
                mr,
                mu,
                mf,
                half_w,
                half_h,
                decal.depth.max(0.05),
                decal.opacity,
                decal.tint,
                !decal.flip_h,
            );
        }
    }
    buf
}

/// Computes the world-space skinned position and normal for vertex `vi` of `mesh`.
#[inline]
fn skin_vertex(mesh: &CachedPartMesh, skin_mats: &[Mat4], vi: usize) -> (Vec3, Vec3) {
    let p = mesh.positions[vi];
    let n = mesh.normals[vi];
    let (Some(ji), Some(jw)) = (&mesh.joint_indices, &mesh.joint_weights) else {
        return (p, n);
    };
    let idx = ji[vi];
    let w = jw[vi];
    let wsum = w[0] + w[1] + w[2] + w[3];
    if wsum <= 1e-5 {
        return (p, n);
    }
    let mut wp = Vec3::ZERO;
    let mut wn = Vec3::ZERO;
    for k in 0..4 {
        if w[k] > 0.0 {
            if let Some(&m) = skin_mats.get(idx[k] as usize) {
                wp += m.transform_point3(p) * w[k];
                wn += m.transform_vector3(n) * w[k];
            }
        }
    }
    (wp, wn.normalize_or(n))
}

/// Möller–Trumbore ray-triangle intersection; returns `(t, u, v)` where `w0 = 1 - u - v, w1 = u, w2 = v`.
#[inline]
fn ray_triangle(origin: Vec3, dir: Vec3, v0: Vec3, v1: Vec3, v2: Vec3) -> Option<(f32, f32, f32)> {
    let e1 = v1 - v0;
    let e2 = v2 - v0;
    let h = dir.cross(e2);
    let a = e1.dot(h);
    if a.abs() < 1e-7 {
        return None;
    }
    let f = 1.0 / a;
    let s = origin - v0;
    let u = f * s.dot(h);
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = f * dir.dot(q);
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = f * e2.dot(q);
    (t > 1e-4).then_some((t, u, v))
}

/// Raycasts a world-space ray against the live skinned AC frame meshes, returning the closest
/// front-facing hit in both world space and rest-pose part space.
pub fn raycast_ac(
    ray: Ray3d,
    cache: &DecalMeshCache,
    global_transforms: &Query<&GlobalTransform>,
    prev_rest_tangent: Option<Vec3>,
) -> Option<SurfaceHit> {
    let origin = ray.origin;
    let dir = *ray.direction;
    let mut best: Option<SurfaceHit> = None;

    for mesh in &cache.meshes {
        let skin_mats: Vec<Mat4> = mesh
            .joints
            .iter()
            .zip(mesh.inverse_bindposes.iter())
            .map(|(&e, &ibp)| {
                global_transforms
                    .get(e)
                    .map(|g| g.to_matrix() * ibp)
                    .unwrap_or(Mat4::IDENTITY)
            })
            .collect();

        // Skin all vertices of this mesh once per raycast (typically a few hundred vertices).
        let skinned: Vec<(Vec3, Vec3)> = (0..mesh.positions.len())
            .map(|vi| skin_vertex(mesh, &skin_mats, vi))
            .collect();

        for tri in mesh.indices.chunks_exact(3) {
            let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
            let (wp0, wn0) = skinned[i0];
            let (wp1, wn1) = skinned[i1];
            let (wp2, wn2) = skinned[i2];
            let Some((t, u, v)) = ray_triangle(origin, dir, wp0, wp1, wp2) else {
                continue;
            };
            if best.is_some_and(|b| t >= b.distance) {
                continue;
            }
            let w0 = 1.0 - u - v;
            let mut world_normal = (wn0 * w0 + wn1 * u + wn2 * v).normalize_or(Vec3::Z);
            // Only pick front-facing triangles relative to the camera ray.
            if world_normal.dot(dir) > 0.05 {
                let geom_n = (wp1 - wp0).cross(wp2 - wp0).normalize_or(world_normal);
                if geom_n.dot(dir) > 0.05 {
                    continue;
                }
                world_normal = -world_normal;
            }
            let world_pos = wp0 * w0 + wp1 * u + wp2 * v;
            let rest_pos = mesh.positions[i0] * w0 + mesh.positions[i1] * u + mesh.positions[i2] * v;
            let rest_normal = (mesh.normals[i0] * w0 + mesh.normals[i1] * u + mesh.normals[i2] * v)
                .normalize_or(Vec3::Z);
            let rest_tangent = stable_tangent(rest_normal, prev_rest_tangent);
            let world_tangent = stable_tangent(world_normal, None);

            best = Some(SurfaceHit {
                slot: mesh.slot,
                world_pos,
                world_normal,
                world_tangent,
                rest_pos,
                rest_normal,
                rest_tangent,
                distance: t,
            });
        }
    }
    best
}

/// Finds the topmost placed decal under a rest-pose surface hit so clicking an existing decal
/// picks it up immediately for click-and-drag repositioning.
fn pick_existing_decal(layers: &[PlacedDecal], hit: &SurfaceHit) -> Option<u64> {
    for decal in layers.iter().rev() {
        if decal.slot != hit.slot {
            continue;
        }
        let n = decal.norm_vec();
        if n.dot(hit.rest_normal) < 0.2 {
            continue;
        }
        let delta = hit.rest_pos - decal.pos_vec();
        if delta.dot(n).abs() > decal.depth * 1.1 {
            continue;
        }
        let radius = (decal.scale * 0.6).max(0.18);
        let plane_dist = (delta - n * delta.dot(n)).length();
        if plane_dist <= radius {
            return Some(decal.id);
        }
    }
    None
}

/// System: polls the async native file picker + handles OS file drag-and-drop onto the game window.
#[allow(clippy::too_many_arguments)]
pub fn handle_imports(
    mut commands: Commands,
    mode: Res<Mode>,
    mut decals: ResMut<DecalState>,
    mut garage_state: ResMut<GarageState>,
    mut images: ResMut<Assets<Image>>,
    mut drop_events: MessageReader<FileDragAndDrop>,
    mut cues: ResMut<Cues>,
    mut audio_assets: ResMut<Assets<AudioSource>>,
) {
    let mut imported_paths: Vec<PathBuf> = Vec::new();

    if let Some(rx_lock) = &decals.picker_rx {
        let polled = rx_lock
            .lock()
            .map(|rx| rx.try_recv())
            .unwrap_or(Err(mpsc::TryRecvError::Disconnected));
        match polled {
            Ok(Some(path)) => {
                decals.picker_rx = None;
                imported_paths.push(path);
            }
            Ok(None) | Err(mpsc::TryRecvError::Disconnected) => {
                decals.picker_rx = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    for ev in drop_events.read() {
        if let FileDragAndDrop::DroppedFile { path_buf, .. } = ev {
            if *mode == Mode::Garage && is_supported_image_ext(path_buf) {
                imported_paths.push(path_buf.clone());
            }
        }
    }

    for path in imported_paths {
        match decals.import_image_file(&path, &mut images) {
            Ok(key) => {
                let name = decals
                    .peek_asset(&key)
                    .map(|a| a.label.clone())
                    .unwrap_or(key);
                if garage_state.screen != GarageScreen::Paint
                    || garage_state.paint_tab != PaintTab::Decals
                {
                    garage_state.screen = GarageScreen::Paint;
                    garage_state.paint_tab = PaintTab::Decals;
                    garage_state.cam_manual_orbit = false;
                }
                decals.place_new_mode = true;
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_OK);
                garage_state.set_banner(
                    format!("IMPORTED DECAL \"{name}\" — click & drag on AC to place!"),
                    Color::srgb(0.45, 0.92, 0.62),
                );
            }
            Err(e) => {
                warn!("decal import failed: {e:#}");
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_ERROR);
                garage_state.set_banner(
                    format!("IMPORT FAILED: {e}"),
                    Color::srgb(1.0, 0.42, 0.35),
                );
            }
        }
    }
}

#[derive(SystemParam)]
pub struct DecalInput<'w> {
    buttons: Res<'w, ButtonInput<MouseButton>>,
    keys: Res<'w, ButtonInput<KeyCode>>,
    scroll: Res<'w, AccumulatedMouseScroll>,
    cues: ResMut<'w, Cues>,
    audio_assets: ResMut<'w, Assets<AudioSource>>,
}

/// System: handles direct 3D click-and-drag placement, repositioning, scale/rotate shortcuts,
/// and draws the 3D surface projection gizmo when in `PAINT -> DECALS`.
#[allow(clippy::too_many_arguments)]
pub fn interact_and_gizmo(
    mut commands: Commands,
    mode: Res<Mode>,
    mut garage_state: ResMut<GarageState>,
    garage: Res<crate::Garage>,
    mut decals: ResMut<DecalState>,
    cache: Res<DecalMeshCache>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cams: Query<
        (&Camera, &GlobalTransform),
        (With<Camera3d>, Without<crate::env::WaterReflectionCamera>),
    >,
    global_transforms: Query<&GlobalTransform>,
    ui_interactions: Query<&Interaction>,
    input: DecalInput,
    mut gizmos: Gizmos,
    mut images: ResMut<Assets<Image>>,
) {
    let buttons = &input.buttons;
    let keys = &input.keys;
    let scroll = &input.scroll;
    let mut cues = input.cues;
    let mut audio_assets = input.audio_assets;
    let active = *mode == Mode::Garage
        && garage_state.screen == GarageScreen::Paint
        && garage_state.paint_tab == PaintTab::Decals;
    if !active {
        if decals.dragging {
            decals.dragging = false;
            decals.mark_all_dirty(true);
        }
        decals.last_hover = None;
        return;
    }

    let Ok(window) = windows.single() else {
        return;
    };
    let Ok((camera, cam_xf)) = cams.single() else {
        return;
    };

    // Determine if cursor is over the left/right UI panels or any hovered UI button.
    let cursor_pos = window.cursor_position();
    let ui_hovered = ui_interactions
        .iter()
        .any(|i| matches!(i, Interaction::Hovered | Interaction::Pressed))
        || cursor_pos.is_some_and(|p| {
            p.x <= 396.0
                || p.x >= window.width() - 304.0
                || p.y <= 58.0
                || p.y >= window.height() - 36.0
        });

    // Handle quick keyboard/scroll modifiers for the currently selected decal.
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    if decals.selected_id.is_some() {
        let mut scale_delta = 0.0f32;
        if shift && scroll.delta.y.abs() > 0.0 {
            scale_delta += scroll.delta.y * 0.06;
        }
        if keys.just_pressed(KeyCode::BracketRight) || keys.just_pressed(KeyCode::Equal) {
            scale_delta += 0.08;
        }
        if keys.just_pressed(KeyCode::BracketLeft) || keys.just_pressed(KeyCode::Minus) {
            scale_delta -= 0.08;
        }
        if scale_delta.abs() > 0.0 {
            let full = !decals.dragging;
            if let Some(d) = decals.selected_layer_mut() {
                d.scale = (d.scale + scale_delta).clamp(0.08, 2.5);
                let (slot, mirror, sc) = (d.slot, d.mirror_x, d.scale);
                decals.brush_scale = sc;
                decals.mark_decal_slots_dirty(slot, mirror, full);
                if full {
                    if let Some(did) = decals.loaded_design_id {
                        decals.save_for_design(did);
                    }
                }
                garage_state.ui_dirty = true;
            }
        }

        let mut rot_delta = 0.0f32;
        if ctrl && scroll.delta.y.abs() > 0.0 {
            rot_delta += scroll.delta.y * 12.0f32.to_radians();
        }
        if keys.just_pressed(KeyCode::KeyR) {
            rot_delta += 15.0f32.to_radians();
        }
        if keys.just_pressed(KeyCode::KeyT) {
            rot_delta -= 15.0f32.to_radians();
        }
        if rot_delta.abs() > 0.0 {
            let full = !decals.dragging;
            if let Some(d) = decals.selected_layer_mut() {
                d.rotation = (d.rotation + rot_delta).rem_euclid(std::f32::consts::TAU);
                let (slot, mirror) = (d.slot, d.mirror_x);
                decals.mark_decal_slots_dirty(slot, mirror, full);
                if full {
                    if let Some(did) = decals.loaded_design_id {
                        decals.save_for_design(did);
                    }
                }
                garage_state.ui_dirty = true;
            }
        }

        if keys.just_pressed(KeyCode::KeyM) {
            if let Some(d) = decals.selected_layer_mut() {
                let slot = d.slot;
                d.mirror_x = !d.mirror_x;
                let mirror = d.mirror_x;
                decals.brush_mirror_x = mirror;
                decals.mark_slot_dirty(slot, true);
                decals.mark_slot_dirty(slot.mirrored_slot(), true);
                if let Some(did) = decals.loaded_design_id {
                    decals.save_for_design(did);
                }
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                garage_state.ui_dirty = true;
            }
        }

        if keys.just_pressed(KeyCode::KeyF) {
            if let Some(d) = decals.selected_layer_mut() {
                d.flip_h = !d.flip_h;
                let (slot, mirror) = (d.slot, d.mirror_x);
                decals.brush_flip_h = d.flip_h;
                decals.mark_decal_slots_dirty(slot, mirror, true);
                if let Some(did) = decals.loaded_design_id {
                    decals.save_for_design(did);
                }
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                garage_state.ui_dirty = true;
            }
        }

        if keys.just_pressed(KeyCode::Delete) {
            if let Some(id) = decals.selected_id.take() {
                if let Some(pos) = decals.layers.iter().position(|l| l.id == id) {
                    let removed = decals.layers.remove(pos);
                    decals.selected_id = decals.layers.last().map(|l| l.id);
                    decals.dragging = false;
                    decals.mark_decal_slots_dirty(removed.slot, removed.mirror_x, true);
                    if let Some(did) = decals.loaded_design_id {
                        decals.save_for_design(did);
                    }
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_CANCEL);
                    garage_state.set_banner("REMOVED DECAL LAYER", Color::srgb(0.95, 0.72, 0.32));
                }
            }
        }
    }

    // Raycast cursor against the live skinned AC when over the 3D viewport (or while dragging).
    let prev_tan = decals.selected_layer().map(|l| l.tan_vec());
    let hit = if !ui_hovered || decals.dragging {
        cursor_pos
            .and_then(|cp| camera.viewport_to_world(cam_xf, cp).ok())
            .and_then(|ray| raycast_ac(ray, &cache, &global_transforms, prev_tan))
    } else {
        None
    };
    decals.last_hover = hit;

    // Left mouse down: either pick up an existing decal under the cursor or stamp a new one!
    if buttons.just_pressed(MouseButton::Left) && !ui_hovered {
        if let Some(h) = hit {
            let picked = if decals.place_new_mode {
                None
            } else {
                pick_existing_decal(&decals.layers, &h)
            };
            if let Some(existing_id) = picked {
                decals.selected_id = Some(existing_id);
                decals.dragging = true;
                sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_MOVE);
                garage_state.ui_dirty = true;
            } else if let Some(image_key) = decals.active_image_key.clone() {
                if decals.slot_layer_count(h.slot) >= MAX_DECALS_PER_SLOT {
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_ERROR);
                    garage_state.set_banner(
                        format!(
                            "{} REACHED 360 MAX ({MAX_DECALS_PER_SLOT} DECALS)",
                            h.slot.label()
                        ),
                        Color::srgb(1.0, 0.42, 0.35),
                    );
                } else {
                    // Ensure the asset is decoded before placing.
                    let _ = decals.resolve_asset(&garage.disc, &image_key, &mut images);
                    let id = decals.next_id;
                    decals.next_id += 1;
                    let placed = PlacedDecal {
                        id,
                        slot: h.slot,
                        image_key,
                        position: h.rest_pos.into(),
                        normal: h.rest_normal.into(),
                        tangent: h.rest_tangent.into(),
                        scale: decals.brush_scale,
                        rotation: 0.0,
                        depth: 0.48,
                        opacity: 1.0,
                        tint: decals.brush_tint,
                        mirror_x: decals.brush_mirror_x,
                        flip_h: decals.brush_flip_h,
                    };
                    let mirror = placed.mirror_x;
                    decals.layers.push(placed);
                    decals.selected_id = Some(id);
                    decals.dragging = true;
                    decals.place_new_mode = false;
                    decals.mark_decal_slots_dirty(h.slot, mirror, false);
                    sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_EQUIP);
                    garage_state.ui_dirty = true;
                }
            } else {
                garage_state.set_banner(
                    "UPLOAD AN IMAGE OR SELECT A 360 EMBLEM FIRST",
                    Color::srgb(0.95, 0.72, 0.32),
                );
            }
        }
    }

    // Left mouse held while dragging: slide the selected decal smoothly across the AC armor!
    if decals.dragging && buttons.pressed(MouseButton::Left) {
        if let Some(h) = hit {
            if let Some(d) = decals.selected_layer_mut() {
                let prev_slot = d.slot;
                let mirror = d.mirror_x;
                let moved = d.pos_vec().distance_squared(h.rest_pos) > 1e-6 || prev_slot != h.slot;
                if moved {
                    let prev_t = Some(d.tan_vec());
                    d.slot = h.slot;
                    d.position = h.rest_pos.into();
                    d.normal = h.rest_normal.into();
                    d.tangent = stable_tangent(h.rest_normal, prev_t).into();
                    if prev_slot != h.slot {
                        decals.mark_decal_slots_dirty(prev_slot, mirror, false);
                        garage_state.ui_dirty = true;
                    }
                    decals.mark_decal_slots_dirty(h.slot, mirror, false);
                }
            }
        }
    }

    // Left mouse released: upgrade live preview texture to full 360 resolution (512 / 1024) & save!
    if decals.dragging && !buttons.pressed(MouseButton::Left) {
        decals.dragging = false;
        decals.mark_all_dirty(true);
        if let Some(did) = decals.loaded_design_id {
            decals.save_for_design(did);
        }
        sound::cue(&mut commands, &mut cues, &mut audio_assets, CUE_OK);
        garage_state.ui_dirty = true;
    }

    // Draw 3D surface projector gizmo at cursor hover point.
    if let Some(h) = hit {
        let hovered_existing_id = if !decals.place_new_mode && !decals.dragging {
            pick_existing_decal(&decals.layers, &h)
        } else {
            None
        };
        let (scale, rotation, aspect, color) = if decals.dragging {
            let d = decals.selected_layer();
            let asp = d
                .and_then(|l| decals.peek_asset(&l.image_key))
                .map(|a| a.aspect())
                .unwrap_or(1.0);
            (
                d.map(|l| l.scale).unwrap_or(decals.brush_scale),
                d.map(|l| l.rotation).unwrap_or(0.0),
                asp,
                Color::srgb(0.98, 0.72, 0.16),
            )
        } else if let Some(hid) = hovered_existing_id {
            let d = decals.layers.iter().find(|l| l.id == hid);
            let asp = d
                .and_then(|l| decals.peek_asset(&l.image_key))
                .map(|a| a.aspect())
                .unwrap_or(1.0);
            (
                d.map(|l| l.scale).unwrap_or(decals.brush_scale),
                d.map(|l| l.rotation).unwrap_or(0.0),
                asp,
                Color::srgb(0.98, 0.72, 0.16),
            )
        } else {
            let asp = decals
                .active_image_key
                .as_deref()
                .and_then(|k| decals.peek_asset(k))
                .map(|a| a.aspect())
                .unwrap_or(1.0);
            (
                decals.brush_scale,
                0.0,
                asp,
                Color::srgb(0.32, 0.88, 1.0),
            )
        };
        let (right, up, fwd) = projector_basis(h.world_normal, h.world_tangent, rotation);
        let c = h.world_pos + fwd * 0.03;
        let hw = scale * 0.5 * aspect;
        let hh = scale * 0.5;
        let tl = c - right * hw + up * hh;
        let tr = c + right * hw + up * hh;
        let br = c + right * hw - up * hh;
        let bl = c - right * hw - up * hh;
        gizmos.line(tl, tr, color);
        gizmos.line(tr, br, color);
        gizmos.line(br, bl, color);
        gizmos.line(bl, tl, color);
        // Up orientation indicator + center crosshair.
        let top_mid = (tl + tr) * 0.5;
        gizmos.line(top_mid, top_mid + up * (hh * 0.28), color);
        gizmos.line(c - right * 0.06, c + right * 0.06, color);
        gizmos.line(c - up * 0.06, c + up * 0.06, color);
    }
}

/// System: rebakes any dirty `g_AC_DecalMap` slot textures and handles optional CLI `--decal`
/// auto-stamping (for automated visual verification via `--shot`).
pub fn sync_bakes(
    garage: Res<crate::Garage>,
    mut garage_state: ResMut<GarageState>,
    mut decals: ResMut<DecalState>,
    cache: Res<DecalMeshCache>,
    mut images: ResMut<Assets<Image>>,
    mut paint_materials: ResMut<Assets<crate::paint::AcPaintMaterial>>,
) {
    decals.init_assets(&mut images);

    // Load saved decals whenever the active design changes.
    let design_id = garage
        .designs
        .get(garage.current)
        .map(|r| r.id)
        .unwrap_or(0);
    if decals.loaded_design_id != Some(design_id) && !cache.meshes.is_empty() {
        decals.load_for_design(design_id);
        garage_state.ui_dirty = true;
    }

    // Handle `--decal <path>` CLI auto-stamp onto the AC Core chest once meshes are ready.
    if let Some(cli_path) = decals.cli_decal.take() {
        if !cache.meshes.is_empty() {
            let key_res = if let Some(emblem_name) = cli_path
                .to_str()
                .and_then(|s| s.strip_prefix("emblem:"))
            {
                let key = format!("emblem:{emblem_name}");
                if decals.ensure_emblem(&garage.disc, &key, &mut images).is_some() {
                    decals.active_image_key = Some(key.clone());
                    Ok(key)
                } else {
                    Err(anyhow::anyhow!("unknown 360 emblem `{emblem_name}`"))
                }
            } else {
                decals.import_image_file(&cli_path, &mut images)
            };
            match key_res {
                Ok(key) => {
                    if let Some((pos, nrm)) = find_core_chest_anchor(&cache) {
                        let id = decals.next_id;
                        decals.next_id += 1;
                        let tan = stable_tangent(nrm, None);
                        decals.layers.push(PlacedDecal {
                            id,
                            slot: DecalSlot::Core,
                            image_key: key,
                            position: pos.into(),
                            normal: nrm.into(),
                            tangent: tan.into(),
                            scale: 0.78,
                            rotation: 0.0,
                            depth: 0.70,
                            opacity: 1.0,
                            tint: [255, 255, 255, 255],
                            mirror_x: false,
                            flip_h: false,
                        });
                        decals.selected_id = Some(id);
                        decals.mark_slot_dirty(DecalSlot::Core, true);
                        garage_state.ui_dirty = true;
                    }
                }
                Err(e) => warn!("--decal {}: {e:#}", cli_path.display()),
            }
        } else {
            decals.cli_decal = Some(cli_path);
        }
    }

    if cache.meshes.is_empty() {
        return;
    }

    // Pre-resolve any referenced decal assets before running the bake loop.
    let referenced_keys: Vec<String> = decals.layers.iter().map(|l| l.image_key.clone()).collect();
    for key in referenced_keys {
        let _ = decals.resolve_asset(&garage.disc, &key, &mut images);
    }

    for slot in DecalSlot::ALL {
        let Some(res) = decals.dirty_slots[slot.index()].take() else {
            continue;
        };
        let has_any = decals
            .layers
            .iter()
            .any(|l| l.slot == slot || (l.mirror_x && l.slot.mirrored_slot() == slot));

        let handle = decals.slot_textures[slot.index()].clone();
        if !has_any {
            if let Some(mut img) = images.get_mut(&handle) {
                *img = crate::paint::empty_decal_image();
            }
        } else {
            // Snapshot assets needed for this slot so `bake_slot_pixels` stays pure.
            let mut local_assets: HashMap<String, DecalAsset> = HashMap::new();
            for l in &decals.layers {
                if !local_assets.contains_key(&l.image_key) {
                    if let Some(a) = decals.peek_asset(&l.image_key) {
                        local_assets.insert(l.image_key.clone(), a.clone());
                    }
                }
            }
            let pixels = bake_slot_pixels(slot, res, &cache, &decals.layers, &mut |k| {
                local_assets.get(k).cloned()
            });
            if let Some(mut img) = images.get_mut(&handle) {
                let mut updated = Image::new(
                    Extent3d {
                        width: res,
                        height: res,
                        depth_or_array_layers: 1,
                    },
                    TextureDimension::D2,
                    pixels,
                    // Stored as premultiplied raw sRGB so `ac_paint.wgsl` blends in sRGB space
                    // identically to 360 `Flver_ACColorDecalColDifSpcBumpIbl.fpo` instructions 131-132.
                    TextureFormat::Rgba8Unorm,
                    RenderAssetUsages::default(),
                );
                updated.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                    address_mode_u: ImageAddressMode::ClampToEdge,
                    address_mode_v: ImageAddressMode::ClampToEdge,
                    mag_filter: ImageFilterMode::Linear,
                    min_filter: ImageFilterMode::Linear,
                    ..default()
                });
                *img = updated;
            }
        }

        // Notify Bevy's MaterialPlugin to rebuild the AcPaint BindGroup on this exact frame so
        // placing, dragging, resizing, rotating, mirroring, or clearing decals updates live.
        let matching_mats: Vec<AssetId<crate::paint::AcPaintMaterial>> = paint_materials
            .iter()
            .filter_map(|(id, mat)| (mat.extension.decal_map == handle).then_some(id))
            .collect();
        let active_flag = if has_any { 1.0 } else { 0.0 };
        for id in matching_mats {
            if let Some(mut mat) = paint_materials.get_mut(id) {
                mat.extension.params.flags.y = active_flag;
            }
        }
    }
}

/// Finds a clean front-facing anchor point centered on the AC's Core chest plate (for `--decal`
/// demo / testing). In rest-pose FLVER model space, the AC chest faces `-Z`.
fn find_core_chest_anchor(cache: &DecalMeshCache) -> Option<(Vec3, Vec3)> {
    let mut front_pts: Vec<(Vec3, Vec3)> = Vec::new();
    let mut min_z = f32::INFINITY;
    for mesh in cache.meshes.iter().filter(|m| m.slot == DecalSlot::Core) {
        for (&p, &n) in mesh.positions.iter().zip(mesh.normals.iter()) {
            if n.z < -0.35 && p.x.abs() < 0.65 {
                min_z = min_z.min(p.z);
                front_pts.push((p, n.normalize_or(-Vec3::Z)));
            }
        }
    }
    if front_pts.is_empty() {
        return None;
    }
    // Average the front chest panel vertices (within 0.45 m of the front-most surface) so the
    // decal centers on the flat armor plate rather than clipping off the bottom edge.
    let mut sum_p = Vec3::ZERO;
    let mut sum_n = Vec3::ZERO;
    let mut count = 0usize;
    for &(p, n) in &front_pts {
        if p.z <= min_z + 0.45 && p.x.abs() < 0.40 {
            sum_p += p;
            sum_n += n;
            count += 1;
        }
    }
    if count == 0 {
        return front_pts.first().copied();
    }
    let avg_p = sum_p / count as f32;
    let avg_n = sum_n.normalize_or(-Vec3::Z);
    // Snap to the closest real chest vertex so the anchor lies directly on the armor skin.
    front_pts
        .into_iter()
        .min_by(|(a, _), (b, _)| {
            a.distance_squared(avg_p)
                .total_cmp(&b.distance_squared(avg_p))
        })
        .map(|(p, n)| (p, (n + avg_n).normalize_or(-Vec3::Z)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_quad_cache(slot: DecalSlot) -> DecalMeshCache {
        // 1x1 meter front-facing quad centered at origin, facing +Z, UV1 filling [0, 1]^2.
        DecalMeshCache {
            meshes: vec![CachedPartMesh {
                slot,
                positions: vec![
                    Vec3::new(-0.5, -0.5, 0.0),
                    Vec3::new(0.5, -0.5, 0.0),
                    Vec3::new(0.5, 0.5, 0.0),
                    Vec3::new(-0.5, 0.5, 0.0),
                ],
                normals: vec![Vec3::Z; 4],
                uv1: vec![
                    Vec2::new(0.0, 1.0),
                    Vec2::new(1.0, 1.0),
                    Vec2::new(1.0, 0.0),
                    Vec2::new(0.0, 0.0),
                ],
                indices: vec![0, 1, 2, 0, 2, 3],
                joint_indices: None,
                joint_weights: None,
                joints: Vec::new(),
                inverse_bindposes: Vec::new(),
            }],
        }
    }

    fn solid_asset(key: &str, rgba: [u8; 4]) -> DecalAsset {
        let mut pixels = Vec::with_capacity(4 * 4 * 4);
        for _ in 0..16 {
            pixels.extend_from_slice(&rgba);
        }
        DecalAsset {
            key: key.to_string(),
            label: key.to_string(),
            width: 4,
            height: 4,
            rgba: Arc::new(pixels),
            thumb: Handle::default(),
        }
    }

    #[test]
    fn slot_mapping_matches_x360_table() {
        assert_eq!(DecalSlot::from_mesh("head", None), Some(DecalSlot::Head));
        assert_eq!(DecalSlot::from_mesh("core", None), Some(DecalSlot::Core));
        assert_eq!(
            DecalSlot::from_mesh("arms", Some("r_kata")),
            Some(DecalSlot::RightArm)
        );
        assert_eq!(
            DecalSlot::from_mesh("arms", Some("l_kata")),
            Some(DecalSlot::LeftArm)
        );
        assert_eq!(DecalSlot::from_mesh("legs", None), Some(DecalSlot::Legs));
        assert_eq!(DecalSlot::from_mesh("armwep_r", None), None);
        // 360 0x8202268c resolution table: [512, 1024, 1024, 1024, 1024]
        assert_eq!(DecalSlot::Head.full_resolution(), 512);
        assert_eq!(DecalSlot::Core.full_resolution(), 1024);
        assert_eq!(DecalSlot::RightArm.full_resolution(), 1024);
        assert_eq!(DecalSlot::LeftArm.full_resolution(), 1024);
        assert_eq!(DecalSlot::Legs.full_resolution(), 1024);
    }

    #[test]
    fn bake_projects_and_mirrors_into_uv1() {
        let cache = test_quad_cache(DecalSlot::Core);
        let red = solid_asset("red", [255, 40, 20, 255]);
        let decal = PlacedDecal {
            id: 1,
            slot: DecalSlot::Core,
            image_key: "red".into(),
            position: [0.25, 0.0, 0.0],
            normal: [0.0, 0.0, 1.0],
            tangent: [1.0, 0.0, 0.0],
            scale: 0.25,
            rotation: 0.0,
            depth: 0.2,
            opacity: 1.0,
            tint: [255, 255, 255, 255],
            mirror_x: true,
            flip_h: false,
        };
        let buf = bake_slot_pixels(DecalSlot::Core, 64, &cache, &[decal], &mut |_| {
            Some(red.clone())
        });
        // Right side (x = +0.25 -> u = 0.75 -> px = 48, py = 32) should be red.
        let right_px = ((32 * 64 + 48) * 4) as usize;
        assert!(buf[right_px] > 240 && buf[right_px + 3] > 240, "right {:?}", &buf[right_px..right_px + 4]);
        // Mirrored left side (x = -0.25 -> u = 0.25 -> px = 16, py = 32) should also be red!
        let left_px = ((32 * 64 + 16) * 4) as usize;
        assert!(buf[left_px] > 240 && buf[left_px + 3] > 240, "left {:?}", &buf[left_px..left_px + 4]);
        // Center (x = 0 -> u = 0.5 -> px = 32, py = 32) should remain transparent!
        let mid_px = ((32 * 64 + 32) * 4) as usize;
        assert_eq!(buf[mid_px + 3], 0, "center {:?}", &buf[mid_px..mid_px + 4]);
    }

    #[test]
    fn premultiplied_src_over_layering_and_opacity() {
        let cache = test_quad_cache(DecalSlot::Head);
        let blue = solid_asset("blue", [0, 0, 255, 255]);
        let white_half = solid_asset("white", [255, 255, 255, 255]);
        let layers = vec![
            PlacedDecal {
                id: 1,
                slot: DecalSlot::Head,
                image_key: "blue".into(),
                position: [0.0, 0.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                tangent: [1.0, 0.0, 0.0],
                scale: 0.6,
                rotation: 0.0,
                depth: 0.2,
                opacity: 1.0,
                tint: [255, 255, 255, 255],
                mirror_x: false,
                flip_h: false,
            },
            PlacedDecal {
                id: 2,
                slot: DecalSlot::Head,
                image_key: "white".into(),
                position: [0.0, 0.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                tangent: [1.0, 0.0, 0.0],
                scale: 0.6,
                rotation: 0.0,
                depth: 0.2,
                opacity: 0.5,
                tint: [255, 255, 255, 255],
                mirror_x: false,
                flip_h: false,
            },
        ];
        let buf = bake_slot_pixels(DecalSlot::Head, 32, &cache, &layers, &mut |k| match k {
            "blue" => Some(blue.clone()),
            "white" => Some(white_half.clone()),
            _ => None,
        });
        let mid = ((16 * 32 + 16) * 4) as usize;
        // 50% white over 100% blue -> ~[128, 128, 255, 255]
        assert!((buf[mid] as i32 - 128).abs() <= 2, "{:?}", &buf[mid..mid + 4]);
        assert!((buf[mid + 1] as i32 - 128).abs() <= 2, "{:?}", &buf[mid..mid + 4]);
        assert!(buf[mid + 2] >= 253 && buf[mid + 3] >= 253, "{:?}", &buf[mid..mid + 4]);
    }

    #[test]
    fn x360_disc_core_bakes_retail_emblem() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let iso = acvd_formats::vfs::default_disc(&root);
        if !iso.is_file() {
            return;
        }
        let disc = Disc::open(&iso).unwrap();
        let state = DecalState::new(None);
        assert!(
            state.disc_emblems.len() >= 300,
            "expected ~306 retail 360 emblems, got {}",
            state.disc_emblems.len()
        );
        let first = state
            .disc_emblems
            .iter()
            .find(|e| e.name == "im0001")
            .expect("im0001");
        let mut images = Assets::<Image>::default();
        let asset = load_disc_emblem(&disc, first, &mut images).expect("decode im0001 BC3");
        assert_eq!((asset.width, asset.height), (256, 256));

        // Load a real Core FLVER from the first valid AC design and verify UV1 baking produces opaque premultiplied texels.
        let core_path = acvd_data::generated::ac_unit::AC_ASSEMBLY_DESIGN_ST_FILES
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .find_map(|row| {
                let built = crate::assemble::assemble(
                    acvd_data::generated::assembly::AC_ASSEMBLY_DESIGN_ST_SLOTS,
                    &row.data,
                );
                built
                    .placements
                    .into_iter()
                    .find(|p| p.column == "core")
                    .map(|p| p.model.path)
            })
            .expect("core placement");
        let flver_bytes = acvd_formats::vfs::open(&disc, core_path).expect("open core flver");
        let flver = acvd_formats::flver::read(&flver_bytes).expect("parse flver");

        let mut cache = DecalMeshCache::default();
        for mesh in &flver.meshes {
            let Ok(verts) = flver.vertices(&flver_bytes, mesh) else {
                continue;
            };
            let Some(uv1) = verts.uvs.get(1).or_else(|| verts.uvs.first()) else {
                continue;
            };
            let Some(&fs_idx) = mesh.face_sets.first() else {
                continue;
            };
            let Ok(tris) = flver.triangles(&flver_bytes, &flver.face_sets[fs_idx]) else {
                continue;
            };
            if tris.is_empty() {
                continue;
            }
            let indices: Vec<u32> = tris.into_iter().flat_map(|t| t).collect();
            let normals = if verts.normals.len() == verts.positions.len() {
                verts.normals.iter().copied().map(Vec3::from).collect()
            } else {
                vec![Vec3::Y; verts.positions.len()]
            };
            cache.meshes.push(CachedPartMesh {
                slot: DecalSlot::Core,
                positions: verts.positions.iter().copied().map(Vec3::from).collect(),
                normals,
                uv1: uv1.iter().copied().map(Vec2::from).collect(),
                indices,
                joint_indices: None,
                joint_weights: None,
                joints: Vec::new(),
                inverse_bindposes: Vec::new(),
            });
        }
        assert!(!cache.meshes.is_empty());
        let (pos, nrm) = find_core_chest_anchor(&cache).expect("core chest anchor");
        let layer = PlacedDecal {
            id: 1,
            slot: DecalSlot::Core,
            image_key: asset.key.clone(),
            position: pos.into(),
            normal: nrm.into(),
            tangent: stable_tangent(nrm, None).into(),
            scale: 0.78,
            rotation: 0.0,
            depth: 0.70,
            opacity: 1.0,
            tint: [255, 255, 255, 255],
            mirror_x: false,
            flip_h: false,
        };
        let buf = bake_slot_pixels(DecalSlot::Core, 256, &cache, &[layer], &mut |_| {
            Some(asset.clone())
        });
        let opaque_texels = buf.chunks_exact(4).filter(|px| px[3] > 200).count();
        assert!(
            opaque_texels > 150,
            "expected baked UV1 emblem coverage on cr0001 core, got only {opaque_texels} texels"
        );
    }
}
