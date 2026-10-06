//! Preflight: overlaps every sheet (hand-authored and disc-derived), checkmarks every
//! row x column intersection, and reports everything that will fail or is unimplemented.
//! Errors block `gen`; fixes belong in the sheets, never in generated code.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use crate::model::*;
use crate::names::{pascal, shouty};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Severity {
    Error,
    Warning,
    Pending,
}

#[derive(Debug, Serialize)]
struct Issue {
    severity: Severity,
    check: &'static str,
    sheet: String,
    row: String,
    column: String,
    detail: String,
}

#[derive(Serialize)]
struct Checkmark {
    kind: &'static str,
    group: String,
    param_type: String,
    instances: usize,
    rows: usize,
    columns: usize,
    intersections: usize,
    checked: usize,
    unchecked: usize,
}

pub struct Summary {
    pub errors: usize,
}

#[derive(Default)]
struct Report {
    issues: Vec<Issue>,
}

impl Report {
    fn add(&mut self, severity: Severity, check: &'static str, sheet: impl Into<String>, row: impl Into<String>, column: impl Into<String>, detail: impl Into<String>) {
        self.issues.push(Issue { severity, check, sheet: sheet.into(), row: row.into(), column: column.into(), detail: detail.into() });
    }
}

fn blank_cells<T: Serialize>(rep: &mut Report, sheet: &str, key: impl Fn(&T) -> String, rows: &[T]) {
    for r in rows {
        let Ok(Value::Object(map)) = serde_json::to_value(r) else { continue };
        for (col, v) in map {
            if v.as_str().is_some_and(|s| s.trim().is_empty()) {
                rep.add(Severity::Error, "sheet.blank_cell", sheet, key(r), col, "unchecked intersection: cell is empty");
            }
        }
    }
}

/// `done` and `n/a` are finished states; anything else is reported as pending work.
fn is_done(status: &str) -> bool {
    let s = status.trim();
    s.eq_ignore_ascii_case("done") || s.eq_ignore_ascii_case("n/a")
}

pub fn run(paths: &Paths) -> Result<Summary> {
    let mut rep = Report::default();
    let groups = load_groups(paths)?;
    let inv: Inventory = read_json(&paths.json().join("_inventory.json"))?;
    let formats: Vec<FormatRow> = read_csv(&paths.sheets().join("formats.csv"))?;
    let systems: Vec<SystemRow> = read_csv(&paths.sheets().join("systems.csv"))?;
    let target: Vec<TargetRow> = read_csv(&paths.sheets().join("target.csv"))?;

    // ---- hand-authored sheets ----
    blank_cells(&mut rep, "target.csv", |r: &TargetRow| r.key.clone(), &target);
    blank_cells(&mut rep, "formats.csv", |r: &FormatRow| r.extension.clone(), &formats);
    blank_cells(&mut rep, "systems.csv", |r: &SystemRow| r.system.clone(), &systems);

    let system_names: HashSet<&str> = systems.iter().map(|s| s.system.as_str()).collect();
    let mut format_exts: HashMap<&str, &FormatRow> = HashMap::new();
    for f in &formats {
        if format_exts.insert(f.extension.as_str(), f).is_some() {
            rep.add(Severity::Error, "formats.duplicate", "formats.csv", &f.extension, "extension", "extension listed twice");
        }
        for sys in f.system.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            if !system_names.contains(sys) {
                rep.add(Severity::Error, "formats.unknown_system", "formats.csv", &f.extension, "system", format!("`{sys}` has no row in systems.csv"));
            }
        }
        if !is_done(&f.parser) {
            rep.add(Severity::Pending, "formats.parser", "formats.csv", &f.extension, "parser", &f.parser);
        }
    }
    let archive_index: ArchiveIndex = read_json(&paths.json().join("_archives.json"))?;
    let disc_exts: HashSet<&str> =
        inv.extensions.iter().chain(&archive_index.entry_extensions).map(|e| e.extension.as_str()).collect();
    for (e, place) in inv.extensions.iter().map(|e| (e, "on disc")).chain(archive_index.entry_extensions.iter().map(|e| (e, "inside binders"))) {
        if !format_exts.contains_key(e.extension.as_str()) {
            let magics = e.magics.iter().map(|(m, n)| format!("{m}x{n}")).collect::<Vec<_>>().join(", ");
            rep.add(Severity::Error, "formats.missing", "formats.csv", &e.extension, "", format!("{} files {place} ({magics}) with no formats row", e.files));
        }
    }
    for f in &formats {
        if !disc_exts.contains(f.extension.as_str()) {
            rep.add(Severity::Warning, "formats.not_on_disc", "formats.csv", &f.extension, "", "row has no matching files on disc");
        }
    }
    for s in &systems {
        if !is_done(&s.data_status) {
            rep.add(Severity::Pending, "systems.data", "systems.csv", &s.system, "data_status", &s.data_status);
        }
        if !is_done(&s.runtime_status) {
            rep.add(Severity::Pending, "systems.runtime", "systems.csv", &s.system, "runtime_status", &s.runtime_status);
        }
    }

    // ---- disc inventory ----
    for e in &inv.def_errors {
        rep.add(Severity::Error, "def.unreadable", "_inventory", e, "", "PARAMDEF could not be parsed");
    }
    for o in &inv.orphan_params {
        rep.add(Severity::Error, "param.no_def", "_inventory", o, "", "PARAM type has no PARAMDEF; add a def sheet");
    }
    for d in inv.duplicates.iter().filter(|d| !d.identical) {
        rep.add(Severity::Warning, "param.duplicate_differs", "_inventory", &d.file, "", format!("differs from {}", d.original));
    }

    // ---- correction sheets must still apply to something on the disc ----
    let excluded: Vec<ExcludedFile> = read_csv(&paths.sheets().join("excluded_files.csv"))?;
    for x in &excluded {
        if inv.excluded.iter().any(|f| f.eq_ignore_ascii_case(&x.file)) {
            rep.add(Severity::Pending, "file.excluded", "excluded_files.csv", &x.file, "", &x.reason);
        } else {
            rep.add(Severity::Error, "sheet.stale_row", "excluded_files.csv", &x.file, "", "no such PARAM file on disc");
        }
    }
    let tables: HashMap<&str, &TableSheet> = groups.iter().flat_map(|g| &g.tables).map(|t| (t.param_type.as_str(), t)).collect();
    let aliases: Vec<TypeAlias> = read_csv(&paths.sheets().join("type_aliases.csv"))?;
    for a in &aliases {
        if !tables.contains_key(a.param_type.as_str()) {
            rep.add(Severity::Error, "sheet.stale_row", "type_aliases.csv", &a.param_type, "", "alias did not resolve to a table");
        }
    }
    let overrides: Vec<ColumnOverride> = read_csv(&paths.sheets().join("column_overrides.csv"))?;
    for o in &overrides {
        let present = tables.get(o.param_type.as_str()).map(|t| t.columns.iter().any(|c| c.index == o.index));
        let applied = match present {
            None => false,
            Some(present) => present != o.drop,
        };
        if !applied {
            rep.add(Severity::Error, "sheet.stale_row", "column_overrides.csv", format!("{} #{}", o.param_type, o.index), "", "override does not apply to any column");
        }
    }

    // ---- param tables: checkmark every row x column ----
    let mut marks = Vec::new();
    for g in &groups {
        let mut idents: HashMap<String, String> = HashMap::new();
        for t in &g.tables {
            let names = std::iter::once((pascal(&t.param_type), t.param_type.clone()))
                .chain(std::iter::once((format!("{}_FILES", shouty(&t.param_type)), t.param_type.clone())))
                .chain(t.instances.iter().map(|i| (shouty(&i.id), i.file.clone())));
            for (ident, owner) in names {
                if let Some(prev) = idents.insert(ident.clone(), owner.clone()) {
                    rep.add(Severity::Error, "gen.duplicate_ident", &g.group, &owner, "", format!("generated name `{ident}` also used by {prev}"));
                }
            }
        }
        if g.group == "_unassigned" {
            for t in &g.tables {
                rep.add(Severity::Error, "group.unassigned", "groups.csv", &t.def_file, "", "no def_prefix rule matches this def");
            }
        }
        for t in &g.tables {
            let sheet = format!("{}/{}", g.group, t.param_type);
            if let Some(e) = &t.layout_error {
                rep.add(Severity::Error, "table.layout", &sheet, "", "", e);
            }
            if let Some(m) = &t.paramdex_mismatch {
                rep.add(Severity::Warning, "table.paramdex_mismatch", &sheet, "", "", m);
            }

            let mut col_ok = vec![true; t.columns.len()];
            let mut seen: HashMap<&str, usize> = HashMap::new();
            for (pos, c) in t.columns.iter().enumerate() {
                let col = format!("#{} {}", c.index, c.name.as_deref().unwrap_or(&c.display_name));
                match c.name.as_deref() {
                    None => {
                        col_ok[pos] = false;
                        rep.add(Severity::Error, "column.unnamed", &sheet, "", &col, "no name from disc, Paramdex or column_overrides.csv");
                    }
                    Some(n) if !n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') => {
                        col_ok[pos] = false;
                        rep.add(Severity::Error, "column.non_ascii_name", &sheet, "", &col, format!("name `{n}` from {} needs an English override", c.name_source.as_deref().unwrap_or("?")));
                    }
                    Some(_) => {}
                }
                if let Some(p) = &c.problem {
                    col_ok[pos] = false;
                    rep.add(Severity::Error, "column.problem", &sheet, "", &col, p);
                }
                if let Some(id) = c.ident.as_deref() {
                    if let Some(prev) = seen.insert(id, c.index) {
                        col_ok[pos] = false;
                        rep.add(Severity::Error, "column.duplicate_ident", &sheet, "", &col, format!("identifier `{id}` already used by column #{prev}"));
                    }
                }
                if c.offset.is_none() {
                    col_ok[pos] = false;
                }
            }

            let mut rows = 0;
            let mut checked = 0;
            let mut out_of_range: BTreeMap<usize, usize> = BTreeMap::new();
            for inst in &t.instances {
                if let Some(e) = &inst.error {
                    rep.add(Severity::Error, "instance.unreadable", &sheet, &inst.file, "", e);
                }
                if let (Some(stride), Some(size)) = (inst.observed_stride, t.row_size) {
                    if stride as usize != size {
                        rep.add(Severity::Error, "instance.stride", &sheet, &inst.file, "", format!("rows are {stride} bytes apart, sheet columns total {size}"));
                    }
                }
                if inst.data_version != t.def_data_version {
                    rep.add(Severity::Warning, "instance.data_version", &sheet, &inst.file, "", format!("PARAM data version {} vs def {}", inst.data_version, t.def_data_version));
                }
                let mut ids = HashSet::new();
                for r in &inst.rows {
                    rows += 1;
                    if !ids.insert(r.id) {
                        rep.add(Severity::Warning, "row.duplicate_id", &sheet, format!("{} id {}", inst.file, r.id), "", "row id repeats within file");
                    }
                    match &r.values {
                        Some(v) if v.len() == t.columns.len() => {
                            for (pos, (c, val)) in t.columns.iter().zip(v).enumerate() {
                                if !col_ok[pos] {
                                    continue;
                                }
                                checked += 1;
                                if let Some(x) = val.as_f64() {
                                    let x = x as f32;
                                    if c.min < c.max && (x < c.min || x > c.max) {
                                        *out_of_range.entry(pos).or_default() += 1;
                                    }
                                }
                            }
                        }
                        Some(v) => rep.add(Severity::Error, "row.width", &sheet, format!("{} id {}", inst.file, r.id), "", format!("{} values for {} columns", v.len(), t.columns.len())),
                        None => rep.add(Severity::Error, "row.undecoded", &sheet, format!("{} id {}", inst.file, r.id), "", r.error.clone().unwrap_or_default()),
                    }
                }
            }
            for (pos, n) in out_of_range {
                let c = &t.columns[pos];
                rep.add(
                    Severity::Warning,
                    "cell.out_of_range",
                    &sheet,
                    format!("{n} rows"),
                    format!("#{} {}", c.index, c.name.as_deref().unwrap_or_default()),
                    format!("retail values outside def range [{}, {}]", c.min, c.max),
                );
            }
            let intersections = rows * t.columns.len();
            marks.push(Checkmark {
                kind: "param",
                group: g.group.clone(),
                param_type: t.param_type.clone(),
                instances: t.instances.len(),
                rows,
                columns: t.columns.len(),
                intersections,
                checked,
                unchecked: intersections - checked,
            });
        }
    }

    let mut exceptions = Exceptions::load(paths, &mut rep)?;
    archives(paths, &mut rep, &mut marks, &archive_index, &format_exts, &mut exceptions)?;
    textures(paths, &mut rep, &mut marks, &mut exceptions)?;
    models(paths, &mut rep, &mut marks, &mut exceptions)?;
    motions(paths, &mut rep, &mut marks, &mut exceptions)?;
    assembly(paths, &mut rep, &mut marks, &groups)?;
    tuning(paths, &mut rep, &mut marks)?;
    ac_states(paths, &mut rep, &mut marks, &groups)?;
    ac_ctrl(paths, &mut rep, &mut marks, &groups)?;
    ps3_citations(paths, &mut rep)?;
    exceptions.report_stale(&mut rep);
    write_outputs(paths, &rep, &marks)
}

