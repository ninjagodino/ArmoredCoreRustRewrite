//! Sheet data model. Hand-authored sheets live in `sheets/*.csv` (committed); disc-derived
//! sheets live in `private/sheets` (ignored) and are always regenerated from the owned disc.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

pub struct Paths {
    pub root: PathBuf,
}

impl Paths {
    pub fn sheets(&self) -> PathBuf {
        self.root.join("sheets")
    }
    pub fn private_sheets(&self) -> PathBuf {
        self.root.join("private").join("sheets")
    }
    pub fn json(&self) -> PathBuf {
        self.private_sheets().join("json")
    }
    pub fn preflight(&self) -> PathBuf {
        self.root.join("private").join("preflight")
    }
    pub fn paramdex(&self, game: &str) -> PathBuf {
        self.root.join("external").join("paramdex").join(game).join("Defs")
    }
    pub fn archives(&self) -> PathBuf {
        self.json().join("archives")
    }
    pub fn textures(&self) -> PathBuf {
        self.json().join("textures")
    }
    pub fn models(&self) -> PathBuf {
        self.json().join("models")
    }
    pub fn motions(&self) -> PathBuf {
        self.json().join("motions")
    }
    pub fn ac_parts(&self) -> PathBuf {
        self.json().join("_ac_parts.json")
    }
    pub fn tuning(&self) -> PathBuf {
        self.json().join("_tuning.json")
    }
    pub fn generated(&self) -> PathBuf {
        self.root.join("crates").join("acvd-data").join("src").join("generated")
    }
}

// ---- hand-authored sheets -------------------------------------------------------------

/// `sheets/groups.csv`: first rule whose `def_prefix` starts the def file stem wins.
#[derive(Debug, Clone, Deserialize)]
pub struct GroupRule {
    pub def_prefix: String,
    pub group: String,
}

/// `sheets/column_overrides.csv`: per-column corrections where the disc def is unnamed or wrong.
/// `index` is the def field index. `drop` removes a def column the PARAM data does not contain.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ColumnOverride {
    pub param_type: String,
    pub index: usize,
    pub name: String,
    pub bits: Option<u8>,
    pub drop: bool,
    pub evidence: String,
}

/// `sheets/type_aliases.csv`: PARAM type strings whose def ships under a different type string.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TypeAlias {
    pub param_type: String,
    pub def_type: String,
    pub evidence: String,
}

/// `sheets/excluded_files.csv`: PARAM files deliberately not decoded (no def describes them).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExcludedFile {
    pub file: String,
    pub reason: String,
}

/// `sheets/container_exceptions.csv`: container findings explained by evidence, reported as
/// warnings instead of errors. `check` is the preflight check name the row answers.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ContainerException {
    pub path: String,
    pub check: String,
    pub evidence: String,
}

/// `sheets/formats.csv`: one row per file extension present on the disc or inside its binders.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FormatRow {
    pub extension: String,
    pub magic: String,
    pub format: String,
    pub system: String,
    pub parser: String,
    pub evidence: String,
}

/// `sheets/systems.csv`: one row per game system the rewrite must cover.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SystemRow {
    pub system: String,
    pub description: String,
    pub data_sources: String,
    pub data_status: String,
    pub runtime_status: String,
}

/// `sheets/target.csv`: To-Do 1 target identification record.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TargetRow {
    pub key: String,
    pub value: String,
    pub evidence: String,
}

pub fn read_csv<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    let mut rdr = csv::Reader::from_path(path).with_context(|| format!("opening {}", path.display()))?;
    rdr.deserialize()
        .enumerate()
        .map(|(i, r)| r.with_context(|| format!("{} data row {}", path.display(), i + 1)))
        .collect()
}

// ---- disc-derived sheets (JSON) -------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupSheet {
    pub group: String,
    pub tables: Vec<TableSheet>,
}

/// One PARAM type: columns come from its PARAMDEF, rows from every PARAM file of that type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSheet {
    pub param_type: String,
    pub def_file: String,
    pub def_format_version: u16,
    pub def_data_version: u16,
    pub row_size: Option<usize>,
    pub layout_error: Option<String>,
    pub paramdex: Option<String>,
    pub paramdex_mismatch: Option<String>,
    pub columns: Vec<Column>,
    pub instances: Vec<Instance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Column {
    pub index: usize,
    pub name: Option<String>,
    pub name_source: Option<String>,
    pub ident: Option<String>,
    pub prim: Option<String>,
    pub count: usize,
    pub bits: Option<u8>,
    pub offset: Option<usize>,
    pub bit_offset: Option<u8>,
    pub enum_type: Option<String>,
    pub display_name: String,
    pub display_format: String,
    pub default: f32,
    pub min: f32,
    pub max: f32,
    pub description: String,
    pub problem: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    pub id: String,
    pub file: String,
    pub data_version: u16,
    pub flags: [u8; 3],
    pub observed_stride: Option<u32>,
    pub error: Option<String>,
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    pub id: u32,
    pub name: String,
    pub values: Option<Vec<Value>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Inventory {
    pub extensions: Vec<ExtensionCount>,
    pub orphan_params: Vec<String>,
    pub unused_defs: Vec<String>,
    pub def_errors: Vec<String>,
    pub duplicates: Vec<Duplicate>,
    pub excluded: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionCount {
    pub extension: String,
    pub files: usize,
    pub bytes: u64,
    pub magics: Vec<(String, usize)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Duplicate {
    pub file: String,
    pub original: String,
    pub identical: bool,
}

// ---- disc-derived sheets: containers ---------------------------------------------------

/// `private/sheets/json/_archives.json`: standalone DCX files and the extensions found inside binders.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArchiveIndex {
    pub dcx: Vec<DcxRow>,
    pub entry_extensions: Vec<ExtensionCount>,
    pub groups: Vec<String>,
    pub texture_groups: Vec<String>,
    #[serde(default)]
    pub model_groups: Vec<String>,
    #[serde(default)]
    pub motion_groups: Vec<String>,
}

/// One DCX file on the disc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DcxRow {
    pub file: String,
    pub uncompressed_size: u32,
    pub compressed_size: u32,
    pub chunks: usize,
    /// Bytes after the last chunk that the DCX header does not cover.
    pub trailing_bytes: usize,
    pub inner_magic: String,
    pub error: Option<String>,
}

/// `private/sheets/json/archives/<group>.json`: binders grouped by top-level disc directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveGroup {
    pub group: String,
    pub binders: Vec<BinderSheet>,
}

/// One BND3 binder. `path` is an asset path (`disc/file.bnd.dcx|inner.bnd` for nested binders).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinderSheet {
    pub path: String,
    pub version: String,
    pub raw_format: u8,
    pub format: u8,
    pub big_endian: u8,
    pub bit_big_endian: u8,
    pub headers_end: u32,
    /// First byte outside entry data that the parsed header, table and names do not explain.
    pub unexplained_byte: Option<usize>,
    pub error: Option<String>,
    pub entries: Vec<EntryRow>,
}

/// One binder entry. `encoding` is how the stored bytes become `size` bytes of contents:
/// `raw`, `zlib`, `dcx` or `zlib+dcx`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryRow {
    pub index: usize,
    pub id: Option<i32>,
    pub name: Option<String>,
    pub flags: u8,
    pub encoding: String,
    pub stored_size: u32,
    pub size: usize,
    /// For DCX entries: bytes after the last chunk that the DCX header does not cover.
    pub dcx_trailing_bytes: usize,
    pub magic: String,
    pub ext: String,
    pub error: Option<String>,
}

/// `private/sheets/json/textures/<group>.json`: every TPF, grouped by top-level disc directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextureGroup {
    pub group: String,
    pub packs: Vec<TpfSheet>,
}

/// One TPF. `path` is an asset path (a disc file, or a binder entry joined with `|`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TpfSheet {
    pub path: String,
    /// TPF platform byte (`tpf::PLATFORM_X360`, or `tpf::PLATFORM_PS3` for the 4 linear packs).
    pub platform: u8,
    pub flag2: u8,
    pub encoding: u8,
    pub error: Option<String>,
    pub textures: Vec<TextureRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextureRow {
    pub index: usize,
    pub name: String,
    pub format: u8,
    pub kind: u8,
    pub mipmaps: u8,
    pub levels: u32,
    pub faces: u32,
    pub width: u16,
    pub height: u16,
    pub size: u32,
    pub flags1: u8,
    pub unk1: u32,
    pub unk2: Option<u32>,
    pub floats: Vec<f32>,
    /// The texture's data runs past the end of the pack.
    #[serde(default)]
    pub truncated: bool,
}

/// `private/sheets/json/motions/<group>.json`: every motion binder (`*_a.bnd.dcx`) and its `.ani` clips.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MotionGroup {
    pub group: String,
    pub sets: Vec<MotionSet>,
}