/// Code and runtime evidence cites the Xbox 360 executable (`ACV2.pe`) and Xenia probes only:
/// PS3 executable addresses, RPCS3 runs and EBOOT/TOC references are errors in any sheet cell.
fn ps3_citations(paths: &Paths, rep: &mut Report) -> Result<()> {
    let mut files: Vec<_> = std::fs::read_dir(paths.sheets())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "csv"))
        .collect();
    files.sort();
    for path in files {
        let sheet = path.file_name().unwrap().to_string_lossy().into_owned();
        let mut rd = csv::ReaderBuilder::new().flexible(true).from_path(&path)?;
        let headers = rd.headers()?.clone();
        for row in rd.records() {
            let row = row?;
            for (i, cell) in row.iter().enumerate() {
                if let Some(hit) = ps3_citation(cell) {
                    let column = headers.get(i).unwrap_or("");
                    rep.add(Severity::Error, "evidence.ps3", &sheet, row.get(0).unwrap_or(""), column, format!("`{hit}`: cite the 360 address or a Xenia probe instead"));
                }
            }
        }
    }
    Ok(())
}

fn ps3_citation(cell: &str) -> Option<&str> {
    for word in ["RPCS3", "EBOOT"] {
        if let Some(i) = cell.find(word) {
            return Some(&cell[i..i + word.len()]);
        }
    }
    let bytes = cell.as_bytes();
    let boundary = |i: usize| i >= bytes.len() || !bytes[i].is_ascii_alphanumeric();
    for (i, _) in cell.match_indices("TOC") {
        if (i == 0 || boundary(i - 1)) && boundary(i + 3) {
            return Some(&cell[i..i + 3]);
        }
    }
    for (i, _) in cell.match_indices("01.02") {
        if (i == 0 || !bytes[i - 1].is_ascii_digit() && bytes[i - 1] != b'.') && (i + 5 >= bytes.len() || !bytes[i + 5].is_ascii_digit()) {
            return Some(&cell[i..i + 5]);
        }
    }
    for (i, _) in cell.match_indices("FUN_0") {
        let hex = cell[i + 4..].bytes().take_while(u8::is_ascii_hexdigit).count();
        if (6..=8).contains(&hex) && matches!(bytes.get(i + 5), Some(b'0' | b'1')) {
            return Some(&cell[i..i + 4 + hex]);
        }
    }
    None
}

/// `sheets/container_exceptions.csv`, tracking which rows matched a finding.
struct Exceptions {
    rows: Vec<ContainerException>,
    used: Vec<bool>,
}

impl Exceptions {
    fn load(paths: &Paths, rep: &mut Report) -> Result<Self> {
        let rows: Vec<ContainerException> = read_csv(&paths.sheets().join("container_exceptions.csv"))?;
        blank_cells(rep, "container_exceptions.csv", |r: &ContainerException| r.path.clone(), &rows);
        Ok(Self { used: vec![false; rows.len()], rows })
    }

    /// Evidence for `(path, check)` if the sheet explains it.
    fn excuse(&mut self, path: &str, check: &str) -> Option<String> {
        let i = self.rows.iter().position(|x| x.path == path && x.check == check)?;
        self.used[i] = true;
        Some(self.rows[i].evidence.clone())
    }