/// One motion binder. Unnamed clips borrow the skeleton of the set's named clips by index.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MotionSet {
    pub path: String,
    pub clips: Vec<ClipRow>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClipRow {
    pub name: String,
    pub error: Option<String>,
    pub named: bool,
    pub frames: u32,
    pub bones: usize,
    pub quantized: bool,
    /// Track count per kind, in `acvd_formats::ani::LAYOUTS` order.
    pub kinds: Vec<usize>,
    pub empty_tracks: usize,
    pub cameras: usize,
    pub keys: usize,
    /// Rest bones, named clips only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skeleton: Vec<ClipBone>,
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ClipBone {
    pub name: String,
    pub parent: Option<u16>,
    pub translation: [f32; 3],
}

/// `sheets/ac_motion.csv`: the motion set of an AC is named by the `table` row whose `key` column
/// equals the `slot` part's `field` (`ac_part_fields.csv`) and that passes `filter`
/// (`<column>=<value>`); its `name_column` cell `<name>` gives the binder
/// `<directory><name>/<name><binder>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcMotionRow {
    pub slot: String,
    pub field: String,
    pub table: String,
    pub key: String,
    pub filter: String,
    pub name_column: String,
    pub directory: String,
    pub binder: String,
    pub evidence: String,
}

impl AcMotionRow {
    pub fn binder_path(&self, name: &str) -> String {
        format!("{}{name}/{name}{}", self.directory, self.binder)
    }
}

/// `sheets/ac_states.csv`: a locomotion state (one row per `direction`, 0 = forward, clockwise
/// in eighths, for directional states) and the `param/acmotion.bin` row playing it; `name` must
/// equal that row's name on the disc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcStateRow {
    pub state: String,
    pub direction: Option<u8>,
    pub row: u32,
    pub name: String,
    pub evidence: String,
}

/// `sheets/tuning_roots.csv`: the USRDIR directory a `system/paramlist.xml` path variable names.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuningRootRow {
    pub variable: String,
    pub directory: String,
    pub evidence: String,
}

/// `sheets/tuning_fields.csv`: an identifier for field `index` of tuning file `file`, whose disc
/// label must equal `label`. `status` other than `done` reports the unit reading as pending.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuningFieldRow {
    pub file: String,
    pub index: usize,
    pub label: String,
    pub ident: String,
    pub unit: String,
    pub status: String,
    pub evidence: String,
}

/// `private/sheets/json/_tuning.json`: every `<Bin>`/`<Dbp>` pair `system/paramlist.xml` lists.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TuningSheet {
    pub source: String,
    pub error: Option<String>,
    pub files: Vec<TuningFile>,
}

/// One tuning binary: `name` is its `<Bin>` file stem as the list spells it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TuningFile {
    pub name: String,
    pub desc: String,
    pub bin: String,
    pub dbp: String,
    pub error: Option<String>,
    /// Variables the list uses that `tuning_roots.csv` maps.
    pub roots: Vec<String>,
    pub extra_strings: usize,
    /// Bytes of the `.bin` past the fields the `.dbp` describes, and whether any is non-zero.
    pub tail: usize,
    pub tail_nonzero: bool,
    pub fields: Vec<TuningValue>,
}

/// One field: `value` from the `.bin`, the rest from the `.dbp` (`menu_value` is the value the
/// debug menu was saved with).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuningValue {
    pub kind: String,
    pub label: String,
    pub format: String,
    pub value: f64,
    pub menu_value: f64,
    pub step: f64,
    pub min: f64,
    pub max: f64,
}

/// `private/sheets/json/models/<group>.json`: every FLVER, grouped by top-level disc directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelGroup {
    pub group: String,
    pub models: Vec<ModelSheet>,
}

/// One FLVER. `path` is an asset path (binder entries joined with `|`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelSheet {
    pub path: String,
    pub version: u32,
    pub error: Option<String>,
    pub bbox_min: [f32; 3],
    pub bbox_max: [f32; 3],
    pub dummies: usize,
    pub bones: usize,
    #[serde(default)]
    pub skeleton: Vec<BoneRow>,
    #[serde(default)]
    pub sockets: Vec<SocketRow>,
    pub materials: Vec<MaterialRow>,
    pub meshes: Vec<MeshRow>,
}

impl ModelSheet {
    /// Name of the root bone above `bone`.
    pub fn root_of(&self, mut bone: usize) -> Option<&str> {
        for _ in 0..self.skeleton.len() {
            let b = self.skeleton.get(bone)?;
            match usize::try_from(b.parent) {
                Ok(p) => bone = p,
                Err(_) => return Some(&b.name),
            }
        }
        None
    }
}

/// One FLVER bone: its rest transform relative to `parent` (-1 for a root), and its origin in
/// model space.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoneRow {
    pub name: String,
    pub parent: i16,
    pub translation: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 3],
    #[serde(default)]
    pub origin: [f32; 3],
}

/// One FLVER dummy point, in model space. `color[0]` is an attach socket id on AC parts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketRow {
    pub color: [u8; 4],
    pub bone: i16,
    pub position: [f32; 3],
}

/// `sheets/ac_part_categories.csv`: one row per non-empty category of `param/acvparts.bin`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcPartCategoryRow {
    pub category: usize,
    pub name: String,
    pub model_dir: String,
    /// `;`-separated model name prefixes (`hr;hl`).
    pub prefixes: String,
    pub record_size: usize,
    pub evidence: String,
}

impl AcPartCategoryRow {
    pub fn prefixes(&self) -> impl Iterator<Item = &str> {
        self.prefixes.split(';').map(str::trim).filter(|p| !p.is_empty())
    }

    pub fn model_path(&self, prefix: &str, model_id: u16) -> String {
        let code = format!("{prefix}{model_id:04}");
        format!("model/ac/parts/{}/{code}/{code}_m.bnd.dcx|{code}.flv", self.model_dir)
    }
}

/// `sheets/assembly_slots.csv`: where one assembly column's part attaches. `socket` is a dummy
/// id (`color[0]`) on the parent slot's model, or `bone:<name>` for one of its bones; `root` names
/// the root bones of this slot's model that land on it (see [`AssemblySlotRow::roots`]). The
/// anchor slot has `none` for parent and socket. `orient` is `none` or the attach-info flag
/// byte (360 `0x8288cea0`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssemblySlotRow {
    pub slot: String,
    pub table: String,
    pub column: String,
    pub category: usize,
    pub prefix: String,
    pub parent: String,
    pub socket: String,
    pub root: String,
    /// `none`, or the flag byte. Empty (older sheets) means no rotation.
    #[serde(default)]
    pub orient: String,
    pub evidence: String,
}

/// `root` cell meaning every root bone of the slot's model.
pub const ALL_ROOTS: &str = "*";

impl AssemblySlotRow {
    /// The root bones this slot moves on `m`: the `;`-separated `root` cell, or every root of
    /// `m` for `*`. The first lands on the socket; a booster's second root uses dummy 25 or 26.
    pub fn roots<'a>(&'a self, m: &'a ModelSheet) -> Vec<&'a str> {
        if self.root.trim() == ALL_ROOTS {
            m.skeleton.iter().filter(|b| b.parent < 0).map(|b| b.name.as_str()).collect()
        } else {
            self.root.split(';').map(str::trim).collect()
        }
    }
}

/// `param/acvparts.bin` as read with the record sizes from `ac_part_categories.csv`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AcPartsSheet {
    pub path: String,
    pub error: Option<String>,
    pub counts: Vec<u16>,
    pub end: usize,
    pub size: usize,
    pub tail: Option<usize>,
    pub records: Vec<AcPartRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcPartRow {
    pub category: usize,
    pub index: usize,
    pub offset: usize,
    pub id: u16,
    pub model_id: u16,
    /// Values of the `ac_part_fields.csv` fields of this record's category.
    #[serde(default)]
    pub fields: BTreeMap<String, f64>,
}

/// `sheets/ac_part_fields.csv`: one decoded big-endian field of a category's `acvparts.bin`
/// records, at byte `offset` from the record start.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcPartFieldRow {
    pub category: usize,
    pub offset: usize,
    #[serde(rename = "type")]
    pub ty: String,
    pub name: String,
    pub evidence: String,
}

impl AcPartFieldRow {
    pub fn size(&self) -> Option<usize> {
        match self.ty.as_str() {
            "u8" | "i8" => Some(1),
            "u16" | "i16" => Some(2),
            "u32" | "i32" | "f32" => Some(4),
            _ => None,
        }
    }

    pub fn read(&self, record: &[u8]) -> Option<f64> {
        let b = record.get(self.offset..self.offset + self.size()?)?;
        let w = |b: &[u8]| [b[0], b[1], b[2], b[3]];
        Some(match self.ty.as_str() {
            "u8" => f64::from(b[0]),
            "i8" => f64::from(b[0] as i8),
            "u16" => f64::from(u16::from_be_bytes([b[0], b[1]])),
            "i16" => f64::from(i16::from_be_bytes([b[0], b[1]])),
            "u32" => f64::from(u32::from_be_bytes(w(b))),
            "i32" => f64::from(i32::from_be_bytes(w(b))),
            _ => f64::from(f32::from_be_bytes(w(b))),
        })
    }
}