    fn report_stale(&self, rep: &mut Report) {
        for (x, used) in self.rows.iter().zip(&self.used) {
            if !used {
                rep.add(Severity::Error, "sheet.stale_row", "container_exceptions.csv", &x.path, &x.check, "exception does not match any finding");
            }
        }
    }
}

/// Columns of a texture row that each get a checkmark.
const TEXTURE_COLUMNS: usize = 8;

fn textures(paths: &Paths, rep: &mut Report, marks: &mut Vec<Checkmark>, exceptions: &mut Exceptions) -> Result<()> {
    let formats: Vec<TextureFormatRow> = read_csv(&paths.sheets().join("texture_formats.csv"))?;
    blank_cells(rep, "texture_formats.csv", |r: &TextureFormatRow| r.code.to_string(), &formats);
    let by_code: HashMap<u8, &TextureFormatRow> = formats.iter().map(|f| (f.code, f)).collect();
    let mut used = HashSet::new();
    for g in load_texture_groups(paths)? {
        let sheet = format!("textures/{}", g.group);
        let (mut rows, mut checked) = (0, 0);
        for p in &g.packs {
            if let Some(e) = &p.error {
                rep.add(Severity::Error, "tpf.unreadable", &sheet, &p.path, "", e);
            }
            let mut names = HashSet::new();
            for t in &p.textures {
                rows += 1;
                let row = format!("{} #{} {}", p.path, t.index, t.name);
                let mut ok = p.error.is_none();
                if !names.insert(t.name.as_str()) {
                    rep.add(Severity::Warning, "texture.duplicate_name", &sheet, &row, "name", "name repeats within the pack");
                }
                if t.truncated {
                    let detail = format!("{} bytes run past the end of the pack", t.size);
                    match exceptions.excuse(&format!("{}#{}", p.path, t.index), "texture.truncated") {
                        Some(why) => rep.add(Severity::Warning, "texture.truncated", &sheet, &row, "size", format!("{detail}: {why}")),
                        None => {
                            ok = false;
                            rep.add(Severity::Error, "texture.truncated", &sheet, &row, "size", detail);
                        }
                    }
                }
                match by_code.get(&t.format) {
                    None => {
                        ok = false;
                        rep.add(Severity::Error, "texture.format_unknown", &sheet, &row, "format", format!("format code {} has no row in texture_formats.csv", t.format));
                    }
                    Some(f) => {
                        used.insert(t.format);
                        let want = acvd_formats::tpf::stored_size(p.platform, t.width, t.height, t.levels, t.faces, f.block_bytes);
                        if want != t.size as u64 {
                            let detail = format!("{}x{} {} levels x{} faces as {} is {want} bytes, stored {}", t.width, t.height, t.levels, t.faces, f.name, t.size);
                            match exceptions.excuse(&format!("{}#{}", p.path, t.index), "texture.size") {
                                Some(why) => rep.add(Severity::Warning, "texture.size", &sheet, &row, "size", format!("{detail}: {why}")),
                                None => {
                                    ok = false;
                                    rep.add(Severity::Error, "texture.size", &sheet, &row, "size", detail);
                                }
                            }
                        }
                    }
                }
                if ok {
                    checked += TEXTURE_COLUMNS;
                }
            }
        }
        marks.push(Checkmark {
            kind: "texture",
            group: g.group.clone(),
            param_type: "TPF textures".into(),
            instances: g.packs.len(),
            rows,
            columns: TEXTURE_COLUMNS,
            intersections: rows * TEXTURE_COLUMNS,
            checked,
            unchecked: rows * TEXTURE_COLUMNS - checked,
        });
    }
    for f in &formats {
        if !used.contains(&f.code) {
            rep.add(Severity::Warning, "texture_formats.not_on_disc", "texture_formats.csv", f.code.to_string(), "", "no texture on this disc uses this code (the PS3 and 360 discs use different sets)");
        }
    }
    Ok(())
}

/// Columns of an AC part record that each get a checkmark: id, model id.
const AC_PART_COLUMNS: usize = 2;

/// The `parent` and `socket` cells of the anchor slot.
pub const NONE: &str = "none";

/// A socket cell: a dummy id on the parent's model, or `bone:<name>`.
pub enum SocketSpec<'a> {
    None,
    Dummy(u8),
    Bone(&'a str),
}

pub fn socket_spec(s: &str) -> Option<SocketSpec<'_>> {
    let s = s.trim();
    if s == NONE {
        Some(SocketSpec::None)
    } else if let Some(b) = s.strip_prefix("bone:") {
        Some(SocketSpec::Bone(b))
    } else {
        s.parse().ok().map(SocketSpec::Dummy)
    }
}

/// Where `spec` sits on `m`: its position and the root bone that carries it. A repeated dummy
/// id resolves to its first dummy (cr03xx/cr05xx repeat every socket on a displaced copy).
pub fn find_socket<'a>(m: &'a ModelSheet, spec: &SocketSpec) -> Option<([f32; 3], &'a str)> {
    match *spec {
        SocketSpec::None => None,
        SocketSpec::Dummy(id) => {
            let s = m.sockets.iter().find(|s| s.color[0] == id)?;
            Some((s.position, m.root_of(usize::try_from(s.bone).ok()?)?))
        }
        SocketSpec::Bone(name) => {
            let i = m.skeleton.iter().position(|b| b.name == name)?;
            Some((m.skeleton[i].origin, m.root_of(i)?))
        }
    }
}