/// `sheets/design_parts.csv`: the part column of a design table that `AcCtrlParamCalc.lua`'s
/// slot number `slot` reads, and the `acvparts.bin` category its ids belong to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesignPartRow {
    pub slot: usize,
    pub table: String,
    pub column: String,
    pub category: usize,
    pub evidence: String,
}

/// `sheets/unbuildable_designs.csv`: a design row whose parts no catalogue on the disc defines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnbuildableDesignRow {
    pub id: u32,
    pub name: String,
    pub evidence: String,
}

/// `sheets/ac_ctrl_calc.csv`: one value of an AC's control parameters, in evaluation order.
/// `param` is the `Scr_SetParam` id the game stores it under (`-` for intermediates); `expr`
/// is an f32 expression over earlier names, `<design column>.<ac_part_fields name>` and
/// `<tuning file>.<tuning_fields ident>` (see `calc`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcCtrlRow {
    pub name: String,
    pub param: String,
    pub unit: String,
    pub expr: String,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialRow {
    pub name: String,
    pub mtd: String,
    /// `(texture type, texture path)` in material order.
    pub textures: Vec<(String, String)>,
}

/// One mesh. `members` lists every vertex layout member as `type/semantic`.
/// `clockwise` / `counter_clockwise` count main face set triangles by winding around the
/// stored normals; `unreferenced` counts vertices no face set uses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshRow {
    pub index: usize,
    pub material: i32,
    pub dynamic: u8,
    pub vertices: usize,
    pub members: Vec<String>,
    pub vertex_error: Option<String>,
    /// Model-space vertex bounds (after `Flver::to_model_space`).
    #[serde(default)]
    pub bounds: Option<([f32; 3], [f32; 3])>,
    /// Vertices whose bone index leaves the mesh's or the model's bone table.
    #[serde(default)]
    pub unboned: usize,
    pub unreferenced: usize,
    pub clockwise: usize,
    pub counter_clockwise: usize,
    pub face_sets: Vec<FaceSetRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaceSetRow {
    pub index: usize,
    pub flags: u32,
    pub strip: u8,
    pub index_size: i32,
    pub indices: usize,
    pub triangles: usize,
    pub degenerate: usize,
    pub max_index: Option<u32>,
    pub error: Option<String>,
}

/// `sheets/vertex_types.csv`: FLVER layout member (type, semantic) pairs and how they decode.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VertexTypeRow {
    pub kind: String,
    pub semantic: u32,
    pub name: String,
    pub size: usize,
    pub evidence: String,
}

/// `sheets/ani_track_kinds.csv`: `.ani` track kinds and the row layout each selects.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AniTrackKindRow {
    pub kind: u32,
    pub width: usize,
    pub translation: bool,
    pub rotation_tangents: bool,
    pub scale: bool,
    pub row_size: usize,
    pub evidence: String,
}

/// `sheets/texture_formats.csv`: TPF format codes and the GPU block format they hold.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TextureFormatRow {
    pub code: u8,
    pub name: String,
    pub block_bytes: u32,
    pub evidence: String,
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(value)?).with_context(|| format!("writing {}", path.display()))
}

pub fn load_groups(paths: &Paths) -> Result<Vec<GroupSheet>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(paths.json()).context("no private/sheets/json; run `extract` first")? {
        let path = entry?.path();
        if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('_')) {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            out.push(read_json(&path)?);
        }
    }
    out.sort_by(|a: &GroupSheet, b| a.group.cmp(&b.group));
    Ok(out)
}

pub fn load_archive_groups(paths: &Paths) -> Result<Vec<ArchiveGroup>> {
    let index: ArchiveIndex = read_json(&paths.json().join("_archives.json"))?;
    index.groups.iter().map(|g| read_json(&paths.archives().join(format!("{g}.json")))).collect()
}

pub fn load_texture_groups(paths: &Paths) -> Result<Vec<TextureGroup>> {
    let index: ArchiveIndex = read_json(&paths.json().join("_archives.json"))?;
    index.texture_groups.iter().map(|g| read_json(&paths.textures().join(format!("{g}.json")))).collect()
}

pub fn load_model_groups(paths: &Paths) -> Result<Vec<ModelGroup>> {
    let index: ArchiveIndex = read_json(&paths.json().join("_archives.json"))?;
    index.model_groups.iter().map(|g| read_json(&paths.models().join(format!("{g}.json")))).collect()
}

pub fn load_motion_groups(paths: &Paths) -> Result<Vec<MotionGroup>> {
    let index: ArchiveIndex = read_json(&paths.json().join("_archives.json"))?;
    index.motion_groups.iter().map(|g| read_json(&paths.motions().join(format!("{g}.json")))).collect()
}