fn assembly(paths: &Paths, rep: &mut Report, marks: &mut Vec<Checkmark>, groups: &[GroupSheet]) -> Result<()> {
    let categories: Vec<AcPartCategoryRow> = read_csv(&paths.sheets().join("ac_part_categories.csv"))?;
    let slots: Vec<AssemblySlotRow> = read_csv(&paths.sheets().join("assembly_slots.csv"))?;
    blank_cells(rep, "ac_part_categories.csv", |r: &AcPartCategoryRow| r.category.to_string(), &categories);
    let parts: AcPartsSheet = read_json(&paths.ac_parts())?;
    let models: HashMap<String, ModelSheet> =
        load_model_groups(paths)?.into_iter().flat_map(|g| g.models).filter(|m| m.error.is_none()).map(|m| (m.path.clone(), m)).collect();
    let category = |c: usize| categories.iter().find(|r| r.category == c);

    // ---- acvparts.bin records ----
    if let Some(e) = &parts.error {
        rep.add(Severity::Error, "ac_parts.unreadable", "ac_part_categories.csv", &parts.path, "record_size", e);
    }
    if let Some(at) = parts.tail {
        let detail = format!("{} bytes past the counted records ({:#x}) hold unindexed data from {at:#x}", parts.size - parts.end, parts.end);
        rep.add(Severity::Pending, "ac_parts.tail", "ac_part_categories.csv", &parts.path, "", detail);
    }
    for c in &categories {
        let records: Vec<&AcPartRow> = parts.records.iter().filter(|r| r.category == c.category).collect();
        if parts.error.is_none() && records.is_empty() {
            rep.add(Severity::Error, "sheet.stale_row", "ac_part_categories.csv", c.category.to_string(), "", "category holds no records");
        }
        let (mut checked, mut missing, mut ids) = (0, Vec::new(), HashSet::new());
        for r in &records {
            let row = format!("{} #{} id {}", c.name, r.index, r.id);
            let mut ok = true;
            if r.id == 0 || !ids.insert(r.id) {
                ok = false;
                rep.add(Severity::Error, "ac_part.id", "ac_parts", &row, "id", "id is zero or repeats within its category");
            }
            if c.prefixes().any(|p| models.contains_key(&c.model_path(p, r.model_id))) {
                checked += 1;
            } else {
                missing.push(r.model_id.to_string());
            }
            if ok {
                checked += 1;
            }
        }
        if !missing.is_empty() {
            let detail = format!("{} model ids name no {} model: {}", missing.len(), c.prefixes, missing.join(", "));
            rep.add(Severity::Warning, "ac_part.model_missing", "ac_parts", &c.name, "model_id", detail);
        }
        marks.push(Checkmark {
            kind: "ac_part",
            group: c.name.clone(),
            param_type: "acvparts.bin records".into(),
            instances: 1,
            rows: records.len(),
            columns: AC_PART_COLUMNS,
            intersections: records.len() * AC_PART_COLUMNS,
            checked,
            unchecked: records.len() * AC_PART_COLUMNS - checked,
        });
    }

    // ---- slot sheet ----
    blank_cells(rep, "assembly_slots.csv", |r: &AssemblySlotRow| r.slot.clone(), &slots);
    let mut slot_ok = vec![true; slots.len()];
    for (i, s) in slots.iter().enumerate() {
        let mut bad = |column: &str, detail: String| {
            slot_ok[i] = false;
            rep.add(Severity::Error, "slot.invalid", "assembly_slots.csv", &s.slot, column, detail);
        };
        match category(s.category) {
            None => bad("category", format!("category {} has no row in ac_part_categories.csv", s.category)),
            Some(c) if !c.prefixes().any(|p| p == s.prefix) => bad("prefix", format!("`{}` is not a prefix of {}", s.prefix, c.name)),
            _ => {}
        }
        match socket_spec(&s.socket) {
            None => bad("socket", format!("`{}` is neither a dummy id nor bone:<name>", s.socket)),
            Some(SocketSpec::None) if s.parent != NONE => bad("socket", "a slot with a parent needs a socket".into()),
            Some(SocketSpec::Dummy(_) | SocketSpec::Bone(_)) if s.parent == NONE => bad("parent", "a socket needs a parent slot".into()),
            _ => {}
        }
        if s.parent != NONE && !slots[..i].iter().any(|p| p.slot == s.parent) {
            bad("parent", format!("`{}` is not an earlier slot", s.parent));
        }
        if slots[..i].iter().any(|p| p.slot == s.slot) {
            bad("slot", "slot listed twice".into());
        }
    }

    // ---- every assembly row x slot ----
    let tables: Vec<&TableSheet> = groups.iter().flat_map(|g| &g.tables).filter(|t| slots.iter().any(|s| s.table == t.param_type)).collect();
    for s in &slots {
        if !tables.iter().any(|t| t.param_type == s.table && t.columns.iter().any(|c| c.ident.as_deref() == Some(s.column.as_str()))) {
            rep.add(Severity::Error, "slot.invalid", "assembly_slots.csv", &s.slot, "column", format!("{} has no column `{}`", s.table, s.column));
        }
    }
    let mut problems: BTreeMap<(&'static str, String), Vec<String>> = BTreeMap::new();
    let mut duplicates: BTreeMap<&str, std::collections::BTreeSet<&str>> = BTreeMap::new();
    for t in tables {
        let slots: Vec<(usize, &AssemblySlotRow)> = slots.iter().enumerate().filter(|(_, s)| s.table == t.param_type).collect();
        let col = |name: &str| t.columns.iter().position(|c| c.ident.as_deref() == Some(name));
        let (mut rows, mut checked) = (0, 0);
        for inst in &t.instances {
            for r in &inst.rows {
                rows += 1;
                let Some(values) = &r.values else { continue };
                let mut placed: HashMap<&str, &ModelSheet> = HashMap::new();
                for &(si, s) in &slots {
                    let id = col(&s.column).and_then(|c| values.get(c)).and_then(Value::as_i64).unwrap_or(0);
                    if id <= 0 {
                        checked += usize::from(slot_ok[si]);
                        continue;
                    }
                    let mut fail = |check: &'static str| problems.entry((check, s.slot.clone())).or_default().push(format!("{}:{}", r.id, id));
                    let Some(c) = category(s.category) else { continue };
                    let Some(part) = parts.records.iter().find(|p| p.category == s.category && i64::from(p.id) == id) else {
                        fail("assembly.part_unknown");
                        continue;
                    };
                    let Some(model) = models.get(&c.model_path(&s.prefix, part.model_id)) else {
                        fail("assembly.model_missing");
                        continue;
                    };
                    let roots = s.roots(model);
                    if roots.is_empty() || !roots.iter().all(|r| model.skeleton.iter().any(|b| b.parent < 0 && b.name == *r)) {
                        fail("assembly.root_missing");
                        continue;
                    }
                    placed.insert(&s.slot, model);
                    if s.parent != NONE {
                        let Some(parent) = placed.get(s.parent.as_str()) else {
                            fail("assembly.parent_empty");
                            continue;
                        };
                        let spec = socket_spec(&s.socket).unwrap_or(SocketSpec::None);
                        let Some((_, carrier)) = find_socket(parent, &spec) else {
                            fail("assembly.socket_missing");
                            continue;
                        };
                        let parent_roots = slots.iter().find(|(_, p)| p.slot == s.parent).map(|(_, p)| p.roots(parent)).unwrap_or_default();
                        if !parent_roots.contains(&carrier) {
                            fail("assembly.socket_root");
                            continue;
                        }
                        if let SocketSpec::Dummy(id) = spec {
                            if parent.sockets.iter().filter(|x| x.color[0] == id).count() > 1 {
                                duplicates.entry(s.slot.as_str()).or_default().insert(parent.path.rsplit('|').next().unwrap_or(&parent.path));
                            }
                        }
                    }
                    checked += usize::from(slot_ok[si]);
                }
            }
        }
        marks.push(Checkmark {
            kind: "assembly",
            group: "assembly".into(),
            param_type: t.param_type.clone(),
            instances: t.instances.len(),
            rows,
            columns: slots.len(),
            intersections: rows * slots.len(),
            checked,
            unchecked: rows * slots.len() - checked,
        });
    }
    for ((check, slot), cases) in problems {
        let sample: Vec<&str> = cases.iter().take(10).map(String::as_str).collect();
        let detail = format!("{} rows (row:part) e.g. {}", cases.len(), sample.join(", "));
        rep.add(Severity::Warning, check, "assembly_slots.csv", slot, "", detail);
    }
    for (slot, models) in duplicates {
        let list: Vec<&str> = models.into_iter().collect();
        rep.add(Severity::Warning, "assembly.socket_duplicate", "assembly_slots.csv", slot, "socket", format!("first of repeated dummies used on {}", list.join(", ")));
    }
    Ok(())
}

/// Columns of a face set row that each get a checkmark.
const FACE_SET_COLUMNS: usize = 10;

fn file_stem(path: &str) -> String {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    name.split('.').next().unwrap_or(name).to_ascii_lowercase()
}

fn models(paths: &Paths, rep: &mut Report, marks: &mut Vec<Checkmark>, exceptions: &mut Exceptions) -> Result<()> {
    let types: Vec<VertexTypeRow> = read_csv(&paths.sheets().join("vertex_types.csv"))?;
    blank_cells(rep, "vertex_types.csv", |r: &VertexTypeRow| format!("{}/{}", r.kind, r.semantic), &types);
    let known: HashMap<String, &VertexTypeRow> = types.iter().map(|t| (format!("{}/{}", t.kind, t.semantic), t)).collect();
    let mut used_types = HashSet::new();
    let texture_names: HashSet<String> =
        load_texture_groups(paths)?.iter().flat_map(|g| &g.packs).flat_map(|p| &p.textures).map(|t| t.name.to_ascii_lowercase()).collect();

    for g in load_model_groups(paths)? {
        let sheet = format!("models/{}", g.group);
        let (mut rows, mut checked) = (0, 0);
        let (mut unreferenced_meshes, mut unreferenced) = (0, 0);
        let mut missing_textures: BTreeMap<String, usize> = BTreeMap::new();
        let mut outside_models: Vec<String> = Vec::new();
        for m in &g.models {
            if let Some(e) = &m.error {
                match exceptions.excuse(&m.path, "model.unreadable") {
                    Some(why) => rep.add(Severity::Pending, "model.unreadable", &sheet, &m.path, "", format!("version {:#x}: {why}", m.version)),
                    None => rep.add(Severity::Error, "model.unreadable", &sheet, &m.path, "", format!("version {:#x}: {e}", m.version)),
                }
                continue;
            }
            for mat in &m.materials {
                for (_, path) in mat.textures.iter().filter(|(_, p)| !p.is_empty()) {
                    if !texture_names.contains(&file_stem(path)) {
                        *missing_textures.entry(file_stem(path)).or_default() += 1;
                    }
                }
            }
            let extent = (0..3).map(|k| m.bbox_max[k] - m.bbox_min[k]).fold(0f32, f32::max);
            let slack = 0.01 * extent + 0.01;
            let outside = m.meshes.iter().filter_map(|x| x.bounds).any(|(lo, hi)| (0..3).any(|k| lo[k] < m.bbox_min[k] - slack || hi[k] > m.bbox_max[k] + slack));
            if outside {
                outside_models.push(m.path.rsplit('|').next().unwrap_or(&m.path).to_owned());
            }
            for mesh in &m.meshes {
                let mesh_row = format!("{} mesh {}", m.path, mesh.index);
                let mut mesh_ok = true;
                if let Some(e) = &mesh.vertex_error {
                    mesh_ok = false;
                    rep.add(Severity::Error, "mesh.vertices", &sheet, &mesh_row, "", e);
                }
                if mesh.unboned > 0 {
                    mesh_ok = false;
                    rep.add(Severity::Error, "mesh.vertex_bone", &sheet, &mesh_row, "", format!("{} vertices name a bone outside the tables", mesh.unboned));
                }
                for member in &mesh.members {
                    used_types.insert(member.clone());
                    if !known.contains_key(member) {
                        mesh_ok = false;
                        rep.add(Severity::Error, "vertex.type_unknown", &sheet, &mesh_row, "members", format!("{member} has no row in vertex_types.csv"));
                    }
                }
                if mesh.counter_clockwise > 0 {
                    let detail = format!("{} triangles wind counter-clockwise around their normals, {} clockwise", mesh.counter_clockwise, mesh.clockwise);
                    rep.add(Severity::Warning, "mesh.winding", &sheet, &mesh_row, "", detail);
                }
                if mesh.unreferenced > 0 {
                    unreferenced_meshes += 1;
                    unreferenced += mesh.unreferenced;
                }
                for fs in &mesh.face_sets {
                    rows += 1;
                    let row = format!("{mesh_row} face set {} flags {:#x}", fs.index, fs.flags);
                    let mut ok = mesh_ok;
                    if let Some(e) = &fs.error {
                        ok = false;
                        rep.add(Severity::Error, "faceset.undecodable", &sheet, &row, "", e);
                    }
                    if fs.max_index.is_some_and(|i| i as usize >= mesh.vertices) {
                        ok = false;
                        rep.add(Severity::Error, "faceset.index_range", &sheet, &row, "max_index", format!("index {:?} with {} vertices", fs.max_index, mesh.vertices));
                    }
                    if fs.degenerate > 0 {
                        rep.add(Severity::Warning, "faceset.degenerate", &sheet, &row, "", format!("{} of {} triangles repeat a vertex", fs.degenerate, fs.triangles));
                    }
                    if ok {
                        checked += FACE_SET_COLUMNS;
                    }
                }
            }
        }
        if !outside_models.is_empty() {
            let detail = format!("model-space vertices leave the header bounding box: {}", outside_models.iter().take(12).cloned().collect::<Vec<_>>().join(", "));
            rep.add(Severity::Warning, "model.bounds_outside", &sheet, format!("{} of {} models", outside_models.len(), g.models.len()), "", detail);
        }
        if unreferenced > 0 {
            rep.add(Severity::Warning, "mesh.unreferenced_vertices", &sheet, format!("{unreferenced_meshes} meshes"), "", format!("{unreferenced} vertices no face set uses"));
        }
        if !missing_textures.is_empty() {
            let sample: Vec<&str> = missing_textures.keys().take(12).map(String::as_str).collect();
            rep.add(
                Severity::Warning,
                "material.texture_missing",
                &sheet,
                format!("{} names", missing_textures.len()),
                "textures",
                format!("{} material references name no TPF texture: {}", missing_textures.values().sum::<usize>(), sample.join(", ")),
            );
        }
        marks.push(Checkmark {
            kind: "model",
            group: g.group.clone(),
            param_type: "FLVER face sets".into(),
            instances: g.models.len(),
            rows,
            columns: FACE_SET_COLUMNS,
            intersections: rows * FACE_SET_COLUMNS,
            checked,
            unchecked: rows * FACE_SET_COLUMNS - checked,
        });
    }
    for t in &types {
        if !used_types.contains(&format!("{}/{}", t.kind, t.semantic)) {
            rep.add(Severity::Warning, "vertex_types.not_on_disc", "vertex_types.csv", format!("{}/{}", t.kind, t.semantic), "", "no mesh on this disc uses this member (the PS3 and 360 discs use different sets)");
        }
    }
    Ok(())
}

/// Columns of a clip row that each get a checkmark: readable, frames, bones, track kinds, skeleton, keys.
const CLIP_COLUMNS: usize = 6;

/// Motion findings are excused per binder (`container_exceptions.csv` path = the set's path).
fn motions(paths: &Paths, rep: &mut Report, marks: &mut Vec<Checkmark>, exceptions: &mut Exceptions) -> Result<()> {
    use acvd_formats::ani::LAYOUTS;
    let kinds: Vec<AniTrackKindRow> = read_csv(&paths.sheets().join("ani_track_kinds.csv"))?;
    blank_cells(rep, "ani_track_kinds.csv", |r: &AniTrackKindRow| r.kind.to_string(), &kinds);
    for l in LAYOUTS {
        match kinds.iter().find(|k| k.kind == l.kind) {
            None => rep.add(Severity::Error, "ani.kind_missing", "ani_track_kinds.csv", l.kind.to_string(), "", "acvd-formats::ani reads this kind; add its row"),
            Some(k) if (k.width, k.translation, k.rotation_tangents, k.scale, k.row_size) != (l.width, l.translation, l.rotation_tangents, l.scale, l.row_size()) => {
                rep.add(Severity::Error, "ani.layout_mismatch", "ani_track_kinds.csv", l.kind.to_string(), "", format!("sheet row disagrees with acvd-formats::ani {l:?}"))
            }
            Some(_) => {}
        }
    }
    let mut used = vec![0usize; LAYOUTS.len()];
    for g in load_motion_groups(paths)? {
        let sheet = format!("motions/{}", g.group);
        let (mut rows, mut checked, mut unskinned) = (0, 0, Vec::new());
        for s in &g.sets {
            // A set may host several actors (cutscenes, cameras): named clips each carry their own
            // skeleton, and an unnamed clip indexes the named skeleton of its bone count.
            let mut sizes: Vec<usize> = s.clips.iter().filter(|c| !c.skeleton.is_empty()).map(|c| c.skeleton.len()).collect();
            sizes.sort_unstable();
            sizes.dedup();
            for size in &sizes {
                let mut shapes: Vec<Vec<(&str, Option<u16>)>> =
                    s.clips.iter().filter(|c| c.skeleton.len() == *size).map(|c| c.skeleton.iter().map(|b| (b.name.as_str(), b.parent)).collect()).collect();
                shapes.sort();
                shapes.dedup();
                if shapes.len() > 1 && s.clips.iter().any(|c| !c.named && c.error.is_none() && c.bones == *size) {
                    let detail = format!("{} distinct {size}-bone named skeletons for unnamed clips to index", shapes.len());
                    let severity = if exceptions.excuse(&s.path, "motion.skeleton_mismatch").is_some() { Severity::Pending } else { Severity::Error };
                    rep.add(severity, "motion.skeleton_mismatch", &sheet, &s.path, "skeleton", detail);
                }
            }
            if sizes.is_empty() {
                unskinned.push(s.path.rsplit('/').next().unwrap_or(&s.path).to_owned());
            }
            for c in &s.clips {
                rows += 1;
                let row = format!("{}|{}", s.path, c.name);
                if let Some(e) = &c.error {
                    let severity = if exceptions.excuse(&s.path, "motion.unreadable").is_some() { Severity::Pending } else { Severity::Error };
                    rep.add(severity, "motion.unreadable", &sheet, &row, "", e);
                    continue;
                }
                used.iter_mut().zip(&c.kinds).for_each(|(u, n)| *u += n);
                let mut ok = CLIP_COLUMNS;
                if !c.named && !sizes.is_empty() && !sizes.contains(&c.bones) {
                    ok -= 1;
                    let severity = if exceptions.excuse(&s.path, "motion.bone_count").is_some() { Severity::Pending } else { Severity::Error };
                    rep.add(severity, "motion.bone_count", &sheet, &row, "bones", format!("{} bones; the set's named skeletons have {sizes:?}", c.bones));
                }
                if !c.named && sizes.is_empty() {
                    ok -= 1;
                }
                checked += ok;
            }
        }
        if !unskinned.is_empty() {
            let detail = format!("no named clip carries the skeleton these clips index: {}", unskinned.iter().take(8).cloned().collect::<Vec<_>>().join(", "));
            rep.add(Severity::Pending, "motion.no_skeleton", &sheet, format!("{} of {} sets", unskinned.len(), g.sets.len()), "skeleton", detail);
        }
        marks.push(Checkmark {
            kind: "motion",
            group: g.group.clone(),
            param_type: ".ani clips".into(),
            instances: g.sets.len(),
            rows,
            columns: CLIP_COLUMNS,
            intersections: rows * CLIP_COLUMNS,
            checked,
            unchecked: rows * CLIP_COLUMNS - checked,
        });
    }
    for (l, n) in LAYOUTS.iter().zip(&used) {
        if *n == 0 {
            rep.add(Severity::Error, "sheet.stale_row", "ani_track_kinds.csv", l.kind.to_string(), "", "no clip on the disc uses this track kind");
        }
    }
    for k in kinds.iter().filter(|k| !LAYOUTS.iter().any(|l| l.kind == k.kind)) {
        rep.add(Severity::Error, "ani.kind_unread", "ani_track_kinds.csv", k.kind.to_string(), "", "acvd-formats::ani has no layout for this kind");
    }

    // ---- which motion set each part selects (ac_motion.csv, ac_part_fields.csv) ----
    let fields: Vec<AcPartFieldRow> = read_csv(&paths.sheets().join("ac_part_fields.csv"))?;
    blank_cells(rep, "ac_part_fields.csv", |r: &AcPartFieldRow| r.name.clone(), &fields);
    let categories: Vec<AcPartCategoryRow> = read_csv(&paths.sheets().join("ac_part_categories.csv"))?;
    for f in &fields {
        match (f.size(), categories.iter().find(|c| c.category == f.category)) {
            (None, _) => rep.add(Severity::Error, "ac_field.invalid", "ac_part_fields.csv", &f.name, "type", format!("`{}` is not u8/i8/u16/i16/u32/i32/f32", f.ty)),
            (_, None) => rep.add(Severity::Error, "ac_field.invalid", "ac_part_fields.csv", &f.name, "category", format!("category {} has no row", f.category)),
            (Some(n), Some(c)) if f.offset + n > c.record_size => {
                rep.add(Severity::Error, "ac_field.invalid", "ac_part_fields.csv", &f.name, "offset", format!("ends past the {}-byte record", c.record_size))
            }
            _ => {}
        }
    }
    let rows: Vec<AcMotionRow> = read_csv(&paths.sheets().join("ac_motion.csv"))?;
    blank_cells(rep, "ac_motion.csv", |r: &AcMotionRow| r.slot.clone(), &rows);
    let selected = crate::motions::part_motions(paths)?;
    let mut problems: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for p in &selected {
        if let Err((check, detail)) = &p.set {
            problems.entry(*check).or_default().push(format!("{}: {detail}", p.id));
        }
    }
    for (check, list) in problems {
        let detail = list.iter().take(8).cloned().collect::<Vec<_>>().join("; ");
        rep.add(Severity::Error, check, "ac_motion.csv", format!("{} parts", list.len()), "", detail);
    }
    let checked = selected.iter().filter(|p| p.set.is_ok()).count();
    marks.push(Checkmark {
        kind: "motion",
        group: "ac_motion".into(),
        param_type: "part -> motion set".into(),
        instances: rows.len(),
        rows: selected.len(),
        columns: 1,
        intersections: selected.len(),
        checked,
        unchecked: selected.len() - checked,
    });
    Ok(())
}

/// Columns of a tuning field that each get a checkmark: value and label.
const TUNING_COLUMNS: usize = 2;

pub fn is_ident(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_lowercase()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Up to eight items of `list`, then how many more.
fn some(list: &[String]) -> String {
    let more = list.len().saturating_sub(8);
    let head = list.iter().take(8).cloned().collect::<Vec<_>>().join(", ");
    if more > 0 { format!("{head} (+{more})") } else { head }
}

fn tuning(paths: &Paths, rep: &mut Report, marks: &mut Vec<Checkmark>) -> Result<()> {
    let sheet: TuningSheet = read_json(&paths.tuning())?;
    let roots: Vec<TuningRootRow> = read_csv(&paths.sheets().join("tuning_roots.csv"))?;
    blank_cells(rep, "tuning_roots.csv", |r: &TuningRootRow| r.variable.clone(), &roots);
    if let Some(e) = &sheet.error {
        rep.add(Severity::Error, "tuning.list", &sheet.source, "", "", e);
    }
    for r in &roots {
        if !sheet.files.iter().any(|f| f.roots.contains(&r.variable)) {
            rep.add(Severity::Error, "sheet.stale_row", "tuning_roots.csv", &r.variable, "", "no paramlist path uses this variable");
        }
    }
    for f in &sheet.files {
        let name = format!("tuning/{}", f.name);
        if let Some(e) = &f.error {
            rep.add(Severity::Error, "tuning.unreadable", &name, &f.bin, "", e);
        }
        if f.tail > 0 {
            let what = if f.tail_nonzero { "data the menu layout does not describe" } else { "zero padding" };
            rep.add(Severity::Warning, "tuning.bin_tail", &name, &f.bin, "", format!("{} bytes past the layout: {what}", f.tail));
        }
        if f.extra_strings > 0 {
            rep.add(Severity::Warning, "tuning.extra_strings", &name, &f.dbp, "label", format!("{} strings past the last (label, format) pair", f.extra_strings));
        }
        let (mut kind, mut range, mut menu, mut unlabeled) = (Vec::new(), Vec::new(), Vec::new(), 0);
        for (i, v) in f.fields.iter().enumerate() {
            let float_format = v.format.contains(['f', 'g', 'e']);
            if v.format.contains('%') && float_format != (v.kind == "f32") {
                kind.push(format!("#{i} {} `{}`", v.kind, v.format));
            }
            if v.min < v.max && (v.value < v.min || v.value > v.max) {
                range.push(format!("#{i} {} not in [{}, {}]", v.value, v.min, v.max));
            }
            if (v.value - v.menu_value).abs() > 1e-6 * v.value.abs().max(1.0) {
                menu.push(format!("#{i} {} (menu {})", v.value, v.menu_value));
            }
            unlabeled += usize::from(v.label.trim().is_empty());
        }
        for (check, list, detail) in [
            ("tuning.format_kind", &kind, "printf format disagrees with the field type"),
            ("tuning.out_of_range", &range, "shipped value outside the menu range"),
            ("tuning.menu_differs", &menu, "shipped value differs from the menu snapshot"),
        ] {
            if !list.is_empty() {
                rep.add(Severity::Warning, check, &name, format!("{} fields", list.len()), "", format!("{detail}: {}", some(list)));
            }
        }
        let rows = f.fields.len();
        let checked = if f.error.is_none() { 2 * rows - unlabeled } else { 0 };
        marks.push(Checkmark {
            kind: "tuning",
            group: "tuning".into(),
            param_type: f.name.clone(),
            instances: 1,
            rows,
            columns: TUNING_COLUMNS,
            intersections: rows * TUNING_COLUMNS,
            checked,
            unchecked: rows * TUNING_COLUMNS - checked,
        });
    }

    let named: Vec<TuningFieldRow> = read_csv(&paths.sheets().join("tuning_fields.csv"))?;
    blank_cells(rep, "tuning_fields.csv", |r: &TuningFieldRow| format!("{} #{}", r.file, r.index), &named);
    let mut idents: HashSet<(&str, &str)> = HashSet::new();
    for n in &named {
        let key = format!("{} #{}", n.file, n.index);
        match sheet.files.iter().find(|f| f.bin == n.file).map(|f| f.fields.get(n.index)) {
            None => rep.add(Severity::Error, "sheet.stale_row", "tuning_fields.csv", &key, "file", "no tuning file at this path"),
            Some(None) => rep.add(Severity::Error, "sheet.stale_row", "tuning_fields.csv", &key, "index", "past the file's last field"),
            Some(Some(v)) if v.label != n.label => {
                rep.add(Severity::Error, "tuning.label_mismatch", "tuning_fields.csv", &key, "label", format!("disc label is `{}`", v.label))
            }
            Some(Some(_)) => {}
        }
        if !is_ident(&n.ident) || !idents.insert((&n.file, &n.ident)) {
            rep.add(Severity::Error, "tuning.ident", "tuning_fields.csv", &key, "ident", format!("`{}` is not a unique snake_case identifier", n.ident));
        }
        if !is_done(&n.status) {
            rep.add(Severity::Pending, "tuning.unit", "tuning_fields.csv", &key, "status", format!("{} ({}): {}", n.ident, n.unit, n.status));
        }
    }
    Ok(())
}

pub const AC_MOTION_FILE: &str = "param/acmotion.bin";

/// `ac_states.csv`: every state names an existing `param/acmotion.bin` row by its disc name.
fn ac_states(paths: &Paths, rep: &mut Report, marks: &mut Vec<Checkmark>, groups: &[GroupSheet]) -> Result<()> {
    let states: Vec<AcStateRow> = read_csv(&paths.sheets().join("ac_states.csv"))?;
    blank_cells(rep, "ac_states.csv", |r: &AcStateRow| r.state.clone(), &states);
    let rows: Vec<&Row> = groups.iter().flat_map(|g| &g.tables).flat_map(|t| &t.instances).filter(|i| i.file == AC_MOTION_FILE).flat_map(|i| &i.rows).collect();
    let mut seen = HashSet::new();
    let mut checked = 0;
    for s in &states {
        let key = format!("{} {}", s.state, s.direction.map_or(String::new(), |d| d.to_string()));
        let mut ok = true;
        if !is_ident(&s.state) {
            ok = false;
            rep.add(Severity::Error, "ac_state.ident", "ac_states.csv", &key, "state", "not a snake_case identifier");
        }
        if s.direction.is_some_and(|d| d > 7) {
            ok = false;
            rep.add(Severity::Error, "ac_state.direction", "ac_states.csv", &key, "direction", "directions are 0..=7");
        }
        if !seen.insert((s.state.as_str(), s.direction)) {
            ok = false;
            rep.add(Severity::Error, "ac_state.duplicate", "ac_states.csv", &key, "", "state and direction listed twice");
        }
        match rows.iter().find(|r| r.id == s.row) {
            None => {
                ok = false;
                rep.add(Severity::Error, "ac_state.no_row", "ac_states.csv", &key, "row", format!("{AC_MOTION_FILE} has no row {}", s.row));
            }
            Some(r) if r.name != s.name => {
                ok = false;
                rep.add(Severity::Error, "ac_state.name_mismatch", "ac_states.csv", &key, "name", format!("row {} is named `{}` on the disc", s.row, r.name));
            }
            Some(_) => {}
        }
        checked += usize::from(ok);
    }
    marks.push(Checkmark {
        kind: "motion",
        group: "ac_states".into(),
        param_type: "state -> acmotion row".into(),
        instances: 1,
        rows: states.len(),
        columns: 1,
        intersections: states.len(),
        checked,
        unchecked: states.len() - checked,
    });
    Ok(())
}

/// Columns of an `ac_ctrl_calc.csv` row that each get a checkmark: name, param, unit, expr.
const CALC_COLUMNS: usize = 4;

/// `design_parts.csv` (every design's part in each column is in the column's category) and
/// `ac_ctrl_calc.csv` (every expression names only what the sheets define, in order).
fn ac_ctrl(paths: &Paths, rep: &mut Report, marks: &mut Vec<Checkmark>, groups: &[GroupSheet]) -> Result<()> {
    let mut scope = crate::calc::Scope::load(paths)?;
    let parts: AcPartsSheet = read_json(&paths.ac_parts())?;
    blank_cells(rep, "design_parts.csv", |r: &DesignPartRow| r.column.clone(), scope.columns());
    let unbuildable: Vec<UnbuildableDesignRow> = read_csv(&paths.sheets().join("unbuildable_designs.csv"))?;
    blank_cells(rep, "unbuildable_designs.csv", |r: &UnbuildableDesignRow| r.id.to_string(), &unbuildable);
    let mut explained = HashSet::new();
    let (mut rows, mut checked, mut seen) = (0, 0, HashSet::new());
    for c in scope.columns() {
        let Some(t) = groups.iter().flat_map(|g| &g.tables).find(|t| t.param_type == c.table) else {
            rep.add(Severity::Error, "design_part.invalid", "design_parts.csv", &c.column, "table", format!("no table {}", c.table));
            continue;
        };
        let Some(col) = t.columns.iter().position(|x| x.ident.as_deref() == Some(c.column.as_str())) else {
            rep.add(Severity::Error, "design_part.invalid", "design_parts.csv", &c.column, "column", format!("{} has no column `{}`", c.table, c.column));
            continue;
        };
        if col != c.slot || !seen.insert(c.slot) {
            rep.add(Severity::Error, "design_part.invalid", "design_parts.csv", &c.column, "slot", format!("column `{}` is {} column #{col}", c.column, c.table));
        }
        let mut unknown = Vec::new();
        let mut found = 0;
        for r in t.instances.iter().flat_map(|i| &i.rows) {
            rows += 1;
            let id = r.values.as_ref().and_then(|v| v.get(col)).and_then(Value::as_i64).unwrap_or(0);
            if id <= 0 {
                checked += 1;
            } else if parts.records.iter().any(|p| p.category == c.category && i64::from(p.id) == id) {
                checked += 1;
                found += 1;
            } else if let Some(u) = unbuildable.iter().find(|u| u.id == r.id) {
                if u.name == r.name {
                    checked += 1;
                    explained.insert(u.id);
                } else {
                    rep.add(Severity::Error, "design_part.unbuildable", "unbuildable_designs.csv", &u.id.to_string(), "name", format!("row {} is named {:?}", r.id, r.name));
                }
            } else {
                unknown.push(format!("{}:{id}", r.id));
            }
        }
        if found == 0 {
            rep.add(Severity::Error, "design_part.category", "design_parts.csv", &c.column, "category", format!("no id is in category {}", c.category));
        } else if !unknown.is_empty() {
            rep.add(Severity::Warning, "design_part.unknown_id", "design_parts.csv", &c.column, "category", format!("ids not in category {}: {}", c.category, some(&unknown)));
        }
    }
    for u in unbuildable.iter().filter(|u| !explained.contains(&u.id)) {
        rep.add(Severity::Error, "design_part.unbuildable", "unbuildable_designs.csv", &u.id.to_string(), "id", "every part of this design is catalogued");
    }
    marks.push(Checkmark {
        kind: "calc",
        group: "design_parts".into(),
        param_type: "design row x part column".into(),
        instances: 1,
        rows,
        columns: 1,
        intersections: rows,
        checked,
        unchecked: rows - checked,
    });

    let calc: Vec<AcCtrlRow> = read_csv(&paths.sheets().join("ac_ctrl_calc.csv"))?;
    blank_cells(rep, "ac_ctrl_calc.csv", |r: &AcCtrlRow| r.name.clone(), &calc);
    let (mut checked, mut params) = (0, HashSet::new());
    for r in &calc {
        let mut ok = CALC_COLUMNS;
        let mut bad = |column: &str, detail: String| {
            ok -= 1;
            rep.add(Severity::Error, "calc.invalid", "ac_ctrl_calc.csv", &r.name, column, detail);
        };
        if !is_ident(&r.name) || scope.defined.contains(&r.name) || scope.columns().iter().any(|c| c.column == r.name) {
            bad("name", "not a unique snake_case identifier distinct from the design columns".into());
        }
        if r.param != NONE_PARAM && !r.param.parse::<u32>().is_ok_and(|p| params.insert(p)) {
            bad("param", format!("`{}` is neither `{NONE_PARAM}` nor an unused Scr_SetParam id", r.param));
        }
        if let Err(e) = scope.translate(&r.expr) {
            bad("expr", e);
        }
        checked += ok;
        scope.defined.push(r.name.clone());
    }
    marks.push(Checkmark {
        kind: "calc",
        group: "ac_ctrl_calc".into(),
        param_type: "AC control parameters".into(),
        instances: 1,
        rows: calc.len(),
        columns: CALC_COLUMNS,
        intersections: calc.len() * CALC_COLUMNS,
        checked,
        unchecked: calc.len() * CALC_COLUMNS - checked,
    });
    Ok(())
}

/// `ac_ctrl_calc.csv` `param` cell of a value the game does not store.
pub const NONE_PARAM: &str = "-";

/// Columns of an archive entry row that each get a checkmark.
const ENTRY_COLUMNS: usize = 8;

fn archives(
    paths: &Paths,
    rep: &mut Report,
    marks: &mut Vec<Checkmark>,
    index: &ArchiveIndex,
    formats: &HashMap<&str, &FormatRow>,
    exceptions: &mut Exceptions,
) -> Result<()> {
    for d in &index.dcx {
        if let Some(e) = &d.error {
            rep.add(Severity::Error, "dcx.unreadable", "_archives", &d.file, "", e);
        } else if d.trailing_bytes > 0 {
            rep.add(Severity::Warning, "dcx.trailing_bytes", "_archives", &d.file, "", format!("{} bytes after the last chunk, ignored", d.trailing_bytes));
        }
    }

    for g in load_archive_groups(paths)? {
        let sheet = format!("archives/{}", g.group);
        let (mut rows, mut checked) = (0, 0);
        for b in &g.binders {
            let mut binder_ok = true;
            if let Some(e) = &b.error {
                binder_ok = false;
                rep.add(Severity::Error, "binder.unreadable", &sheet, &b.path, "", e);
            }
            if let Some(at) = b.unexplained_byte {
                match exceptions.excuse(&b.path, "binder.unexplained_byte") {
                    Some(why) => rep.add(Severity::Warning, "binder.unexplained_byte", &sheet, &b.path, "", format!("byte {at:#x}: {why}")),
                    None => {
                        binder_ok = false;
                        rep.add(Severity::Error, "binder.unexplained_byte", &sheet, &b.path, "", format!("byte {at:#x} is not header, table, name or entry data"));
                    }
                }
            }
            let mut names: HashMap<&str, usize> = HashMap::new();
            for e in &b.entries {
                rows += 1;
                let mut ok = binder_ok;
                let row = format!("{} #{}", b.path, e.index);
                match &e.name {
                    Some(n) => *names.entry(n).or_default() += 1,
                    None => {
                        ok = false;
                        rep.add(Severity::Error, "entry.unnamed", &sheet, &row, "name", "binder entry has no name");
                    }
                }
                if let Some(err) = &e.error {
                    ok = false;
                    rep.add(Severity::Error, "entry.unreadable", &sheet, &row, "", err);
                }
                if e.dcx_trailing_bytes > 0 {
                    rep.add(Severity::Warning, "dcx.trailing_bytes", &sheet, &row, "", format!("inner DCX has {} bytes after the last chunk, ignored", e.dcx_trailing_bytes));
                }
                if !formats.contains_key(e.ext.as_str()) {
                    ok = false;
                }
                if ok {
                    checked += ENTRY_COLUMNS;
                }
            }
            let repeated: Vec<&str> = names.iter().filter(|(_, &n)| n > 1).map(|(k, _)| *k).collect();
            if !repeated.is_empty() {
                rep.add(Severity::Warning, "entry.duplicate_name", &sheet, &b.path, "name", format!("{} names repeat (addressed as #index): {}", repeated.len(), repeated.join(", ")));
            }
        }
        marks.push(Checkmark {
            kind: "archive",
            group: g.group.clone(),
            param_type: "BND3 entries".into(),
            instances: g.binders.len(),
            rows,
            columns: ENTRY_COLUMNS,
            intersections: rows * ENTRY_COLUMNS,
            checked,
            unchecked: rows * ENTRY_COLUMNS - checked,
        });
    }

    Ok(())
}

fn write_outputs(paths: &Paths, rep: &Report, marks: &[Checkmark]) -> Result<Summary> {
    let dir = paths.preflight();
    std::fs::create_dir_all(&dir)?;
    let mut w = csv::Writer::from_path(dir.join("issues.csv"))?;
    for i in &rep.issues {
        w.serialize(i)?;
    }
    w.flush()?;
    let mut w = csv::Writer::from_path(dir.join("checkmarks.csv"))?;
    for m in marks {
        w.serialize(m)?;
    }
    w.flush()?;

    let count = |s: Severity| rep.issues.iter().filter(|i| i.severity == s).count();
    let (errors, warnings, pending) = (count(Severity::Error), count(Severity::Warning), count(Severity::Pending));
    let tally = |kind: &str| -> (usize, usize) {
        let of = marks.iter().filter(|m| m.kind == kind);
        (of.clone().map(|m| m.checked).sum(), of.map(|m| m.intersections).sum())
    };
    let (checked, total) = tally("param");
    let (a_checked, a_total) = tally("archive");
    let (t_checked, t_total) = tally("texture");
    let (m_checked, m_total) = tally("model");
    let (p_checked, p_total) = tally("ac_part");
    let (s_checked, s_total) = tally("assembly");
    let (c_checked, c_total) = tally("motion");
    let (u_checked, u_total) = tally("tuning");
    let (k_checked, k_total) = tally("calc");

    let mut by_check: BTreeMap<(Severity, &str), Vec<&Issue>> = BTreeMap::new();
    for i in &rep.issues {
        by_check.entry((i.severity, i.check)).or_default().push(i);
    }

    let mut md = String::new();
    writeln!(md, "# Preflight\n")?;
    writeln!(md, "- errors: {errors}\n- warnings: {warnings}\n- pending (unimplemented): {pending}")?;
    writeln!(md, "- param intersections checked: {checked} / {total}")?;
    writeln!(md, "- archive entry intersections checked: {a_checked} / {a_total}")?;
    writeln!(md, "- texture intersections checked: {t_checked} / {t_total}")?;
    writeln!(md, "- model face set intersections checked: {m_checked} / {m_total}")?;
    writeln!(md, "- AC part record intersections checked: {p_checked} / {p_total}")?;
    writeln!(md, "- assembly row x slot intersections checked: {s_checked} / {s_total}")?;
    writeln!(md, "- motion clip intersections checked: {c_checked} / {c_total}")?;
    writeln!(md, "- tuning field intersections checked: {u_checked} / {u_total}")?;
    writeln!(md, "- AC control calc intersections checked: {k_checked} / {k_total}\n")?;
    for ((sev, check), list) in &by_check {
        writeln!(md, "## {sev:?} `{check}` ({})\n", list.len())?;
        for i in list.iter().take(40) {
            writeln!(md, "- {} | {} | {} | {}", i.sheet, i.row, i.column, i.detail)?;
        }
        if list.len() > 40 {
            writeln!(md, "- ... {} more in issues.csv", list.len() - 40)?;
        }
        writeln!(md)?;
    }
    std::fs::write(dir.join("report.md"), &md)?;

    println!(
        "preflight: {errors} errors, {warnings} warnings, {pending} pending; {checked}/{total} param, {a_checked}/{a_total} archive, {t_checked}/{t_total} texture, {m_checked}/{m_total} model, {p_checked}/{p_total} AC part, {s_checked}/{s_total} assembly, {c_checked}/{c_total} motion, {u_checked}/{u_total} tuning, {k_checked}/{k_total} calc intersections checked"
    );
    for ((sev, check), list) in &by_check {
        println!("  {sev:?} {check}: {}", list.len());
    }
    println!("preflight: report at {}", dir.join("report.md").display());
    Ok(Summary { errors })
}

#[cfg(test)]
mod tests {
    use super::ps3_citation;

    #[test]
    fn flags_ps3_code_and_runtime_citations() {
        assert_eq!(ps3_citation("FUN_00a7f5d4 struct[2]"), Some("FUN_00a7f5d4"));
        assert_eq!(ps3_citation("stub FUN_01a5ad14"), Some("FUN_01a5ad14"));
        assert_eq!(ps3_citation("an RPCS3 breakpoint"), Some("RPCS3"));
        assert_eq!(ps3_citation("TOC 0x1dcc8c0 -0xbfc"), Some("TOC"));
        assert_eq!(ps3_citation("the 01.02 update"), Some("01.02"));
        assert_eq!(ps3_citation("360 FUN_82899280 and 0x8285ff28"), None);
        assert_eq!(ps3_citation("version 1.01.02, TOCTOU"), None);
    }
}
